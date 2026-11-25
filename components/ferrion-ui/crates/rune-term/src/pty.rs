use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::screen::{MAX_COLS, MAX_ROWS};

pub const READ_BUF_BYTES: usize = 8192;
pub const MAX_PENDING_CHUNKS: usize = 256;
pub const MAX_WRITE_BYTES: usize = 65_536;
pub const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
pub const POLL_SLICE_MS: i32 = 50;
pub const MAX_WRITE_RETRIES: usize = 1024;
pub const MAX_WAIT_POLLS: usize = 400;
pub const WAIT_POLL_SLICE: Duration = Duration::from_millis(10);

#[derive(Debug)]
pub enum PtyError {
    InvalidSize { cols: u16, rows: u16 },
    OpenFailed(io::Error),
    PipeFailed(io::Error),
    FcntlFailed(io::Error),
    SpawnFailed(io::Error),
    ResizeFailed(io::Error),
    WriteFailed(io::Error),
    WriteTooLarge { len: usize, max: usize },
    SignalFailed(io::Error),
    WaitFailed(io::Error),
    ShutdownTimeout,
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSize { cols, rows } => write!(f, "invalid pty size {cols}x{rows}"),
            Self::OpenFailed(e) => write!(f, "openpty failed: {e}"),
            Self::PipeFailed(e) => write!(f, "wake pipe failed: {e}"),
            Self::FcntlFailed(e) => write!(f, "fcntl failed: {e}"),
            Self::SpawnFailed(e) => write!(f, "spawn failed: {e}"),
            Self::ResizeFailed(e) => write!(f, "TIOCSWINSZ failed: {e}"),
            Self::WriteFailed(e) => write!(f, "pty write failed: {e}"),
            Self::WriteTooLarge { len, max } => write!(f, "pty write {len} exceeds bound {max}"),
            Self::SignalFailed(e) => write!(f, "kill failed: {e}"),
            Self::WaitFailed(e) => write!(f, "wait failed: {e}"),
            Self::ShutdownTimeout => write!(f, "pty reader thread join timed out"),
        }
    }
}

impl std::error::Error for PtyError {}

#[derive(Clone, Debug)]
pub struct PtyConfig {
    pub program: String,
    pub args: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub term: String,
}

impl PtyConfig {
    pub fn shell(program: impl Into<String>, cols: u16, rows: u16) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cols,
            rows,
            term: "xterm-256color".to_owned(),
        }
    }
}

pub struct Pty {
    master: OwnedFd,
    wake_write: OwnedFd,
    child: Child,
    rx: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    done: Arc<(Mutex<bool>, Condvar)>,
    reader: Option<JoinHandle<()>>,
    eof: Arc<AtomicBool>,
    closed: bool,
}

impl Pty {
    pub fn spawn(cfg: &PtyConfig) -> Result<Self, PtyError> {
        if !(1..=MAX_COLS).contains(&cfg.cols) || !(1..=MAX_ROWS).contains(&cfg.rows) {
            return Err(PtyError::InvalidSize {
                cols: cfg.cols,
                rows: cfg.rows,
            });
        }
        let (master_raw, slave_raw) = open_pty_raw(cfg.cols, cfg.rows)?;
        let master = own_fd(master_raw);
        let slave = own_fd(slave_raw);
        set_fd_cloexec(master.as_raw_fd())?;
        set_fd_cloexec(slave.as_raw_fd())?;

        let (wake_r_raw, wake_w_raw) = create_pipe()?;
        let wake_read = own_fd(wake_r_raw);
        let wake_write = own_fd(wake_w_raw);
        set_fd_cloexec(wake_read.as_raw_fd())?;
        set_fd_cloexec(wake_write.as_raw_fd())?;
        set_fd_nonblock(wake_read.as_raw_fd())?;
        set_fd_nonblock(wake_write.as_raw_fd())?;

        let stdin_fd = slave.try_clone().map_err(PtyError::OpenFailed)?;
        let stdout_fd = slave.try_clone().map_err(PtyError::OpenFailed)?;
        let stderr_fd = slave.try_clone().map_err(PtyError::OpenFailed)?;
        let mut cmd = Command::new(&cfg.program);
        cmd.args(&cfg.args)
            .env("TERM", &cfg.term)
            .stdin(Stdio::from(stdin_fd))
            .stdout(Stdio::from(stdout_fd))
            .stderr(Stdio::from(stderr_fd));
        attach_pre_exec(&mut cmd);
        let child = cmd.spawn().map_err(PtyError::SpawnFailed)?;
        drop(slave);

        let reader_master = master.try_clone().map_err(PtyError::OpenFailed)?;
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(MAX_PENDING_CHUNKS);
        let stop = Arc::new(AtomicBool::new(false));
        let eof = Arc::new(AtomicBool::new(false));
        let done = Arc::new((Mutex::new(false), Condvar::new()));

        let stop_t = Arc::clone(&stop);
        let eof_t = Arc::clone(&eof);
        let done_t = Arc::clone(&done);
        let reader = thread::Builder::new()
            .name("rune-term-pty".to_owned())
            .spawn(move || {
                reader_loop(reader_master, wake_read, tx, stop_t, eof_t);
                let (lock, cv) = &*done_t;
                let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                *guard = true;
                cv.notify_all();
            })
            .map_err(PtyError::SpawnFailed)?;

        Ok(Self {
            master,
            wake_write,
            child,
            rx,
            stop,
            done,
            reader: Some(reader),
            eof,
            closed: false,
        })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn is_eof(&self) -> bool {
        self.eof.load(Ordering::Acquire)
    }

    pub fn try_read_chunks(&self, max_chunks: usize) -> Vec<Vec<u8>> {
        let limit = max_chunks.min(MAX_PENDING_CHUNKS);
        let mut out = Vec::new();
        for _ in 0..limit {
            match self.rx.try_recv() {
                Ok(chunk) => out.push(chunk),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    pub fn write_all(&mut self, bytes: &[u8]) -> Result<(), PtyError> {
        if bytes.len() > MAX_WRITE_BYTES {
            return Err(PtyError::WriteTooLarge {
                len: bytes.len(),
                max: MAX_WRITE_BYTES,
            });
        }
        let mut written = 0usize;
        for _ in 0..MAX_WRITE_RETRIES {
            if written >= bytes.len() {
                return Ok(());
            }
            match write_fd(self.master.as_raw_fd(), &bytes[written..]) {
                Ok(0) => {
                    return Err(PtyError::WriteFailed(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "zero-byte pty write",
                    )));
                }
                Ok(n) => written += n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(PtyError::WriteFailed(err)),
            }
        }
        Err(PtyError::WriteFailed(io::Error::new(
            io::ErrorKind::TimedOut,
            "pty write retry bound exceeded",
        )))
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), PtyError> {
        if !(1..=MAX_COLS).contains(&cols) || !(1..=MAX_ROWS).contains(&rows) {
            return Err(PtyError::InvalidSize { cols, rows });
        }
        ioctl_set_winsize(self.master.as_raw_fd(), cols, rows)
    }

    pub fn send_signal(&self, sig: i32) -> Result<(), PtyError> {
        let pid = self.child.id() as libc::pid_t;
        if pid <= 0 {
            return Err(PtyError::SignalFailed(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid child pid",
            )));
        }
        match kill_pid(-pid, sig) {
            Ok(()) => Ok(()),
            Err(err) if err.raw_os_error() == Some(libc::ESRCH) => {
                kill_pid(pid, sig).map_err(PtyError::SignalFailed)
            }
            Err(err) => Err(PtyError::SignalFailed(err)),
        }
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, PtyError> {
        self.child.try_wait().map_err(PtyError::WaitFailed)
    }

    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<ExitStatus>, PtyError> {
        let start = rune::time::now();
        for _ in 0..MAX_WAIT_POLLS {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            thread::sleep(WAIT_POLL_SLICE);
        }
        self.try_wait()
    }

    pub fn shutdown(&mut self) -> Result<Option<ExitStatus>, PtyError> {
        if self.closed {
            return self.try_wait();
        }
        self.closed = true;
        self.stop.store(true, Ordering::Release);
        write_fd(self.wake_write.as_raw_fd(), &[1u8]).ok();

        let (lock, cv) = &*self.done;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = cv
            .wait_timeout_while(guard, SHUTDOWN_JOIN_TIMEOUT, |done| !*done)
            .unwrap_or_else(|e| e.into_inner());
        let finished = *guard;
        drop(guard);

        if finished {
            if let Some(handle) = self.reader.take() {
                handle.join().ok();
            }
        } else {
            return Err(PtyError::ShutdownTimeout);
        }

        if let Some(status) = self.try_wait()? {
            return Ok(Some(status));
        }
        self.send_signal(libc::SIGTERM).ok();
        if let Some(status) = self.wait_timeout(Duration::from_millis(500))? {
            return Ok(Some(status));
        }
        self.child.kill().ok();
        let status = self.child.wait().map_err(PtyError::WaitFailed)?;
        Ok(Some(status))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.shutdown().ok();
    }
}

fn reader_loop(
    master: OwnedFd,
    wake_read: OwnedFd,
    tx: mpsc::SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    eof: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; READ_BUF_BYTES];
    let mut drain = [0u8; 64];
    while !stop.load(Ordering::Acquire) {
        let mut pfds = [
            libc::pollfd {
                fd: master.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        match poll_two_fds(&mut pfds, POLL_SLICE_MS) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if (pfds[1].revents & libc::POLLIN) != 0 {
            read_fd(wake_read.as_raw_fd(), &mut drain).ok();
            if stop.load(Ordering::Acquire) {
                break;
            }
        }
        if (pfds[0].revents & libc::POLLIN) != 0 {
            match read_fd(master.as_raw_fd(), &mut buf) {
                Ok(0) => {
                    eof.store(true, Ordering::Release);
                    break;
                }
                Ok(n) => {
                    let mut chunk = buf[..n].to_vec();
                    loop {
                        match tx.try_send(chunk) {
                            Ok(()) => break,
                            Err(TrySendError::Full(returned)) => {
                                if stop.load(Ordering::Acquire) {
                                    return;
                                }
                                chunk = returned;
                                thread::sleep(Duration::from_millis(2));
                            }
                            Err(TrySendError::Disconnected(_)) => return,
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    eof.store(true, Ordering::Release);
                    break;
                }
            }
        } else if (pfds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
            eof.store(true, Ordering::Release);
            break;
        }
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn open_pty_raw(cols: u16, rows: u16) -> Result<(RawFd, RawFd), PtyError> {
    assert!((1..=MAX_COLS).contains(&cols));
    assert!((1..=MAX_ROWS).contains(&rows));
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &ws,
        )
    };
    if rc != 0 || master < 0 || slave < 0 {
        return Err(PtyError::OpenFailed(io::Error::last_os_error()));
    }
    Ok((master, slave))
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn own_fd(raw: RawFd) -> OwnedFd {
    assert!(raw >= 0);
    unsafe { OwnedFd::from_raw_fd(raw) }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn create_pipe() -> Result<(RawFd, RawFd), PtyError> {
    let mut fds: [libc::c_int; 2] = [-1, -1];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc != 0 || fds[0] < 0 || fds[1] < 0 {
        return Err(PtyError::PipeFailed(io::Error::last_os_error()));
    }
    Ok((fds[0], fds[1]))
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_fd_cloexec(fd: RawFd) -> Result<(), PtyError> {
    assert!(fd >= 0);
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    if rc < 0 {
        return Err(PtyError::FcntlFailed(io::Error::last_os_error()));
    }
    Ok(())
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn get_fd_flags(fd: RawFd) -> Result<libc::c_int, PtyError> {
    assert!(fd >= 0);
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(PtyError::FcntlFailed(io::Error::last_os_error()));
    }
    Ok(flags)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_fd_flags(fd: RawFd, flags: libc::c_int) -> Result<(), PtyError> {
    assert!(fd >= 0);
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    if rc < 0 {
        return Err(PtyError::FcntlFailed(io::Error::last_os_error()));
    }
    Ok(())
}

fn set_fd_nonblock(fd: RawFd) -> Result<(), PtyError> {
    let flags = get_fd_flags(fd)?;
    set_fd_flags(fd, flags | libc::O_NONBLOCK)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn ioctl_get_winsize(fd: RawFd) -> Result<(u16, u16), PtyError> {
    assert!(fd >= 0);
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if rc != 0 {
        return Err(PtyError::ResizeFailed(io::Error::last_os_error()));
    }
    Ok((ws.ws_row, ws.ws_col))
}

pub fn read_stdin_winsize() -> Result<(u16, u16), PtyError> {
    ioctl_get_winsize(0)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn ioctl_set_winsize(fd: RawFd, cols: u16, rows: u16) -> Result<(), PtyError> {
    assert!(fd >= 0);
    assert!((1..=MAX_COLS).contains(&cols));
    assert!((1..=MAX_ROWS).contains(&rows));
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let rc = unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
    if rc != 0 {
        return Err(PtyError::ResizeFailed(io::Error::last_os_error()));
    }
    Ok(())
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn setsid_in_child() -> io::Result<()> {
    let rc = unsafe { libc::setsid() };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_ctty_stdin() -> io::Result<()> {
    let rc = unsafe { libc::ioctl(0, libc::TIOCSCTTY, 0 as libc::c_int) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn attach_pre_exec(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            setsid_in_child()?;
            set_ctty_stdin()?;
            Ok(())
        });
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn poll_two_fds(pfds: &mut [libc::pollfd; 2], timeout_ms: i32) -> io::Result<i32> {
    assert!(pfds[0].fd >= 0);
    assert!(pfds[1].fd >= 0);
    let rc = unsafe { libc::poll(pfds.as_mut_ptr(), 2, timeout_ms) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn read_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    assert!(fd >= 0);
    assert!(!buf.is_empty());
    let rc = unsafe { libc::read(fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc as usize)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn write_fd(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    assert!(fd >= 0);
    assert!(!buf.is_empty());
    let rc = unsafe { libc::write(fd, buf.as_ptr().cast::<libc::c_void>(), buf.len()) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(rc as usize)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn kill_pid(pid: libc::pid_t, sig: i32) -> io::Result<()> {
    assert!(pid != 0);
    let rc = unsafe { libc::kill(pid, sig) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(dead_code)]
type UnusedChildHandles = (Option<ChildStdin>, Option<ChildStdout>, Option<ChildStderr>);
