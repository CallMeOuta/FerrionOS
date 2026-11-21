//! A library for creating and managing daemons for RedoxOS.
#![feature(never_type)]

use std::io::{self, PipeWriter, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

use libredox::Fd;
use libredox::protocol::{NsDup, NsPermissions};
use redox_scheme::Socket;
use redox_scheme::scheme::{SchemeAsync, SchemeSync};

unsafe fn get_fd(var: &str) -> RawFd {
    let fd: RawFd = std::env::var(var)
        .unwrap_or_else(|_| {
            panic!(
                "Daemons can't be started manually. \
            Add a service config to make init start this daemon instead."
            )
        })
        .parse()
        .unwrap();
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        panic!(
            "daemon: failed to set CLOEXEC flag for {var} fd: {}",
            io::Error::last_os_error()
        );
    }
    fd
}

unsafe fn pass_fd(cmd: &mut Command, env: &str, fd: RawFd) {
    cmd.env(env, format!("{}", fd));
    unsafe {
        cmd.pre_exec(move || {
            // Pass notify pipe to child
            if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

/// A long running background process that handles requests.
#[must_use = "Daemon::ready must be called"]
pub struct Daemon {
    write_pipe: PipeWriter,
}

impl Daemon {
    /// Create a new daemon.
    #[expect(clippy::new_ret_no_self, reason = "Daemon is not intented to return")]
    pub fn new(f: impl FnOnce(Self) -> !) -> ! {
        let write_pipe = unsafe { io::PipeWriter::from_raw_fd(get_fd("INIT_NOTIFY")) };

        f(Daemon { write_pipe })
    }

    /// Notify the process that the daemon is ready to accept requests.
    pub fn ready(mut self) {
        self.write_pipe.write_all(&[0]).unwrap();
    }

    /// Executes `Command` as a child process.
    // FIXME remove once the service spawning of hwd and pcid-spawner is moved to init
    #[deprecated]
    pub fn spawn(mut cmd: Command) {
        let (mut read_pipe, write_pipe) = io::pipe().unwrap();

        unsafe { pass_fd(&mut cmd, "INIT_NOTIFY", write_pipe.as_raw_fd()) };

        if let Err(err) = cmd.spawn() {
            eprintln!("daemon: failed to execute {cmd:?}: {err}");
            return;
        }

        drop(write_pipe);
        let mut data = [0];
        match read_pipe.read_exact(&mut data) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                eprintln!("daemon: {cmd:?} exited without notifying readiness");
            }
            Err(err) => {
                eprintln!("daemon: failed to wait for {cmd:?}: {err}");
            }
        }
    }
}

/// A long running background process that handles requests using schemes.
#[must_use = "SchemeDaemon::ready must be called"]
pub struct SchemeDaemon {
    write_pipe: PipeWriter,
}

impl SchemeDaemon {
    /// Create a new daemon for use with schemes.
    #[expect(clippy::new_ret_no_self)]
    pub fn new(f: impl FnOnce(SchemeDaemon) -> !) -> ! {
        let write_pipe = unsafe { io::PipeWriter::from_raw_fd(get_fd("INIT_NOTIFY")) };

        f(SchemeDaemon { write_pipe })
    }

    /// Notify the process that the scheme daemon is ready to accept requests.
    pub fn ready_with_fd(self, cap_fd: Fd) -> syscall::Result<()> {
        libredox::call::call_wo(
            self.write_pipe.as_raw_fd() as usize,
            &cap_fd.into_raw().to_ne_bytes(),
            syscall::CallFlags::FD,
            &[],
        )?;
        Ok(())
    }

    /// Notify the process that the synchronous scheme daemon is ready to accept requests.
    pub fn ready_sync_scheme<S: SchemeSync>(
        self,
        socket: &Socket,
        scheme: &mut S,
    ) -> syscall::Result<()> {
        let cap_id = scheme.scheme_root()?;
        let cap_fd = socket.create_this_scheme_fd(0, cap_id, 0, 0)?;
        self.ready_with_fd(Fd::new(cap_fd))
    }

    /// Notify the process that the asynchronous scheme daemon is ready to accept requests.
    pub fn ready_async_scheme<S: SchemeAsync>(
        self,
        socket: &Socket,
        scheme: &mut S,
    ) -> syscall::Result<()> {
        let cap_id = scheme.scheme_root()?;
        let cap_fd = socket.create_this_scheme_fd(0, cap_id, 0, 0)?;
        self.ready_with_fd(Fd::new(cap_fd))
    }
}

/// Makes `cmd` start its program without the hardware right (FerrionOS lockdown, S13).
///
/// The program keeps the caller's namespace, seen through a handle that hides the schemes
/// reaching hardware (`irq`, `serio`, `kernel.acpi`, `pci`, `acpi`) and whose `memory` allocates
/// ordinary memory only, so it can map no physical memory and use no I/O ports. init starts every
/// service this way unless its unit sets `hardware`; a driver starts its helpers this way.
pub fn without_hardware(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(drop_hardware_right);
    }
}

fn drop_hardware_right() -> io::Result<()> {
    let ns = libredox::call::getns()?;
    let mut request = [0_u8; 2 * size_of::<usize>()];
    let (kind, permissions) = request.split_at_mut(size_of::<usize>());
    kind.copy_from_slice(&(NsDup::ShrinkPermissions as usize).to_le_bytes());
    // Every permission libredox defines; the namespace manager's hardware bit is not among them.
    permissions.copy_from_slice(&NsPermissions::all().bits().to_le_bytes());
    let restricted = libredox::call::dup(ns, request)?;
    // `setns` hands back the handle it replaced; close it so the program cannot inherit it.
    let previous = libredox::call::setns(restricted)?;
    libredox::call::close(previous)?;
    Ok(())
}
