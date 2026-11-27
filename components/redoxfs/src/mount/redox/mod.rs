use redox_scheme::{
    CallRequest, RequestKind, Response, SignalBehavior, Socket,
    scheme::{IntoTag, Op, SchemeResponse, SchemeState, SchemeSync},
};
use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;
use syscall::error::{EBADF, ENOSYS, EOPNOTSUPP, Error};
use syscall::flag::{O_STAT, StdFsCallKind};

use crate::{Disk, FileSystem, IS_UMT};

use self::lock::{Call, LockTable, Reply, read_flags};
use self::scheme::FileScheme;

pub mod lock;
pub mod resource;
pub mod scheme;

//FIXME: mut callback is not mut
#[allow(unused_mut)]

pub fn mount<D, P, T, F>(filesystem: FileSystem<D>, mountpoint: P, mut callback: F) -> io::Result<T>
where
    D: Disk,
    P: AsRef<Path>,
    F: FnOnce(&Path) -> T,
{
    let mountpoint = mountpoint.as_ref();
    let socket = Socket::create()?;

    let scheme_name = format!("{}", mountpoint.display());
    let mounted_path = format!("/scheme/{}", mountpoint.display());

    let mut state = SchemeState::new();
    let mut scheme = FileScheme::new(scheme_name, mounted_path.clone(), filesystem, &socket)?;
    let mut locks = LockTable::default();
    let mut replies = Vec::new();

    redox_scheme::scheme::register_sync_scheme(
        &socket,
        &format!("{}", mountpoint.display()),
        &mut scheme,
    )?;

    let res = callback(Path::new(&mounted_path));

    'serve: while IS_UMT.load(Ordering::SeqCst) == 0 {
        let Some(req) = socket.next_request(SignalBehavior::Restart)? else {
            break;
        };
        let response = match req.kind() {
            RequestKind::Call(r) => {
                handle_call(r, &mut scheme, &mut state, &mut locks, &mut replies)
            }
            RequestKind::SendFd(sendfd_request) => {
                let result = scheme.on_sendfd(&sendfd_request);
                Some(Response::new(result, sendfd_request))
            }
            RequestKind::Cancellation(cancellation) => {
                let alive = |pid| scheme.process_alive(pid);
                locks.cancel(cancellation.id, &alive, &mut replies);
                None
            }
            RequestKind::OnClose { id } => {
                if let Ok((node, _)) = scheme.lock_node(id) {
                    let alive = |pid| scheme.process_alive(pid);
                    locks.on_close(node, id, &alive, &mut replies);
                }
                scheme.on_close(id);
                state.on_close(id);
                None
            }
            RequestKind::OnDetach { id, pid } => {
                let Ok((node, _)) = scheme.lock_node(id) else {
                    log::warn!("RequestKind::OnDetach with invalid `id`");
                    continue;
                };
                let alive = |pid| scheme.process_alive(pid);
                locks.on_detach(node, pid, &alive, &mut replies);
                None
            }
            _ => {
                // TODO: Redoxfs does not yet support asynchronous file IO. It might still make
                // sense to implement cancellation for huge buffers, e.g. dd bs=1G
                continue;
            }
        };
        for reply in replies.drain(..) {
            if !socket.write_response(reply.into_response(), SignalBehavior::Restart)? {
                break 'serve;
            }
        }
        if let Some(response) = response
            && !socket.write_response(response, SignalBehavior::Restart)?
        {
            break;
        }
    }

    // Cleanup on unmount
    scheme.fs.cleanup()?;
    scheme.fs.disk.flush()?;

    Ok(res)
}

fn handle_call<D: Disk>(
    req: CallRequest,
    scheme: &mut FileScheme<D>,
    state: &mut SchemeState,
    locks: &mut LockTable,
    replies: &mut Vec<Reply>,
) -> Option<Response> {
    let caller = req.caller();
    let mut op = match req.op() {
        Ok(Op::StdFsCall(op)) => op,
        Ok(op) => return Some(op.handle_sync(caller, scheme, state)),
        Err(req) => return Some(Response::new(Err(Error::new(ENOSYS)), req)),
    };
    let fd = op.fd();
    let (_, payload, metadata) = op.fds_and_payload_and_metadata();
    let (kind, arg1, arg2) = (metadata[0] as u8, metadata[1], metadata[2]);
    let lock_kind = matches!(
        StdFsCallKind::try_from_raw(kind),
        Some(StdFsCallKind::Lock | StdFsCallKind::Unlock | StdFsCallKind::GetLock)
    );
    let target = scheme.lock_node(fd);
    let node = target
        .as_ref()
        .ok()
        .filter(|&&(_, flags)| flags & O_STAT == 0)
        .map(|&(node, _)| node);
    if !lock_kind {
        let mut op = Op::StdFsCall(op);
        let result = match op.handle_sync_dont_consume(&caller, scheme, state) {
            SchemeResponse::Regular(result) | SchemeResponse::RegularAndNotifyOnDetach(result) => {
                result
            }
            SchemeResponse::Opened(_) => Err(Error::new(EOPNOTSUPP)),
        };
        return Some(locks.reply_for(op.into_tag(), node, result).into_response());
    }
    let (node, flags) = match (target, node) {
        (Ok(target), Some(_)) => target,
        (Ok(_), None) => return Some(Response::new(Err(Error::new(EBADF)), op)),
        (Err(err), _) => return Some(Response::new(Err(err), op)),
    };
    let call = Call {
        node,
        flags,
        handle: fd,
        pid: caller.pid,
    };
    let alive = |pid| scheme.process_alive(pid);
    let result = match StdFsCallKind::try_from_raw(kind) {
        Some(StdFsCallKind::Lock) => {
            let flags = read_flags(payload);
            locks.lock(&call, op.into_tag(), arg1, arg2, flags, &alive, replies);
            return None;
        }
        Some(StdFsCallKind::Unlock) => {
            let flags = read_flags(payload);
            locks.unlock(&call, arg1, arg2, flags, &alive, replies)
        }
        _ => locks.get_lock(&call, payload),
    };
    Some(
        locks
            .reply_for(op.into_tag(), Some(call.node), result)
            .into_response(),
    )
}
