use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::fmt::Debug;
use core::mem;

use hashbrown::HashMap;
use libredox::protocol::{NsDup, NsPermissions};
use log::{error, warn};
use redox_path::{RedoxPath, RedoxScheme};
use redox_rt::proc::{FdGuard, FdGuardUpper};
use redox_scheme::scheme::{SchemeState, SchemeSync};
use redox_scheme::{
    CallerCtx, OpenResult, RequestKind, Response, SendFdRequest, SignalBehavior, Socket,
};
use syscall::dirent::{DirEntry, DirentBuf, DirentKind};
use syscall::error::*;
use syscall::schemev2::NewFdFlags;
use syscall::{CallFlags, FobtainFdFlags, Stat};

/// Schemes that reach hardware directly. Only a namespace handle that still holds the hardware
/// right opens, lists, forks, registers or removes them (FerrionOS lockdown, S13).
const HARDWARE_SCHEMES: &[&str] = &["irq", "serio", "kernel.acpi", "pci", "acpi"];
/// The kernel's memory scheme. Through a handle without the hardware right it is the kernel's
/// restricted root, which allocates ordinary memory and refuses physical memory and port I/O.
const MEMORY_SCHEME: &str = "memory";
/// The bit of a `ShrinkPermissions` request that keeps the hardware right. libredox's
/// `NsPermissions` does not define it, so a request built from those permissions drops the right.
const NS_HARDWARE: usize = 1 << (usize::BITS - 1);

/// Whether only a handle with the hardware right may register or remove `name`.
fn is_protected_scheme(name: &str) -> bool {
    name == MEMORY_SCHEME || HARDWARE_SCHEMES.contains(&name)
}

#[derive(Debug, Clone)]
struct Namespace {
    schemes: HashMap<String, Arc<FdGuard>>,
}

impl Namespace {
    /// The capability `name` stands for, seen through a handle with or without the hardware right.
    fn resolve(
        &self,
        name: &str,
        hardware: bool,
        restricted_memory: Option<&Arc<FdGuard>>,
    ) -> Option<Arc<FdGuard>> {
        let fd = self.schemes.get(name)?;
        if hardware {
            Some(fd.clone())
        } else if name == MEMORY_SCHEME {
            restricted_memory.cloned()
        } else if HARDWARE_SCHEMES.contains(&name) {
            None
        } else {
            Some(fd.clone())
        }
    }
    fn fork(
        &self,
        buf: &[u8],
        hardware: bool,
        restricted_memory: Option<&Arc<FdGuard>>,
    ) -> Result<Self> {
        let mut schemes = HashMap::new();
        let mut cursor = 0;
        while cursor < buf.len() {
            let len = read_num::<usize>(&buf[cursor..])?;
            cursor += mem::size_of::<usize>();
            let name = String::from_utf8(Vec::from(&buf[cursor..cursor + len]))
                .map_err(|_| Error::new(EINVAL))?;
            cursor += len;
            if name.ends_with('*') {
                let prefix = &name[..name.len() - 1];
                for registered_name in self.schemes.keys() {
                    if registered_name.starts_with(prefix)
                        && let Some(fd) = self.resolve(registered_name, hardware, restricted_memory)
                    {
                        schemes.insert(registered_name.clone(), fd);
                    }
                }
            } else {
                let Some(fd) = self.resolve(&name, hardware, restricted_memory) else {
                    warn!("Scheme {} not found in namespace", name);
                    continue;
                };
                schemes.insert(name, fd);
            }
        }
        Ok(Self { schemes })
    }
    fn remove_scheme(&mut self, scheme: &str) -> Option<()> {
        self.schemes.remove(scheme).map(|_| ())
    }
}

#[derive(Debug, Clone)]
struct NamespaceAccess {
    namespace: Rc<RefCell<Namespace>>,
    permission: NsPermissions,
    /// The hardware right. The init process holds it and passes it only to drivers.
    hardware: bool,
}

impl NamespaceAccess {
    fn has_permission(&self, permission: NsPermissions) -> bool {
        self.permission.contains(permission)
    }
}

#[derive(Debug, Clone)]
struct SchemeRegister {
    target_namespace: Rc<RefCell<Namespace>>,
    scheme_name: String,
}

impl SchemeRegister {
    fn register(&self, fd: FdGuard) -> Result<()> {
        let mut ns = self.target_namespace.borrow_mut();
        if ns.schemes.contains_key(&self.scheme_name) {
            return Err(Error::new(EEXIST));
        }
        ns.schemes.insert(self.scheme_name.clone(), Arc::new(fd));
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum Handle {
    Access(NamespaceAccess),
    Register(SchemeRegister),
    OtherScheme { name: String, fd: Rc<FdGuard> },
    List(NamespaceAccess),
}

pub struct NamespaceScheme<'sock> {
    socket: &'sock Socket,
    handles: HashMap<usize, Handle>,
    root_namespace: Namespace,
    next_id: usize,
    scheme_creation_cap: Rc<FdGuardUpper>,
    /// What `memory` resolves to without the hardware right; `None` hides `memory` instead.
    restricted_memory: Option<Arc<FdGuard>>,
}

const HIGH_PERMISSIONS: NsPermissions = NsPermissions::SCHEME_CREATE;

impl<'sock> NamespaceScheme<'sock> {
    pub fn new(
        socket: &'sock Socket,
        schemes: HashMap<String, Arc<FdGuard>>,
        scheme_creation_cap: FdGuardUpper,
    ) -> Self {
        // FerrionOS lockdown (S13): the kernel hands its full memory root to the boot process.
        // Handles without the hardware right get the restricted root derived from it.
        let restricted_memory = schemes.get(MEMORY_SCHEME).and_then(|memory| {
            memory
                .openat("restricted-root", libredox::protocol::O_CLOEXEC, 0)
                .inspect_err(|err| error!("No restricted memory root, hiding `memory`: {}", err))
                .ok()
                .map(Arc::new)
        });
        Self {
            socket,
            handles: HashMap::new(),
            root_namespace: Namespace { schemes },
            next_id: 0,
            scheme_creation_cap: Rc::new(scheme_creation_cap),
            restricted_memory,
        }
    }

    fn add_namespace(
        &mut self,
        id: usize,
        schemes: Namespace,
        permission: NsPermissions,
        hardware: bool,
    ) {
        let handle = Handle::Access(NamespaceAccess {
            namespace: Rc::new(RefCell::new(schemes)),
            permission,
            hardware,
        });
        self.handles.insert(id, handle);
    }

    fn get_ns_access(&self, id: usize) -> Option<&NamespaceAccess> {
        let handle = self.handles.get(&id);
        match handle {
            Some(Handle::Access(access)) => Some(access),
            _ => None,
        }
    }

    fn open_namespace_resource(
        &mut self,
        ns_access: &NamespaceAccess,
        redox_path: RedoxPath<'_>,
        _flags: usize,
        _fcntl_flags: u32,
        _ctx: &CallerCtx,
    ) -> Result<usize> {
        let (_scheme, reference) = redox_path.as_parts().ok_or(Error::new(EINVAL))?;

        match reference.as_ref() {
            "scheme-creation-cap" => {
                if !ns_access.has_permission(NsPermissions::SCHEME_CREATE) {
                    error!("Permission denied to get scheme creation capability");
                    return Err(Error::new(EACCES));
                }

                let new_id = self.next_id;
                self.next_id += 1;
                self.handles.insert(new_id, Handle::List(ns_access.clone()));

                let resource_id = self.next_id;
                self.next_id += 1;

                let dup_fd = self.scheme_creation_cap.dup(&[])?;

                self.handles.insert(
                    resource_id,
                    Handle::OtherScheme {
                        name: redox_path.to_string(),
                        fd: Rc::new(dup_fd),
                    },
                );
                Ok(resource_id)
            }
            "" => {
                if !ns_access.has_permission(NsPermissions::LIST) {
                    error!("Permission denied to list schemes in namespace root");
                    return Err(Error::new(EACCES));
                }

                let new_id = self.next_id;
                self.next_id += 1;

                self.handles.insert(new_id, Handle::List(ns_access.clone()));

                Ok(new_id)
            }
            _ => {
                error!("Unknown special reference: {}", reference.as_ref());
                Err(Error::new(EINVAL))
            }
        }
    }

    fn open_scheme_resource(
        &self,
        ns_access: &NamespaceAccess,
        scheme: &str,
        flags: usize,
        ctx: &CallerCtx,
    ) -> Result<FdGuard> {
        let namespace = ns_access.namespace.borrow();
        let Some(cap_fd) =
            namespace.resolve(scheme, ns_access.hardware, self.restricted_memory.as_ref())
        else {
            if namespace.schemes.contains_key(scheme) {
                warn!(
                    "pid {} lacks the hardware right for {} (FerrionOS lockdown)",
                    ctx.pid, scheme
                );
            } else {
                log::info!("Scheme {:?} not found in namespace", scheme);
            }
            return Err(Error::new(ENODEV));
        };

        if flags & syscall::O_DIRECTORY == 0 {
            return Err(Error::new(EISDIR));
        }

        cap_fd.dup(&[])
    }

    fn fork_namespace(&mut self, ns_access: &NamespaceAccess, names: &[u8]) -> Result<usize> {
        let new_id = self.next_id;
        let new_namespace = ns_access
            .namespace
            .borrow()
            .fork(names, ns_access.hardware, self.restricted_memory.as_ref())
            .inspect_err(|&e| {
                error!("Failed to fork namespace {}: {}", new_id, e);
            })?;
        self.add_namespace(
            new_id,
            new_namespace,
            NsPermissions::all().difference(HIGH_PERMISSIONS),
            ns_access.hardware,
        );
        self.next_id += 1;
        Ok(new_id)
    }

    /// `raw_permission` is a set of `NsPermissions`, plus `NS_HARDWARE` to keep the hardware right.
    fn shrink_permissions(
        &mut self,
        mut ns: NamespaceAccess,
        raw_permission: usize,
    ) -> Result<usize> {
        ns.permission = ns
            .permission
            .intersection(NsPermissions::from_bits_truncate(raw_permission));
        ns.hardware &= raw_permission & NS_HARDWARE != 0;
        let next_id = self.next_id;
        self.handles.insert(next_id, Handle::Access(ns));
        self.next_id += 1;
        Ok(next_id)
    }

    fn on_close(&mut self, id: usize) {
        let _ = self.handles.remove(&id);
    }
}

impl<'sock> SchemeSync for NamespaceScheme<'sock> {
    fn openat(
        &mut self,
        fd: usize,
        path: &str,
        flags: usize,
        fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        let handle = self.handles.get(&fd).cloned().ok_or_else(|| {
            error!("Handle with ID {} not found", fd);
            Error::new(ENOENT)
        })?;

        match handle {
            Handle::Access(ns_access) => {
                let redox_path = RedoxPath::from_absolute(path).ok_or(Error::new(EINVAL))?;
                let (scheme, _reference) = redox_path.as_parts().ok_or(Error::new(EINVAL))?;

                match scheme.as_ref() {
                    "namespace" | "" => {
                        let new_id = self.open_namespace_resource(
                            &ns_access,
                            redox_path,
                            flags,
                            fcntl_flags,
                            ctx,
                        )?;
                        Ok(OpenResult::ThisScheme {
                            number: new_id,
                            flags: NewFdFlags::empty(),
                        })
                    }
                    _ => {
                        let res_fd =
                            self.open_scheme_resource(&ns_access, scheme.as_ref(), flags, ctx)?;
                        Ok(OpenResult::OtherScheme { fd: res_fd.take() })
                    }
                }
            }
            Handle::List(ns_access) => {
                if path.is_empty() || path == "." {
                    let new_id = self.next_id;
                    self.next_id += 1;

                    self.handles.insert(new_id, Handle::List(ns_access.clone()));
                    Ok(OpenResult::ThisScheme {
                        number: new_id,
                        flags: NewFdFlags::empty(),
                    })
                } else {
                    error!("Cannot open a path under List handle: {}", path);
                    Err(Error::new(EINVAL))
                }
            }
            Handle::OtherScheme {
                name: saved_path_str,
                fd: target_fd,
            } => {
                let saved_path =
                    RedoxPath::from_absolute(&saved_path_str).ok_or(Error::new(syscall::EPROTO))?;
                let (_scheme, reference) =
                    saved_path.as_parts().ok_or(Error::new(syscall::EPROTO))?;

                if reference.as_ref() == path {
                    Ok(OpenResult::OtherScheme {
                        fd: target_fd.dup(&[])?.take(),
                    })
                } else {
                    error!(
                        "Resource path mismatch: expected {}, got {}",
                        reference.as_ref(),
                        path,
                    );
                    Err(Error::new(EINVAL))
                }
            }
            _ => {
                error!("Handle {} is not accessible for openat", fd);
                Err(Error::new(EBADF))
            }
        }
    }

    fn dup(&mut self, id: usize, buf: &[u8], _ctx: &CallerCtx) -> Result<OpenResult> {
        let ns_access = self
            .get_ns_access(id)
            .ok_or_else(|| {
                error!("Namespace with ID {} not found", id);
                Error::new(ENOENT)
            })?
            .clone();

        let raw_kind = read_num::<usize>(buf)?;
        let Some(kind) = NsDup::try_from_raw(raw_kind) else {
            error!("Unknown dup kind: {}", raw_kind);
            return Err(Error::new(EINVAL));
        };
        let payload = &buf[mem::size_of::<NsDup>()..];
        let new_id = match kind {
            NsDup::ForkNs => self.fork_namespace(&ns_access, payload)?,
            NsDup::ShrinkPermissions => {
                self.shrink_permissions(ns_access, read_num::<usize>(payload)?)?
            }
            NsDup::IssueRegister => {
                let name = core::str::from_utf8(payload).map_err(|_| Error::new(EINVAL))?;
                let scheme_name = RedoxScheme::new(name).ok_or_else(|| {
                    error!("Invalid scheme name: {}", name);
                    Error::new(EINVAL)
                })?;

                if !ns_access.has_permission(NsPermissions::INSERT) {
                    error!(
                        "Permission denied to issue register capability for namespace {}",
                        id
                    );
                    return Err(Error::new(EACCES));
                }
                if !ns_access.hardware && is_protected_scheme(scheme_name.as_ref()) {
                    error!("Registering {} needs the hardware right", name);
                    return Err(Error::new(EACCES));
                }
                let new_id = self.next_id;
                let register_cap = Handle::Register(SchemeRegister {
                    target_namespace: ns_access.namespace.clone(),
                    scheme_name: scheme_name.as_ref().to_string(),
                });
                self.handles.insert(new_id, register_cap);
                self.next_id += 1;
                new_id
            }
        };

        Ok(OpenResult::ThisScheme {
            number: new_id,
            flags: NewFdFlags::empty(),
        })
    }

    fn unlinkat(&mut self, fd: usize, path: &str, _flags: usize, _ctx: &CallerCtx) -> Result<()> {
        let ns_access = self.get_ns_access(fd).ok_or_else(|| {
            error!("Namespace with ID {} not found", fd);
            Error::new(ENOENT)
        })?;
        let mut ns = ns_access.namespace.borrow_mut();

        let redox_path = RedoxPath::from_absolute(path).ok_or(Error::new(EINVAL))?;
        let (scheme, reference) = redox_path.as_parts().ok_or(Error::new(EINVAL))?;
        if !reference.as_ref().is_empty() {
            return Err(Error::new(EXDEV));
        }
        if !ns_access.has_permission(NsPermissions::DELETE) {
            error!("Permission denied to remove scheme for namespace {}", fd);
            return Err(Error::new(EACCES));
        }
        if !ns_access.hardware && is_protected_scheme(scheme.as_ref()) {
            error!("Removing {} needs the hardware right", scheme);
            return Err(Error::new(EACCES));
        }
        match ns.remove_scheme(scheme.as_ref()) {
            Some(_) => Ok(()),
            None => {
                error!("Scheme {} not found in namespace", scheme);
                Err(Error::new(ENODEV))
            }
        }
    }

    fn on_sendfd(&mut self, sendfd_request: &SendFdRequest) -> Result<usize> {
        let namespace_id = sendfd_request.id();
        let num_fds = sendfd_request.num_fds();

        let handle = self.handles.get(&namespace_id).ok_or_else(|| {
            error!("Namespace with ID {} not found", namespace_id);
            Error::new(ENOENT)
        })?;
        let Handle::Register(register_cap) = handle else {
            error!(
                "Handle with ID {} is not a register capability",
                namespace_id
            );
            return Err(Error::new(EACCES));
        };

        if num_fds == 0 {
            return Ok(0);
        }
        if num_fds > 1 {
            error!("Can only send one fd at a time");
            return Err(Error::new(EINVAL));
        }
        let mut new_fd = usize::MAX;
        if let Err(e) = sendfd_request.obtain_fd(
            self.socket,
            FobtainFdFlags::UPPER_TBL,
            core::slice::from_mut(&mut new_fd),
        ) {
            error!("on_sendfd: obtain_fd failed with error: {:?}", e);
            return Err(e);
        }
        register_cap.register(FdGuard::new(new_fd))?;

        Ok(num_fds)
    }

    fn getdents<'buf>(
        &mut self,
        id: usize,
        mut buf: DirentBuf<&'buf mut [u8]>,
        opaque_offset: u64,
    ) -> Result<DirentBuf<&'buf mut [u8]>> {
        let Handle::List(ns_access) = self.handles.get(&id).ok_or(Error::new(EBADF))? else {
            return Err(Error::new(ENOTDIR));
        };

        if !ns_access.has_permission(NsPermissions::LIST) {
            return Err(Error::new(EACCES));
        }

        let ns = ns_access.namespace.borrow();

        let opaque_offset = opaque_offset as usize;
        for (i, (name, _)) in ns.schemes.iter().enumerate().skip(opaque_offset) {
            let hidden = ns
                .resolve(name, ns_access.hardware, self.restricted_memory.as_ref())
                .is_none();
            if name.is_empty() || hidden {
                continue;
            }
            if let Err(err) = buf.entry(DirEntry {
                kind: DirentKind::Unspecified,
                name,
                inode: 0,
                next_opaque_id: i as u64 + 1,
            }) {
                if err.errno == EINVAL && i > opaque_offset {
                    break;
                } else {
                    return Err(err);
                }
            }
        }

        Ok(buf)
    }

    fn fstat(&mut self, id: usize, stat: &mut Stat, _ctx: &CallerCtx) -> Result<()> {
        let resource_stat = match self.handles.get(&id).ok_or(Error::new(EBADF))? {
            Handle::List(_) | Handle::OtherScheme { .. } => Stat {
                st_mode: 0o444 | syscall::MODE_DIR,
                st_uid: 0,
                st_gid: 0,
                st_size: 0,
                ..Default::default()
            },
            Handle::Access(_) | Handle::Register(_) => Stat {
                st_mode: 0o666 | syscall::MODE_FILE,
                st_uid: 0,
                st_gid: 0,
                st_size: 0,
                ..Default::default()
            },
        };
        *stat = resource_stat;
        Ok(())
    }
}

trait NumFromBytes: Sized + Debug {
    fn from_le_bytes_slice(buffer: &[u8]) -> Result<Self, Error>;
}

macro_rules! num_from_bytes_impl {
    ($($t:ty),*) => {
        $(
            impl NumFromBytes for $t {
                fn from_le_bytes_slice(buffer: &[u8]) -> Result<Self, Error> {
                    let size = mem::size_of::<Self>();
                    let buffer_slice = buffer.get(..size).and_then(|s| s.try_into().ok());

                    if let Some(slice) = buffer_slice {
                        Ok(Self::from_le_bytes(slice))
                    } else {
                        error!(
                            "read_num: buffer is too short to read num of size {} (buffer len: {})",
                            size, buffer.len()
                        );
                        Err(Error::new(EINVAL))
                    }
                }
            }
        )*
    };
}

num_from_bytes_impl!(usize);

fn read_num<T>(buffer: &[u8]) -> Result<T, Error>
where
    T: NumFromBytes,
{
    T::from_le_bytes_slice(buffer)
}

pub fn run(
    sync_pipe: FdGuard,
    socket: Socket,
    schemes: HashMap<String, Arc<FdGuard>>,
    scheme_creation_cap: FdGuard,
) -> ! {
    let scheme_creation_cap = scheme_creation_cap
        .to_upper()
        .expect("Failed to move scheme creation cap into upper");
    let mut state = SchemeState::new();
    let mut scheme = NamespaceScheme::new(&socket, schemes, scheme_creation_cap);

    // send namespace fd to bootstrap
    let new_id = scheme.next_id;
    // The boot process, and through it init, starts with the hardware right.
    scheme.add_namespace(
        new_id,
        scheme.root_namespace.clone(),
        NsPermissions::all(),
        true,
    );
    scheme.next_id += 1;
    let cap_fd = scheme
        .socket
        .create_this_scheme_fd(0, new_id, 0, 0)
        .expect("nsmgr: failed to create namespace fd");
    let _ = redox_rt::sys::sys_call_wo(
        sync_pipe.as_raw_fd(),
        &cap_fd.to_ne_bytes(),
        CallFlags::FD,
        &[],
    );
    drop(sync_pipe);

    log::info!("bootstrap: namespace scheme start!");
    while let Some(req) = socket
        .next_request(SignalBehavior::Restart)
        .expect("bootstrap: failed to read scheme request from kernel")
    {
        match req.kind() {
            RequestKind::Call(req) => {
                let resp = req.handle_sync(&mut scheme, &mut state);

                if !socket
                    .write_response(resp, SignalBehavior::Restart)
                    .expect("bootstrap: failed to write scheme response to kernel")
                {
                    break;
                }
            }
            RequestKind::OnClose { id } => scheme.on_close(id),
            RequestKind::SendFd(sendfd_request) => {
                let result = scheme.on_sendfd(&sendfd_request);
                let resp = Response::new(result, sendfd_request);
                if !socket
                    .write_response(resp, SignalBehavior::Restart)
                    .expect("bootstrap: failed to write scheme response to kernel")
                {
                    break;
                }
            }
            _ => (),
        }
    }

    unreachable!()
}
