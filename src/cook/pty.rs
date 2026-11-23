use libc::{self, winsize};
use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::{io::AsRawFd, io::RawFd, process::CommandExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::{fs::File, mem, ptr, time::Duration};

use crate::{Error, Result, wrap_io_err};

const MAX_ACTIVE_JOBS: usize = 256;
const MAX_SLAVE_PGIDS: usize = 64;
const MAX_REAP_POLLS: usize = 100;
const REAP_POLL_INTERVAL: Duration = Duration::from_millis(5);
const GRACEFUL_TERM_POLLS: usize = 20;

static ACTIVE_PGIDS: [AtomicI32; MAX_ACTIVE_JOBS] = [const { AtomicI32::new(0) }; MAX_ACTIVE_JOBS];
static SIGNAL_HANDLERS_INSTALLED: AtomicBool = AtomicBool::new(false);

fn register_pgid(pgid: libc::pid_t) -> Result<()> {
    if pgid <= 0 {
        return Err(Error::Other(format!("invalid process group id {pgid}")));
    }
    for slot in &ACTIVE_PGIDS {
        if slot
            .compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Ok(());
        }
    }
    Err(Error::Other(format!(
        "active pty job table exceeded MAX_ACTIVE_JOBS ({MAX_ACTIVE_JOBS})"
    )))
}

pub fn unregister_pgid(pgid: libc::pid_t) {
    for slot in &ACTIVE_PGIDS {
        let _ = slot.compare_exchange(pgid, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

extern "C" fn terminate_active_jobs_on_signal(sig: libc::c_int) {
    for slot in &ACTIVE_PGIDS {
        let pgid = slot.load(Ordering::SeqCst);
        if pgid > 0 {
            unsafe {
                libc::kill(-pgid, libc::SIGTERM);
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

pub fn install_signal_handlers() {
    if SIGNAL_HANDLERS_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let mut sa: libc::sigaction = mem::zeroed();
        sa.sa_sigaction = terminate_active_jobs_on_signal as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = 0;
        libc::sigaction(libc::SIGINT, &sa, ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &sa, ptr::null_mut());
    }
}

fn pgid_alive(pgid: libc::pid_t) -> bool {
    let rc = unsafe { libc::kill(-pgid, 0) };
    if rc == 0 {
        return true;
    }
    let err = io::Error::last_os_error().raw_os_error();
    err != Some(libc::ESRCH)
}

fn reap_pgid_nonblocking(pgid: libc::pid_t) {
    let mut status: libc::c_int = 0;
    for _ in 0..MAX_REAP_POLLS {
        let pid = unsafe { libc::waitpid(-pgid, &mut status, libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
    }
}

fn kill_and_reap_pgid(pgid: libc::pid_t) -> Result<()> {
    if pgid <= 0 {
        return Ok(());
    }
    reap_pgid_nonblocking(pgid);
    if pgid_alive(pgid) {
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        for _ in 0..GRACEFUL_TERM_POLLS {
            reap_pgid_nonblocking(pgid);
            if !pgid_alive(pgid) {
                break;
            }
            std::thread::sleep(REAP_POLL_INTERVAL);
        }
    }
    if pgid_alive(pgid) {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
        for _ in 0..MAX_REAP_POLLS {
            reap_pgid_nonblocking(pgid);
            if !pgid_alive(pgid) {
                break;
            }
            std::thread::sleep(REAP_POLL_INTERVAL);
        }
    }
    unregister_pgid(pgid);
    if pgid_alive(pgid) {
        return Err(Error::Other(format!(
            "live descendants remain in process group {pgid}"
        )));
    }
    Ok(())
}

pub struct Job {
    child: Option<Child>,
    pgid: libc::pid_t,
}

impl Job {
    pub fn new(mut child: Child) -> Result<Self> {
        let pgid = child.id() as libc::pid_t;
        if let Err(err) = register_pgid(pgid) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
        Ok(Self {
            child: Some(child),
            pgid,
        })
    }

    pub fn pgid(&self) -> libc::pid_t {
        self.pgid
    }

    pub fn wait(&mut self) -> Result<ExitStatus> {
        let status = match self.child.as_mut() {
            Some(child) => child
                .wait()
                .map_err(|e| Error::from_io_error(e, "Waiting for job"))?,
            None => return Err(Error::Other("job already waited".to_string())),
        };
        let reap_res = kill_and_reap_pgid(self.pgid);
        self.child = None;
        reap_res?;
        Ok(status)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = kill_and_reap_pgid(self.pgid);
            let _ = child.wait();
        } else {
            unregister_pgid(self.pgid);
        }
    }
}

macro_rules! log_to_pty {
    ($logger:expr, $($arg:tt)+) => {
        if $logger.is_some() {
            use std::io::Write;
            let mut logfd = $logger.as_ref().unwrap().1.try_clone().unwrap();
            let _ = logfd.write(format!($($arg)+).as_bytes());
            let _ = logfd.write(&[b'\n']);
        } else {
            eprintln!($($arg)+);
        }
    };
}

pub(crate) use log_to_pty;

pub type PtyOut<'a> = Option<(&'a mut UnixSlavePty, &'a mut PipeWriter)>;

pub fn setup_pty() -> (
    Box<dyn Read + Send>,
    PipeReader,
    (UnixSlavePty, std::io::PipeWriter),
) {
    let pty_system = UnixPtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            ..Default::default()
        })
        .expect("Unable to open pty");

    let pty_reader = pair
        .master
        .try_clone_reader()
        .expect("Unable to clone pty reader");

    let (log_reader, log_writer) = std::io::pipe().expect("Failed to create log pipe");
    assert_cloexec(log_reader.as_raw_fd());
    assert_cloexec(log_writer.as_raw_fd());
    let pipes = (pair.slave, log_writer);
    (pty_reader, log_reader, pipes)
}

pub fn setup_pty_with_stdin() -> (
    Box<dyn Read + Send>,
    PipeReader,
    Box<dyn Write + Send>,
    (UnixSlavePty, PipeWriter),
) {
    let pty_system = UnixPtySystem::default();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("Unable to open pty");

    let pty_reader = pair
        .master
        .try_clone_reader()
        .expect("Unable to clone pty reader");

    let pty_writer = pair
        .master
        .try_clone_writer()
        .expect("Unable to clone pty writer");

    let (log_reader, log_writer) = std::io::pipe().expect("Failed to create log pipe");
    assert_cloexec(log_reader.as_raw_fd());
    assert_cloexec(log_writer.as_raw_fd());

    let pipes = (pair.slave, log_writer);

    (pty_reader, log_reader, pty_writer, pipes)
}

pub fn spawn_to_pipe(command: &mut Command, stdout_pipe: &PtyOut) -> Result<Child> {
    match stdout_pipe {
        Some(stdout) => stdout.0.spawn_command(command),
        None => command.spawn().map_err(wrap_io_err!(
            PathBuf::from(command.get_program()),
            "Spawning"
        )),
    }
}

pub fn spawn_to_pipe_with_stdin(command: &mut Command, stdout_pipe: &PtyOut) -> Result<Child> {
    match stdout_pipe {
        Some(stdout) => stdout.0.spawn_command_stdin(command),
        None => command.spawn().map_err(wrap_io_err!(
            PathBuf::from(command.get_program()),
            "Spawning"
        )),
    }
}

pub fn write_to_pty(pty: &PtyOut, text: &str) {
    log_to_pty!(pty, "{}", text);
}

//
// based on portable-pty crate
// copied here since it isn't flexible enough
//

#[derive(Default)]
pub struct UnixPtySystem {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl Default for PtySize {
    fn default() -> Self {
        PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

fn openpty(size: PtySize) -> Result<(UnixMasterPty, UnixSlavePty)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;

    let mut size = winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    };

    let result = unsafe {
        #[allow(clippy::unnecessary_mut_passed)]
        libc::openpty(
            &mut master,
            &mut slave,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut size,
        )
    };

    if result != 0 {
        return Err(Error::from_last_io_error("Opening openpty"));
    }

    let master = UnixMasterPty {
        fd: PtyFd(unsafe { File::from_raw_fd(master) }),
    };
    let slave = UnixSlavePty {
        fd: PtyFd(unsafe { File::from_raw_fd(slave) }),
        pgids: Mutex::new(Vec::new()),
    };

    cloexec(master.fd.as_raw_fd())?;
    cloexec(slave.fd.as_raw_fd())?;
    assert_cloexec(master.fd.as_raw_fd());
    assert_cloexec(slave.fd.as_raw_fd());

    Ok((master, slave))
}

pub struct PtyPair {
    pub slave: UnixSlavePty,
    pub master: UnixMasterPty,
}

impl UnixPtySystem {
    fn openpty(&self, size: PtySize) -> Result<PtyPair> {
        let (master, slave) = openpty(size)?;
        Ok(PtyPair { master, slave })
    }
}

struct PtyFd(pub File);
impl std::ops::Deref for PtyFd {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}

impl Read for PtyFd {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(ref e) if e.raw_os_error() == Some(libc::EIO) => Ok(0),
            x => x,
        }
    }
}

impl Write for PtyFd {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl PtyFd {
    fn resize(&self, size: PtySize) -> Result<()> {
        let ws_size = winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.pixel_width,
            ws_ypixel: size.pixel_height,
        };

        if unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &ws_size as *const _,
            )
        } != 0
        {
            return Err(Error::from_last_io_error("ioctl resize (TIOCSWINSZ)"));
        }

        Ok(())
    }

    fn get_size(&self) -> Result<PtySize> {
        let mut size: winsize = unsafe { mem::zeroed() };
        if unsafe {
            libc::ioctl(
                self.0.as_raw_fd(),
                libc::TIOCGWINSZ as _,
                &mut size as *mut _,
            )
        } != 0
        {
            return Err(Error::from_last_io_error("ioctl get size (TIOCGWINSZ)"));
        }
        Ok(PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
            pixel_width: size.ws_xpixel,
            pixel_height: size.ws_ypixel,
        })
    }

    fn spawn_command(&self, cmd: &mut Command, stdin: bool) -> Result<std::process::Child> {
        cmd.stdout(self.try_clone().map_err(wrap_io_err!("Cloning pty"))?)
            .stderr(self.try_clone().map_err(wrap_io_err!("Cloning pty"))?);

        if stdin {
            cmd.stdin(self.try_clone().map_err(wrap_io_err!("Cloning pty"))?);
        }

        unsafe {
            cmd.pre_exec(move || {
                for signo in &[
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                ] {
                    libc::signal(*signo, libc::SIG_DFL);
                }

                let empty_set: libc::sigset_t = std::mem::zeroed();
                libc::sigprocmask(libc::SIG_SETMASK, &empty_set, std::ptr::null_mut());

                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }

                Ok(())
            })
        };

        let spawned = cmd.spawn();
        cmd.stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit());
        if stdin {
            cmd.stdin(std::process::Stdio::inherit());
        }
        let mut child = spawned.map_err(wrap_io_err!("Spawning cmd"))?;
        if stdin {
            child.stdin.take();
        }
        child.stdout.take();
        child.stderr.take();

        Ok(child)
    }
}

pub struct UnixMasterPty {
    fd: PtyFd,
}

pub struct UnixSlavePty {
    fd: PtyFd,
    pgids: Mutex<Vec<libc::pid_t>>,
}

fn cloexec(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(Error::from_last_io_error("fcntl to read flags"));
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result == -1 {
        return Err(Error::from_last_io_error("fcntl to set CLOEXEC"));
    }
    Ok(())
}

fn assert_cloexec(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(
        flags >= 0 && (flags & libc::FD_CLOEXEC) != 0,
        "descriptor {fd} is missing FD_CLOEXEC"
    );
}

impl UnixSlavePty {
    fn track_child(&self, mut child: Child) -> Result<Child> {
        let mut pgids = self.pgids.lock().unwrap();
        pgids.retain(|&p| {
            if pgid_alive(p) {
                true
            } else {
                unregister_pgid(p);
                false
            }
        });
        if pgids.len() >= MAX_SLAVE_PGIDS {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Other(format!(
                "slave pty tracked process groups exceeded MAX_SLAVE_PGIDS ({MAX_SLAVE_PGIDS})"
            )));
        }
        let pgid = child.id() as libc::pid_t;
        if let Err(err) = register_pgid(pgid) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
        pgids.push(pgid);
        Ok(child)
    }

    fn spawn_command(&self, builder: &mut Command) -> Result<std::process::Child> {
        let child = self.fd.spawn_command(builder, false)?;
        self.track_child(child)
    }

    fn spawn_command_stdin(&self, builder: &mut Command) -> Result<std::process::Child> {
        let child = self.fd.spawn_command(builder, true)?;
        self.track_child(child)
    }

    pub fn reap_and_assert_no_descendants(&self) -> Result<()> {
        let pgids = std::mem::take(&mut *self.pgids.lock().unwrap());
        let mut first_err: Option<Error> = None;
        for pgid in pgids {
            if let Err(err) = kill_and_reap_pgid(pgid)
                && first_err.is_none()
            {
                first_err = Some(err);
            }
        }
        match first_err {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

impl UnixMasterPty {
    #[allow(unused)]
    fn resize(&self, size: PtySize) -> Result<()> {
        self.fd.resize(size)
    }

    #[allow(unused)]
    fn get_size(&self) -> Result<PtySize> {
        self.fd.get_size()
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>> {
        let fd = PtyFd(
            self.fd
                .try_clone()
                .map_err(wrap_io_err!("Cloning pty fd"))?,
        );
        assert_cloexec(fd.as_raw_fd());
        Ok(Box::new(fd))
    }

    pub fn try_clone_writer(&self) -> Result<Box<dyn Write + Send>> {
        let fd = PtyFd(
            self.fd
                .try_clone()
                .map_err(wrap_io_err!("Cloning pty fd"))?,
        );
        assert_cloexec(fd.as_raw_fd());
        Ok(Box::new(fd))
    }
}
