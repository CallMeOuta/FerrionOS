#[cfg(target_os = "redox")]
fn main() {
    if let Err(error) = daemon::run() {
        eprintln!("ferrion-scoped: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "redox"))]
fn main() {
    eprintln!("ferrion-scoped: runs on FerrionOS only");
    std::process::exit(2);
}

#[cfg(target_os = "redox")]
mod daemon {
    use std::collections::BTreeMap;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use redox_scheme::scheme::{SchemeState, SchemeSync, register_sync_scheme};
    use redox_scheme::{CallerCtx, OpenResult, RequestKind, Response, SignalBehavior, Socket};
    use syscall::error::{EACCES, EINVAL, EIO, EMFILE, ENOENT, EOPNOTSUPP, Error, Result};
    use syscall::flag::CallFlags;

    const PROXY: &str = "/usr/bin/ferrion-scope-proxy";
    const MAX_PROXIES: usize = 64;
    const ROOT: usize = 0;
    const SHRINK_PERMISSIONS: usize = 1;
    const LIST_INSERT_DELETE_CREATE: usize = 0b1111;

    struct Proxy {
        child: Child,
        control: libredox::Fd,
    }

    struct Daemon {
        proxies: BTreeMap<u32, Proxy>,
    }

    fn drop_hardware() -> Result<()> {
        let mut buf = [0u8; 16];
        buf[..8].copy_from_slice(&SHRINK_PERMISSIONS.to_ne_bytes());
        buf[8..].copy_from_slice(&LIST_INSERT_DELETE_CREATE.to_ne_bytes());
        let ns = libredox::call::getns()?;
        let shrunk = libredox::call::dup(ns, buf)?;
        let old = libredox::call::setns(shrunk)?;
        let _ = libredox::call::close(old);
        Ok(())
    }

    fn spawn(uid: u32, gid: u32) -> Result<Proxy> {
        let mut pipe = [0i32; 2];
        if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(Error::new(EIO));
        }
        let (ready, notify) = (
            libredox::Fd::new(pipe[0] as usize),
            libredox::Fd::new(pipe[1] as usize),
        );
        let notify_raw = notify.raw() as i32;
        let mut command = Command::new(PROXY);
        command
            .arg(uid.to_string())
            .arg(gid.to_string())
            .arg(notify_raw.to_string())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(notify_raw, libc::F_SETFD, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let spawned = command.spawn();
        drop(notify);
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                eprintln!("ferrion-scoped: cannot start {PROXY} for uid {uid}: {e}");
                return Err(Error::new(EIO));
            }
        };
        let mut control = usize::MAX;
        let got = libredox::call::call_ro(
            ready.raw(),
            unsafe {
                std::slice::from_raw_parts_mut(
                    (&mut control as *mut usize).cast::<u8>(),
                    size_of::<usize>(),
                )
            },
            CallFlags::FD | CallFlags::FD_UPPER | CallFlags::FD_CLOEXEC,
            &[],
        );
        drop(ready);
        match got {
            Ok(1) if control != usize::MAX => Ok(Proxy {
                child,
                control: libredox::Fd::new(control),
            }),
            other => {
                eprintln!(
                    "ferrion-scoped: {PROXY} for uid {uid} did not hand over its scheme: {other:?}"
                );
                let _ = child.kill();
                let _ = child.wait();
                Err(Error::new(EIO))
            }
        }
    }

    impl SchemeSync for Daemon {
        fn scheme_root(&mut self) -> Result<usize> {
            Ok(ROOT)
        }

        fn openat(
            &mut self,
            dirfd: usize,
            path: &str,
            _flags: usize,
            _fcntl_flags: u32,
            ctx: &CallerCtx,
        ) -> Result<OpenResult> {
            if dirfd != ROOT {
                return Err(Error::new(EACCES));
            }
            if path.trim_matches('/') != "new" {
                return Err(Error::new(ENOENT));
            }
            if let Some(proxy) = self.proxies.get_mut(&ctx.uid)
                && !matches!(proxy.child.try_wait(), Ok(None))
            {
                self.proxies.remove(&ctx.uid);
            }
            if !self.proxies.contains_key(&ctx.uid) {
                if self.proxies.len() >= MAX_PROXIES {
                    return Err(Error::new(EMFILE));
                }
                let proxy = spawn(ctx.uid, ctx.gid)?;
                self.proxies.insert(ctx.uid, proxy);
            }
            let proxy = self.proxies.get(&ctx.uid).ok_or(Error::new(EIO))?;
            let fd = proxy.control.dup(&[])?;
            Ok(OpenResult::OtherScheme { fd })
        }

        fn dup(&mut self, _old_id: usize, _buf: &[u8], _ctx: &CallerCtx) -> Result<OpenResult> {
            Err(Error::new(EINVAL))
        }
    }

    pub fn run() -> Result<()> {
        let notify: usize = std::env::var("INIT_NOTIFY")
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(Error::new(EINVAL))?;
        let socket = Socket::create()?;
        let mut daemon = Daemon {
            proxies: BTreeMap::new(),
        };
        let mut state = SchemeState::new();
        register_sync_scheme(&socket, "file-scope", &mut daemon)?;
        drop_hardware()?;
        libredox::call::write(notify, &[0])?;
        let _ = libredox::call::close(notify);
        loop {
            let Some(request) = socket.next_request(SignalBehavior::Restart)? else {
                return Ok(());
            };
            match request.kind() {
                RequestKind::Call(call) => {
                    let response = call.handle_sync(&mut daemon, &mut state);
                    socket.write_response(response, SignalBehavior::Restart)?;
                }
                RequestKind::SendFd(request) => {
                    socket.write_response(
                        Response::err(EOPNOTSUPP, request),
                        SignalBehavior::Restart,
                    )?;
                }
                _ => {}
            }
        }
    }
}
