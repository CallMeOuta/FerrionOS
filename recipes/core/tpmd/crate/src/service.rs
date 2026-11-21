use libredox::call;
use libredox::flag::F_SETFD;
use redox_scheme::scheme::{SchemeState, SchemeSync};
use redox_scheme::{RequestKind, Response, SignalBehavior, Socket};
use syscall::error::{EOPNOTSUPP, Error, Result};

pub fn init_notify_fd() -> Option<usize> {
    std::env::var("INIT_NOTIFY").ok()?.parse().ok()
}

pub fn set_inherit(fd: usize, inherit: bool) -> Result<()> {
    let flags = if inherit {
        0
    } else {
        libredox::flag::O_CLOEXEC as usize
    };
    call::fcntl(fd, F_SETFD as usize, flags)?;
    Ok(())
}

pub fn ready(notify: usize, socket: &Socket, root: usize) -> Result<()> {
    let cap_fd = socket.create_this_scheme_fd(0, root, 0, 0)?;
    call::call_wo(notify, &cap_fd.to_ne_bytes(), syscall::CallFlags::FD, &[])?;
    call::close(notify)?;
    Ok(())
}

pub fn serve(socket: &Socket, scheme: &mut impl SchemeSync) -> Result<()> {
    let mut state = SchemeState::new();
    while let Some(request) = socket.next_request(SignalBehavior::Restart)? {
        let response = match request.kind() {
            RequestKind::Call(call) => call.handle_sync(scheme, &mut state),
            RequestKind::SendFd(req) => Response::new(Err(Error::new(EOPNOTSUPP)), req),
            RequestKind::RecvFd(req) => Response::open_dup_like(Err(Error::new(EOPNOTSUPP)), req),
            RequestKind::OnClose { id } => {
                scheme.on_close(id);
                continue;
            }
            _ => continue,
        };
        socket.write_response(response, SignalBehavior::Restart)?;
    }
    Ok(())
}
