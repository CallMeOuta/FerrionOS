use libc::{self, winsize};
use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::{io::AsRawFd, io::RawFd, process::CommandExt};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::{fs::File, mem, ptr, time::Duration};

use crate::{Error, Result, wrap_io_err};

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

    let pipes = (pair.slave, log_writer);

    (pty_reader, log_reader, pty_writer, pipes)
}

pub fn flush_pty(logger: &mut PtyOut) {
    let Some((pty, file)) = logger else {
        return;
    };
    let _ = pty.flush();
    std::thread::sleep(Duration::from_millis(10));
    let _ = file.flush();
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
    };

    cloexec(master.fd.as_raw_fd())?;
    cloexec(slave.fd.as_raw_fd())?;

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

        let mut child = cmd.spawn().map_err(wrap_io_err!("Spawning cmd"))?;

        if stdin {
            child.stdin.take();
        }
        child.stdout.take();
        child.stderr.take();

        Ok(child)
    }

    fn flush(&mut self) -> Result<()> {
        self.0.flush().map_err(wrap_io_err!("Flushing pty"))
    }
}

pub struct UnixMasterPty {
    fd: PtyFd,
}

pub struct UnixSlavePty {
    fd: PtyFd,
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

impl UnixSlavePty {
    fn spawn_command(&self, builder: &mut Command) -> Result<std::process::Child> {
        self.fd.spawn_command(builder, false)
    }
    fn spawn_command_stdin(&self, builder: &mut Command) -> Result<std::process::Child> {
        self.fd.spawn_command(builder, true)
    }
    fn flush(&mut self) -> Result<()> {
        self.fd.flush()
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
        Ok(Box::new(fd))
    }

    pub fn try_clone_writer(&self) -> Result<Box<dyn Write + Send>> {
        let fd = PtyFd(
            self.fd
                .try_clone()
                .map_err(wrap_io_err!("Cloning pty fd"))?,
        );
        Ok(Box::new(fd))
    }
}
