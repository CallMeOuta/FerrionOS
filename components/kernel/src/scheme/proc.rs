use crate::{
    context::{
        self,
        context::{AccessStop, FdTbl, HardBlockedReason, LockedFdTbl, SignalState},
        file::{InternalFlags, KernelSchemeRef},
        memory::{handle_notify_files, AddrSpace, AddrSpaceWrapper, Grant, PageSpan, UnmapVec},
        unblock_context, wakeup_context, Context, ContextLock, Status,
    },
    cpu_set::LogicalCpuId,
    event,
    memory::{Page, VirtualAddress, PAGE_SIZE},
    numa, ptrace,
    scheme::{
        memory::{MemoryScheme, MemoryType},
        FileHandle, KernelScheme,
    },
    sync::{CleanLockToken, LockToken, RwLock, L1, L4},
    syscall::{
        data::{GrantDesc, Map, SetSighandlerData, Stat},
        error::*,
        flag::*,
        usercopy::{UserSliceRo, UserSliceRw, UserSliceWo},
        EnvRegisters, FloatRegisters, IntRegisters,
    },
};

use super::{CallerCtx, KernelSchemes, OpenResult, SchemeExt};
use ::syscall::{ProcSchemeAttrs, SigProcControl, Sigcontrol};
use alloc::{
    boxed::Box,
    collections::VecDeque,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    mem::size_of,
    num::NonZeroUsize,
    slice, str,
    sync::atomic::{AtomicUsize, Ordering},
};
use hashbrown::{hash_map::DefaultHashBuilder, HashMap};
use syscall::{data::GlobalSchemes, CpuSet, Error, NumaMemoryPolicy};

fn read_from(dst: UserSliceWo, src: &[u8], offset: u64) -> Result<usize> {
    let avail_src = usize::try_from(offset)
        .ok()
        .and_then(|o| src.get(o..))
        .unwrap_or(&[]);
    dst.copy_common_bytes_from_slice(avail_src)
}

const MAX_ACCESS_STOP_HOLDERS: usize = 64;
const MAX_ACCESS_STOP_WAIT_ITERS: usize = 65_536;

fn release_access_stop(context: &mut Context) -> (bool, Option<LogicalCpuId>) {
    assert!(matches!(context.access_stop, AccessStop::Stopped { .. }));
    let is_last = match &mut context.access_stop {
        AccessStop::Stopped { holders, .. } => {
            let count = holders.get();
            assert!(count > 0);
            if let Some(next) = NonZeroUsize::new(count - 1) {
                *holders = next;
                false
            } else {
                true
            }
        }
        AccessStop::None => unreachable!(),
    };
    if !is_last {
        return (false, None);
    }
    let AccessStop::Stopped { saved, holders } =
        core::mem::replace(&mut context.access_stop, AccessStop::None)
    else {
        unreachable!();
    };
    assert!(holders.get() == 1);
    if matches!(
        context.status,
        Status::HardBlocked {
            reason: HardBlockedReason::StoppedForAccess,
        }
    ) && !context.being_sigkilled
    {
        context.status = saved;
    }
    let wake = context.status.is_runnable();
    assert!(!(wake && context.status.is_dead()));
    (wake, context.cpu_id)
}

fn try_stop_context<T>(
    context_ref: Arc<ContextLock>,
    token: &mut CleanLockToken,
    callback: impl FnOnce(&mut Context, LockToken<'_, L4>) -> Result<T>,
) -> Result<T> {
    if context::is_current(&context_ref) {
        let context = &mut context_ref.write(token.token());
        let (context, token) = context.token_split();
        return callback(context, token);
    }

    {
        let mut guard = context_ref.write(token.token());
        if guard.being_sigkilled || guard.status.is_dead() {
            return Err(Error::new(ESRCH));
        }
        match &mut guard.access_stop {
            AccessStop::None => {
                let saved = core::mem::replace(
                    &mut guard.status,
                    Status::HardBlocked {
                        reason: HardBlockedReason::StoppedForAccess,
                    },
                );
                guard.access_stop = AccessStop::Stopped {
                    saved,
                    holders: NonZeroUsize::MIN,
                };
            }
            AccessStop::Stopped { holders, .. } => {
                let count = holders.get();
                assert!(count > 0);
                if count >= MAX_ACCESS_STOP_HOLDERS {
                    return Err(Error::new(EAGAIN));
                }
                *holders = NonZeroUsize::new(count + 1).ok_or(Error::new(EAGAIN))?;
            }
        }
        if !guard.running {
            let (context, l4_token) = guard.token_split();
            let ret = callback(context, l4_token);
            let (wake, cpu_id) = release_access_stop(context);
            drop(guard);
            if wake {
                wakeup_context(&context_ref, cpu_id, &mut token.downgrade());
            }
            return ret;
        }
    }

    for _ in 0..MAX_ACCESS_STOP_WAIT_ITERS {
        context::switch(token);
        let mut guard = context_ref.write(token.token());
        if guard.being_sigkilled || guard.status.is_dead() {
            let (wake, cpu_id) = release_access_stop(&mut guard);
            drop(guard);
            if wake {
                wakeup_context(&context_ref, cpu_id, &mut token.downgrade());
            }
            return Err(Error::new(ESRCH));
        }
        if !guard.running {
            let (context, l4_token) = guard.token_split();
            let ret = callback(context, l4_token);
            let (wake, cpu_id) = release_access_stop(context);
            drop(guard);
            if wake {
                wakeup_context(&context_ref, cpu_id, &mut token.downgrade());
            }
            return ret;
        }
    }

    let mut guard = context_ref.write(token.token());
    let (wake, cpu_id) = release_access_stop(&mut guard);
    drop(guard);
    if wake {
        wakeup_context(&context_ref, cpu_id, &mut token.downgrade());
    }
    Err(Error::new(EAGAIN))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegsKind {
    Float,
    Int,
    Env,
}
const ATTR_CALL_SET_RUID: u64 = 0x4645_0002;

#[derive(Clone)]
enum ContextHandle {
    // Opened by the process manager, after which it is locked. This capability is used to open
    // Attr handles, to set ens/euid/egid/pid.
    Authority,
    Attr,

    Status {
        privileged: bool,
    }, // can write ContextVerb

    Regs(RegsKind),
    Sighandler,
    /// The thread's foreign-call registration; only the thread itself may change it.
    ForeignCall,
    Start,
    NewFiletable {
        filetable: Arc<LockedFdTbl>,
        binary_format: bool,
        data: Box<[u8]>,
    },
    Filetable {
        filetable: Weak<LockedFdTbl>,
        binary_format: bool,
        data: Box<[u8]>,
    },
    AddrSpace {
        addrspace: Arc<AddrSpaceWrapper>,
    },
    CurrentAddrSpace,

    AwaitingAddrSpaceChange {
        new: Arc<AddrSpaceWrapper>,
        new_sp: usize,
        new_ip: usize,
        arg1: Option<usize>,
    },

    CurrentFiletable,

    AwaitingFiletableChange {
        new_ft: Arc<LockedFdTbl>,
    },

    // TODO: Remove this once openat is implemented, or allow openat-via-dup via e.g. the top-level
    // directory.
    OpenViaDup,
    SchedAffinity,

    MmapMinAddr(Arc<AddrSpaceWrapper>),
}
impl ContextHandle {
    fn owns_filetable(&self) -> bool {
        matches!(
            self,
            Self::NewFiletable { .. } | Self::AwaitingFiletableChange { .. }
        )
    }
}

#[derive(Clone)]
struct Handle {
    context: Arc<ContextLock>,
    kind: ContextHandle,
}

struct HandleTable {
    map: HashMap<usize, Handle>,
    owned_filetables: usize,
}

impl HandleTable {
    const fn new() -> Self {
        Self {
            map: HashMap::with_hasher(DefaultHashBuilder::new()),
            owned_filetables: 0,
        }
    }

    fn get(&self, id: &usize) -> Option<&Handle> {
        self.map.get(id)
    }

    #[cfg(feature = "debugger")]
    fn iter(&self) -> hashbrown::hash_map::Iter<'_, usize, Handle> {
        self.map.iter()
    }

    fn insert(&mut self, id: usize, handle: Handle) -> Option<Handle> {
        if handle.kind.owns_filetable() {
            self.owned_filetables = self
                .owned_filetables
                .checked_add(1)
                .expect("owned filetable count overflow");
        }
        let prev = self.map.insert(id, handle);
        if let Some(ref old) = prev
            && old.kind.owns_filetable()
        {
            assert!(self.owned_filetables > 0);
            self.owned_filetables -= 1;
        }
        prev
    }

    fn remove(&mut self, id: &usize) -> Option<Handle> {
        let prev = self.map.remove(id);
        if let Some(ref old) = prev
            && old.kind.owns_filetable()
        {
            assert!(self.owned_filetables > 0);
            self.owned_filetables -= 1;
        }
        prev
    }

    fn replace(&mut self, id: usize, handle: Handle) -> Option<Handle> {
        let entry = self.map.get_mut(&id)?;
        let old_owned = entry.kind.owns_filetable();
        let new_owned = handle.kind.owns_filetable();
        let old = core::mem::replace(entry, handle);
        match (old_owned, new_owned) {
            (false, true) => {
                self.owned_filetables = self
                    .owned_filetables
                    .checked_add(1)
                    .expect("owned filetable count overflow");
            }
            (true, false) => {
                assert!(self.owned_filetables > 0);
                self.owned_filetables -= 1;
            }
            _ => {}
        }
        Some(old)
    }

    fn set_filetable_data(&mut self, id: usize, new_data: Box<[u8]>) -> Option<()> {
        let entry = self.map.get_mut(&id)?;
        if let ContextHandle::Filetable { ref mut data, .. } = entry.kind {
            *data = new_data;
        }
        Some(())
    }

    fn owned_filetables(&self) -> usize {
        self.owned_filetables
    }
}

pub struct ProcScheme;

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static HANDLES: RwLock<L1, HandleTable> = RwLock::new(HandleTable::new());

fn close_filetable_iteratively(
    initial: FdTbl,
    live_at_entry: usize,
    token: &mut CleanLockToken,
) {
    let mut worklist = VecDeque::new();
    worklist.push_back(initial);
    let mut drained = 0usize;
    let mut max_steps = live_at_entry
        .checked_add(1)
        .expect("filetable drain bound overflow");
    assert!(max_steps <= live_at_entry + drained + 1);

    while let Some(mut ft) = worklist.pop_front() {
        assert!(drained < max_steps);
        drained += 1;
        ft.drain_with(|file| {
            let Ok(Some(desc)) = file.detach_and_take_last(token) else {
                return;
            };
            if matches!(desc.scheme_ref, KernelSchemeRef::Global(GlobalSchemes::Proc)) {
                event::unregister_file(GlobalSchemes::Proc.scheme_id(), desc.number, token);
                if let Ok(Some((next_ft, from_live_swap, live_after))) =
                    ProcScheme::close_nonrecursive(desc.number, token)
                {
                    if from_live_swap {
                        let live_total = live_after
                            .checked_add(worklist.len())
                            .expect("filetable drain bound overflow");
                        let candidate = live_total
                            .checked_add(drained)
                            .and_then(|v| v.checked_add(1))
                            .expect("filetable drain bound overflow");
                        max_steps = candidate;
                        assert!(max_steps <= live_total + drained + 1);
                    }
                    worklist.push_back(next_ft);
                }
            } else {
                let _ = desc.try_close(token);
            }
        });
    }
    assert!(worklist.is_empty());
    assert!(drained <= max_steps);
}

#[cfg(feature = "debugger")]
#[allow(dead_code)]
pub fn foreach_addrsp(
    token: &mut CleanLockToken,
    mut f: impl FnMut(&Arc<AddrSpaceWrapper>, LockToken<L1>),
) {
    let mut handles_guard = HANDLES.read(token.token());
    let (handles, mut token) = handles_guard.token_split();
    for (_, handle) in handles.iter() {
        let Handle {
            kind:
                ContextHandle::AddrSpace { addrspace, .. }
                | ContextHandle::AwaitingAddrSpaceChange { new: addrspace, .. }
                | ContextHandle::MmapMinAddr(addrspace),
            ..
        } = handle
        else {
            continue;
        };
        f(addrspace, token.token());
    }
}

fn new_handle(
    (handle, fl): (Handle, InternalFlags),
    token: &mut CleanLockToken,
) -> Result<(usize, InternalFlags)> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let _ = HANDLES.write(token.token()).insert(id, handle);
    Ok((id, fl))
}

enum OpenTy {
    Ctxt(Arc<ContextLock>),
    Auth,
}

impl ProcScheme {
    fn openat_context(
        &self,
        path: &str,
        context: &Arc<ContextLock>,
        token: &mut CleanLockToken,
    ) -> Result<Option<(ContextHandle, bool)>> {
        Ok(Some(match path {
            "addrspace" => (
                ContextHandle::AddrSpace {
                    addrspace: Arc::clone(
                        context
                            .read(token.token())
                            .addr_space()
                            .map_err(|_| Error::new(ENOENT))?,
                    ),
                },
                true,
            ),
            "filetable" => (
                ContextHandle::Filetable {
                    filetable: Arc::downgrade(&context.read(token.token()).files),
                    binary_format: false,
                    data: Box::new([]),
                },
                true,
            ),
            "filetable-binary" => (
                ContextHandle::Filetable {
                    filetable: Arc::downgrade(&context.read(token.token()).files),
                    binary_format: true,
                    data: Box::new([]),
                },
                true,
            ),
            "current-addrspace" => (ContextHandle::CurrentAddrSpace, false),
            "current-filetable" => (ContextHandle::CurrentFiletable, false),
            "regs/float" => (ContextHandle::Regs(RegsKind::Float), false),
            "regs/int" => (ContextHandle::Regs(RegsKind::Int), false),
            "regs/env" => (ContextHandle::Regs(RegsKind::Env), false),
            "sighandler" => (ContextHandle::Sighandler, false),
            "foreign-call" => (ContextHandle::ForeignCall, false),
            "start" => (ContextHandle::Start, false),
            "open_via_dup" => (ContextHandle::OpenViaDup, false),
            "mmap-min-addr" => (
                ContextHandle::MmapMinAddr(Arc::clone(
                    context
                        .read(token.token())
                        .addr_space()
                        .map_err(|_| Error::new(ENOENT))?,
                )),
                false,
            ),
            "sched-affinity" => (ContextHandle::SchedAffinity, true),
            "status" => (ContextHandle::Status { privileged: false }, false),
            _ if path.starts_with("auth-") => {
                let nonprefix = &path["auth-".len()..];
                let next_dash = nonprefix.find('-').ok_or(Error::new(ENOENT))?;
                let auth_fd = nonprefix[..next_dash]
                    .parse::<usize>()
                    .map_err(|_| Error::new(ENOENT))?;
                let actual_name = &nonprefix[next_dash + 1..];

                let handle = match actual_name {
                    "attrs" => ContextHandle::Attr,
                    "status" => ContextHandle::Status { privileged: true },
                    _ => return Err(Error::new(ENOENT)),
                };

                let (hopefully_this_scheme, number) = extract_scheme_number(auth_fd, token)?;
                verify_scheme(hopefully_this_scheme)?;
                if !matches!(
                    HANDLES
                        .read(token.token())
                        .get(&number)
                        .ok_or(Error::new(ENOENT))?
                        .kind,
                    ContextHandle::Authority
                ) {
                    return Err(Error::new(ENOENT));
                }

                (handle, false)
            }
            _ => return Ok(None),
        }))
    }
    fn open_inner(
        &self,
        ty: OpenTy,
        operation_str: Option<&str>,
        _flags: usize,
        token: &mut CleanLockToken,
    ) -> Result<(usize, InternalFlags)> {
        let operation_name = operation_str.ok_or(Error::new(EINVAL))?;
        let (mut handle, positioned) = match ty {
            OpenTy::Ctxt(context) => match self.openat_context(operation_name, &context, token)? {
                Some((kind, positioned)) => (Handle { context, kind }, positioned),
                _ => {
                    return Err(Error::new(EINVAL));
                }
            },
            OpenTy::Auth => {
                extern "C" fn ret() {
                    let mut token = unsafe { CleanLockToken::new() };
                    let killed = context::current().read(token.token()).being_sigkilled;
                    if killed {
                        crate::syscall::exit_this_context(None, &mut token);
                    }
                }
                let context = match operation_str.ok_or(Error::new(ENOENT))? {
                    "new-context" => {
                        let id = NonZeroUsize::new(NEXT_ID.fetch_add(1, Ordering::Relaxed))
                            .ok_or(Error::new(EMFILE))?;
                        let context = context::spawn(true, Some(id), ret, token)?;
                        HANDLES.write(token.token()).insert(
                            id.get(),
                            Handle {
                                context,
                                kind: ContextHandle::OpenViaDup,
                            },
                        );
                        return Ok((id.get(), InternalFlags::empty()));
                    }
                    "cur-context" => Arc::clone(&*context::current()),
                    _ => return Err(Error::new(ENOENT)),
                };

                (
                    Handle {
                        context,
                        kind: ContextHandle::OpenViaDup,
                    },
                    false,
                )
            }
        };

        {
            let filetable_opt = match handle {
                Handle {
                    kind:
                        ContextHandle::Filetable {
                            ref filetable,
                            binary_format,
                            ref mut data,
                        },
                    ..
                } => Some((
                    filetable.upgrade().ok_or(Error::new(EOWNERDEAD))?,
                    binary_format,
                    data,
                )),
                Handle {
                    kind:
                        ContextHandle::NewFiletable {
                            ref filetable,
                            binary_format,
                            ref mut data,
                        },
                    ..
                } => Some((Arc::clone(filetable), binary_format, data)),
                _ => None,
            };
            if let Some((filetable, binary_format, data)) = filetable_opt {
                *data = if binary_format {
                    let mut data = Vec::new();
                    for index in filetable
                        .read(token.token())
                        .enumerate_fds()
                        .map(|(i, _)| i)
                    {
                        data.extend((index as u64).to_le_bytes());
                    }
                    data.into_boxed_slice()
                } else {
                    use core::fmt::Write;

                    let mut data = String::new();
                    for index in filetable
                        .read(token.token())
                        .enumerate_fds()
                        .map(|(i, _)| i)
                    {
                        writeln!(data, "{}", index).unwrap();
                    }
                    data.into_bytes().into_boxed_slice()
                };
            }
        };

        let (id, int_fl) = new_handle(
            (
                handle.clone(),
                if positioned {
                    InternalFlags::POSITIONED
                } else {
                    InternalFlags::empty()
                },
            ),
            token,
        )?;

        Ok((id, int_fl))
    }
}

impl ProcScheme {
    fn close_nonrecursive(
        id: usize,
        token: &mut CleanLockToken,
    ) -> Result<Option<(FdTbl, bool, usize)>> {
        let (handle, live_owned_now) = {
            let mut handles = HANDLES.write(token.token());
            let handle = handles.remove(&id).ok_or(Error::new(EBADF))?;
            (handle, handles.owned_filetables())
        };

        match handle {
            Handle {
                context,
                kind:
                    ContextHandle::AwaitingAddrSpaceChange {
                        new,
                        new_sp,
                        new_ip,
                        arg1,
                    },
            } => {
                let mut new = Some(new);
                let old_ctx = try_stop_context(context, token, |context, token| {
                    let regs = context.regs_mut().ok_or(Error::new(EBADFD))?;
                    regs.set_instr_pointer(new_ip);
                    regs.set_stack_pointer(new_sp);
                    #[cfg(any(
                        target_arch = "x86_64",
                        target_arch = "aarch64",
                        target_arch = "riscv64"
                    ))]
                    regs.set_arg1(arg1);

                    Ok(context.set_addr_space(new.take(), token))
                });
                if let Some(unused_new) = new
                    && let Some(addrspace) = Arc::into_inner(unused_new)
                {
                    addrspace.into_drop(token);
                }
                let old_ctx = old_ctx?;
                if let Some(old_ctx) = old_ctx
                    && let Some(addrspace) = Arc::into_inner(old_ctx)
                {
                    addrspace.into_drop(token);
                }
                let _ = ptrace::send_event(
                    crate::syscall::ptrace_event!(PTRACE_EVENT_ADDRSPACE_SWITCH, 0),
                    token,
                );
            }
            Handle {
                kind: ContextHandle::AddrSpace { addrspace } | ContextHandle::MmapMinAddr(addrspace),
                ..
            } => {
                if let Some(addrspace) = Arc::into_inner(addrspace) {
                    addrspace.into_drop(token);
                }
            }
            Handle {
                kind: ContextHandle::NewFiletable { filetable, .. },
                ..
            } => {
                if let Some(ft) = Arc::into_inner(filetable) {
                    return Ok(Some((ft.into_inner(), false, live_owned_now)));
                }
            }
            Handle {
                kind: ContextHandle::AwaitingFiletableChange { new_ft },
                context,
            } => {
                let (table_to_close, from_live_swap) = {
                    let mut guard = context.write(token.token());
                    if guard.files_closed {
                        (new_ft, false)
                    } else {
                        (core::mem::replace(&mut guard.files, new_ft), true)
                    }
                };
                let live_after_swap = if from_live_swap {
                    HANDLES.read(token.token()).owned_filetables()
                } else {
                    live_owned_now
                };
                if let Some(ft) = Arc::into_inner(table_to_close) {
                    return Ok(Some((ft.into_inner(), from_live_swap, live_after_swap)));
                }
            }
            _ => (),
        }
        Ok(None)
    }
}

impl KernelScheme for ProcScheme {
    fn scheme_root(&self, token: &mut CleanLockToken) -> Result<usize> {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        HANDLES.write(token.token()).insert(
            id,
            Handle {
                // TODO: placeholder
                context: Arc::clone(&*context::current()),
                kind: ContextHandle::Authority,
            },
        );
        Ok(id)
    }

    fn fevent(
        &self,
        id: usize,
        _flags: EventFlags,
        token: &mut CleanLockToken,
    ) -> Result<EventFlags> {
        let handles = HANDLES.read(token.token());
        let _handle = handles.get(&id).ok_or(Error::new(EBADF))?;

        Ok(EventFlags::empty())
    }

    fn close(&self, id: usize, token: &mut CleanLockToken) -> Result<()> {
        if let Some((ft, _, live_owned)) = Self::close_nonrecursive(id, token)? {
            close_filetable_iteratively(ft, live_owned, token);
        }
        Ok(())
    }
    fn kfmap(
        &self,
        id: usize,
        dst_addr_space: &Arc<AddrSpaceWrapper>,
        map: &crate::syscall::data::Map,
        consume: bool,
        token: &mut CleanLockToken,
    ) -> Result<usize> {
        let handle = HANDLES
            .read(token.token())
            .get(&id)
            .ok_or(Error::new(EBADF))?
            .clone();
        let Handle { kind, ref context } = handle;

        match kind {
            ContextHandle::AddrSpace { ref addrspace } => {
                if Arc::ptr_eq(addrspace, dst_addr_space) {
                    return Err(Error::new(EBUSY));
                }

                let PageSpan {
                    base: requested_dst_page,
                    ..
                } = crate::syscall::validate_region(map.address, map.size)?;
                let src_span =
                    PageSpan::validate_nonempty(VirtualAddress::new(map.offset), map.size)
                        .ok_or(Error::new(EINVAL))?;

                let fixed = map.flags.contains(MapFlags::MAP_FIXED)
                    || map.flags.contains(MapFlags::MAP_FIXED_NOREPLACE);
                let requested_dst_base = (map.address != 0 || fixed).then_some(requested_dst_page);

                let mut src_addr_space_guard = addrspace.acquire_write(token.downgrade());
                let (src_addr_space, lock_token) = src_addr_space_guard.token_split();

                let src_page_count = NonZeroUsize::new(src_span.count).ok_or(Error::new(EINVAL))?;

                let mut notify_files = UnmapVec::new();

                // TODO: Validate flags
                let result_base = if consume {
                    dst_addr_space.r#move(
                        Some((addrspace, &mut *src_addr_space)),
                        src_span,
                        requested_dst_base,
                        src_page_count.get(),
                        map.flags,
                        &mut notify_files,
                        lock_token,
                    )
                } else {
                    // SAFETY: We've compared  Arc::ptr_eq(addrspace, dst_addr_space) before
                    let mut dst_addrsp_guard =
                        unsafe { dst_addr_space.acquire_rewrite(lock_token) };
                    dst_addrsp_guard.mmap(
                        dst_addr_space,
                        requested_dst_base,
                        src_page_count,
                        map.flags,
                        &mut notify_files,
                        |dst_page, _, dst_mapper, flusher| {
                            Grant::borrow(
                                Arc::clone(addrspace),
                                src_addr_space,
                                src_span.base,
                                dst_page,
                                src_span.count,
                                map.flags,
                                dst_mapper,
                                flusher,
                                true,
                                true,
                                false,
                            )
                        },
                    )
                };

                drop(src_addr_space_guard);

                handle_notify_files(notify_files, token);

                Ok(result_base?.start_address().data())
            }
            ContextHandle::Sighandler => {
                let context = context.read(token.token());
                // let (context, token) = context.token_split();
                let sig = context.sig.as_ref().ok_or(Error::new(EBADF))?;
                let frame = match map.offset {
                    // tctl
                    0 => &sig.thread_control,
                    // pctl
                    PAGE_SIZE => &sig.proc_control,
                    _ => return Err(Error::new(EINVAL)),
                };
                // TODO: Allocated or AllocatedShared?
                let addrsp = AddrSpace::current()?;
                // TODO: Lock ordering violation
                let mut token = unsafe { CleanLockToken::new() };
                let page = addrsp.acquire_write(token.downgrade()).mmap_anywhere(
                    &addrsp,
                    NonZeroUsize::new(1).unwrap(),
                    MapFlags::PROT_READ | MapFlags::PROT_WRITE,
                    |page, flags, mapper, flusher| {
                        Grant::allocated_shared_one_page(
                            frame.get(),
                            page,
                            flags,
                            mapper,
                            flusher,
                            false,
                        )
                    },
                )?;
                Ok(page.start_address().data())
            }
            _ => Err(Error::new(EBADF)),
        }
    }
    fn kreadoff(
        &self,
        id: usize,
        buf: UserSliceWo,
        offset: u64,
        _read_flags: u32,
        _stored_flags: u32,
        token: &mut CleanLockToken,
    ) -> Result<usize> {
        // Don't hold a global lock during the context switch later on
        let handle = {
            let handles = HANDLES.read(token.token());
            handles.get(&id).ok_or(Error::new(EBADF))?.clone()
        };

        let Handle { context, kind } = handle;
        kind.kreadoff(id, context, buf, offset, token)
    }
    fn kcall(
        &self,
        fds: &[usize],
        payload: UserSliceRw,
        flags: CallFlags,
        metadata: &[u64],
        token: &mut CleanLockToken,
    ) -> Result<usize> {
        if fds.len() != 1 {
            return Err(Error::new(EINVAL));
        }
        let id = fds[0];
        // TODO: simplify
        let handle = {
            let handles = HANDLES.read(token.token());
            let handle = handles.get(&id).ok_or(Error::new(EBADF))?;
            handle.clone()
        };

        handle
            .kind
            .kcall(fds, payload, flags, metadata, &handle.context, token)
    }
    fn kwriteoff(
        &self,
        id: usize,
        buf: UserSliceRo,
        _offset: u64,
        _fcntl_flags: u32,
        _stored_flags: u32,
        token: &mut CleanLockToken,
    ) -> Result<usize> {
        // TODO: offset

        // Don't hold a global lock during the context switch later on
        let handle = {
            let handles = HANDLES.read(token.token());
            let handle = handles.get(&id).ok_or(Error::new(EBADF))?;
            handle.clone()
        };

        let Handle { context, kind } = handle;
        kind.kwriteoff(id, context, buf, token)
    }

    fn kfpath(&self, _id: usize, buf: UserSliceWo, _token: &mut CleanLockToken) -> Result<usize> {
        //TODO: construct useful path?
        buf.copy_common_bytes_from_slice("/scheme/kernel.proc/".as_bytes())
    }

    fn kfstat(&self, id: usize, buffer: UserSliceWo, token: &mut CleanLockToken) -> Result<()> {
        let stat = {
            let handles = HANDLES.read(token.token());
            let handle = handles.get(&id).ok_or(Error::new(EBADF))?;

            Stat {
                st_mode: MODE_FILE | 0o666,
                st_size: handle.fsize()?,

                ..Stat::default()
            }
        };

        buffer.copy_exactly(&stat)?;

        Ok(())
    }

    fn fsize(&self, id: usize, token: &mut CleanLockToken) -> Result<u64> {
        let handles = HANDLES.read(token.token());
        let handle = handles.get(&id).ok_or(Error::new(EBADF))?;

        handle.fsize()
    }

    /// Dup is currently used to implement clone() and execve().
    fn kdup(
        &self,
        old_id: usize,
        raw_buf: UserSliceRo,
        _: CallerCtx,
        token: &mut CleanLockToken,
    ) -> Result<OpenResult> {
        let info = {
            let handles = HANDLES.read(token.token());
            let handle = handles.get(&old_id).ok_or(Error::new(EBADF))?;

            handle.clone()
        };

        let handle = |h, positioned| {
            (
                h,
                if positioned {
                    InternalFlags::POSITIONED
                } else {
                    InternalFlags::empty()
                },
            )
        };
        let mut array = [0_u8; 64];
        if raw_buf.len() > array.len() {
            return Err(Error::new(EINVAL));
        }
        raw_buf.copy_to_slice(&mut array[..raw_buf.len()])?;
        let buf = &array[..raw_buf.len()];

        new_handle(
            match info {
                Handle {
                    kind: ContextHandle::Authority,
                    ..
                } => {
                    return self
                        .open_inner(
                            OpenTy::Auth,
                            Some(core::str::from_utf8(buf).map_err(|_| Error::new(EINVAL))?)
                                .filter(|s| !s.is_empty()),
                            O_RDWR,
                            token,
                        )
                        .map(|(r, fl)| OpenResult::SchemeLocal(r, fl))
                }
                Handle {
                    kind: ContextHandle::OpenViaDup,
                    context,
                } => {
                    return self
                        .open_inner(
                            OpenTy::Ctxt(context),
                            Some(core::str::from_utf8(buf).map_err(|_| Error::new(EINVAL))?)
                                .filter(|s| !s.is_empty()),
                            O_RDWR,
                            token,
                        )
                        .map(|(r, fl)| OpenResult::SchemeLocal(r, fl));
                }

                Handle {
                    kind:
                        ContextHandle::Filetable {
                            ref filetable,
                            binary_format,
                            ref data,
                        },
                    context,
                } => {
                    // TODO: Maybe allow userspace to either copy or transfer recently dupped file
                    // descriptors between file tables.
                    let new_handle = match buf {
                        b"copy" => {
                            let filetable = filetable.upgrade().ok_or(Error::new(EOWNERDEAD))?;

                            let new_filetable =
                                Arc::new(RwLock::new(filetable.read(token.token()).try_clone()?));

                            Handle {
                                kind: ContextHandle::NewFiletable {
                                    filetable: new_filetable,
                                    binary_format,
                                    data: data.clone(),
                                },
                                context,
                            }
                        }
                        b"refresh" => {
                            let filetable = filetable.upgrade().ok_or(Error::new(EOWNERDEAD))?;

                            let new_data = if binary_format {
                                let mut data = Vec::new();
                                for index in filetable
                                    .read(token.token())
                                    .enumerate_fds()
                                    .map(|(i, _)| i)
                                {
                                    data.extend((index as u64).to_le_bytes());
                                }
                                data.into_boxed_slice()
                            } else {
                                use core::fmt::Write;
                                let mut data = String::new();
                                for index in filetable
                                    .read(token.token())
                                    .enumerate_fds()
                                    .map(|(i, _)| i)
                                {
                                    writeln!(data, "{}", index).unwrap();
                                }
                                data.into_bytes().into_boxed_slice()
                            };
                            Handle {
                                kind: ContextHandle::Filetable {
                                    filetable: Arc::downgrade(&filetable),
                                    binary_format,
                                    data: new_data,
                                },
                                context,
                            }
                        }
                        _ => return Err(Error::new(EINVAL)),
                    };
                    handle(new_handle, true)
                }
                Handle {
                    kind: ContextHandle::AddrSpace { ref addrspace },
                    context,
                } => {
                    const GRANT_FD_PREFIX: &[u8] = b"grant-fd-";

                    let kind = match buf {
                        // TODO: Better way to obtain new empty address spaces, perhaps using SYS_OPEN. But
                        // in that case, what scheme?
                        b"empty" => ContextHandle::AddrSpace {
                            addrspace: AddrSpaceWrapper::new()?,
                        },
                        b"exclusive" => ContextHandle::AddrSpace {
                            addrspace: addrspace.try_clone(token)?,
                        },
                        b"mmap-min-addr" => ContextHandle::MmapMinAddr(Arc::clone(addrspace)),

                        _ if buf.starts_with(GRANT_FD_PREFIX) => {
                            let string = core::str::from_utf8(&buf[GRANT_FD_PREFIX.len()..])
                                .map_err(|_| Error::new(EINVAL))?;
                            let page_addr = usize::from_str_radix(string, 16)
                                .map_err(|_| Error::new(EINVAL))?;

                            if page_addr % PAGE_SIZE != 0 {
                                return Err(Error::new(EINVAL));
                            }

                            let page = Page::containing_address(VirtualAddress::new(page_addr));

                            let mut token = token.token();
                            let read_lock = addrspace.acquire_read(token.downgrade());
                            let (_, info) =
                                read_lock.grants.contains(page).ok_or(Error::new(EINVAL))?;
                            return Ok(OpenResult::External(
                                info.file_ref()
                                    .map(|r| Arc::clone(&r.description))
                                    .ok_or(Error::new(EBADF))?,
                            ));
                        }

                        _ => return Err(Error::new(EINVAL)),
                    };

                    handle(Handle { context, kind }, true)
                }
                _ => return Err(Error::new(EINVAL)),
            },
            token,
        )
        .map(|(r, fl)| OpenResult::SchemeLocal(r, fl))
    }
    fn kfdwrite(
        &self,
        id: usize,
        descs: Vec<Arc<context::file::LockedFileDescription>>,
        _flags: CallFlags,
        metadata: &[u64],
        token: &mut CleanLockToken,
    ) -> Result<usize> {
        let context = {
            let mut handles = HANDLES.read(token.token());
            let (handles, _token) = handles.token_split();
            let handle = handles.get(&id).unwrap();

            let Handle { context, kind } = handle;

            if let ContextHandle::Filetable { .. } | ContextHandle::NewFiletable { .. } = &kind {
                context.clone()
            } else {
                return Err(Error::new(EBADF));
            }
        };

        let target_fd = metadata.first().ok_or(Error::new(EINVAL))?;
        let file = descs.first().ok_or(Error::new(EINVAL))?;
        let target_handle =
            FileHandle(usize::try_from(*target_fd).map_err(|_| Error::new(EBADFD))?);

        if let Some(old_file) = {
            let mut context = context.read(token.token());
            let (context, mut token) = context.token_split();
            context.remove_file(target_handle, &mut token.token())
        } {
            old_file.close(token)?;
        }

        let mut context = context.write(token.token());
        let (context, mut token) = context.token_split();
        context
            .insert_file(
                target_handle,
                context::file::FileDescriptor {
                    description: file.clone(),
                },
                &mut token,
            )
            .ok_or(Error::new(EMFILE))?;

        Ok(0)
    }
}
fn extract_scheme_number(fd: usize, token: &mut CleanLockToken) -> Result<(KernelSchemes, usize)> {
    let current_lock = context::current();
    let mut current = current_lock.read(token.token());
    let (context, mut token) = current.token_split();
    let file_descriptor = context
        .get_file(FileHandle::from(fd), &mut token)
        .ok_or(Error::new(EBADF))?;

    let desc = file_descriptor.description.read(token.token());
    Ok((desc.scheme_ref.upgrade()?, desc.number))
}
fn verify_scheme(scheme: KernelSchemes) -> Result<()> {
    if !matches!(scheme, KernelSchemes::Global(GlobalSchemes::Proc)) {
        return Err(Error::new(EBADF));
    }
    Ok(())
}
impl Handle {
    fn fsize(&self) -> Result<u64> {
        match self.kind {
            ContextHandle::Filetable { ref data, .. }
            | ContextHandle::NewFiletable { ref data, .. } => Ok(data.len() as u64),
            _ => Ok(0),
        }
    }
}
impl ContextHandle {
    fn kwriteoff(
        self,
        id: usize,
        context: Arc<ContextLock>,
        buf: UserSliceRo,
        token: &mut CleanLockToken,
    ) -> Result<usize> {
        match self {
            Self::AddrSpace { addrspace } => {
                let mut chunks = buf.usizes();
                let mut words_read = 0;
                let mut next = || {
                    words_read += 1;
                    chunks.next().ok_or(Error::new(EINVAL))
                };

                match next()?? {
                    op @ ADDRSPACE_OP_MMAP | op @ ADDRSPACE_OP_TRANSFER => {
                        let fd = next()??;
                        let offset = next()??;
                        let page_span = crate::syscall::validate_region(next()??, next()??)?;
                        let flags = MapFlags::from_bits(next()??).ok_or(Error::new(EINVAL))?;

                        if fd == !0 {
                            if op == ADDRSPACE_OP_TRANSFER {
                                return Err(Error::new(EOPNOTSUPP));
                            }

                            return MemoryScheme::fmap_anonymous(
                                &addrspace,
                                &Map {
                                    offset,
                                    size: page_span.count * PAGE_SIZE,
                                    address: page_span.base.start_address().data(),
                                    flags,
                                },
                                false,
                                MemoryType::Writeback,
                                token,
                            );
                        } else {
                            let (scheme, number) = extract_scheme_number(fd, token)?;

                            // ADDRSPACE_OP_MMAP and ADDRSPACE_OP_TRANSFER return the target address
                            // rather than the amount of written bytes.
                            // FIXME maybe make all these operations calls rather than writes?
                            return scheme.kfmap(
                                number,
                                &addrspace,
                                &Map {
                                    offset,
                                    size: page_span.count * PAGE_SIZE,
                                    address: page_span.base.start_address().data(),
                                    flags,
                                },
                                op == ADDRSPACE_OP_TRANSFER,
                                token,
                            );
                        }
                    }
                    ADDRSPACE_OP_MUNMAP => {
                        let page_span = crate::syscall::validate_region(next()??, next()??)?;

                        let unpin = false;
                        let res = addrspace.munmap(page_span, unpin, token)?;
                        handle_notify_files(res, token);
                    }
                    ADDRSPACE_OP_MPROTECT => {
                        let page_span = crate::syscall::validate_region(next()??, next()??)?;
                        let flags = MapFlags::from_bits(next()??).ok_or(Error::new(EINVAL))?;

                        addrspace.mprotect(page_span, flags, token)?;
                    }
                    _ => return Err(Error::new(EINVAL)),
                }
                Ok(words_read * size_of::<usize>())
            }
            ContextHandle::Regs(kind) => match kind {
                RegsKind::Float => {
                    let regs = unsafe { buf.read_exact::<FloatRegisters>()? };

                    try_stop_context(context, token, |context, _| {
                        // NOTE: The kernel will never touch floats

                        // Ignore the rare case of floating point
                        // registers being uninitiated
                        context.set_fx_regs(regs);

                        Ok(size_of::<FloatRegisters>())
                    })
                }
                RegsKind::Int => {
                    let regs = unsafe { buf.read_exact::<IntRegisters>()? };

                    try_stop_context(context, token, |context, _| match context.regs_mut() {
                        None => {
                            println!(
                                "{}:{}: Couldn't read registers from stopped process",
                                file!(),
                                line!()
                            );
                            Err(Error::new(ENOTRECOVERABLE))
                        }
                        Some(stack) => {
                            stack.load(&regs);

                            Ok(size_of::<IntRegisters>())
                        }
                    })
                }
                RegsKind::Env => {
                    let regs = unsafe { buf.read_exact::<EnvRegisters>()? };
                    write_env_regs(context, regs, token)?;
                    Ok(size_of::<EnvRegisters>())
                }
            },
            ContextHandle::Sighandler => {
                let data = unsafe { buf.read_exact::<SetSighandlerData>()? };

                if data.user_handler >= crate::USER_END_OFFSET
                    || data.excp_handler >= crate::USER_END_OFFSET
                {
                    return Err(Error::new(EPERM));
                }
                if data.thread_control_addr >= crate::USER_END_OFFSET
                    || data.proc_control_addr >= crate::USER_END_OFFSET
                {
                    return Err(Error::new(EFAULT));
                }

                let state = if data.thread_control_addr != 0 && data.proc_control_addr != 0 {
                    let validate_off = |addr, sz| {
                        let off: usize = addr % PAGE_SIZE;
                        if off.is_multiple_of(align_of::<usize>()) && off + sz <= PAGE_SIZE {
                            Ok(off as u16)
                        } else {
                            Err(Error::new(EINVAL))
                        }
                    };

                    let addrsp = Arc::clone(context.read(token.token()).addr_space()?);

                    Some(SignalState {
                        threadctl_off: validate_off(
                            data.thread_control_addr,
                            size_of::<Sigcontrol>(),
                        )?,
                        procctl_off: validate_off(
                            data.proc_control_addr,
                            size_of::<SigProcControl>(),
                        )?,
                        user_handler: NonZeroUsize::new(data.user_handler)
                            .ok_or(Error::new(EINVAL))?,
                        excp_handler: NonZeroUsize::new(data.excp_handler),
                        thread_control: addrsp.borrow_frame_enforce_rw_allocated(
                            Page::containing_address(VirtualAddress::new(data.thread_control_addr)),
                            token,
                        )?,
                        proc_control: addrsp.borrow_frame_enforce_rw_allocated(
                            Page::containing_address(VirtualAddress::new(data.proc_control_addr)),
                            token,
                        )?,
                    })
                } else {
                    None
                };

                context.write(token.token()).sig = state;

                Ok(size_of::<SetSighandlerData>())
            }
            ContextHandle::ForeignCall => {
                // Three words: the entry, then the start and end of the native range. Three zeros
                // unregister. The registration only moves where this thread's own `syscall`
                // instructions resume, so it grants nothing, and no other thread may set it.
                if !cfg!(target_arch = "x86_64") {
                    return Err(Error::new(EOPNOTSUPP));
                }
                let mut words = buf.usizes();
                let mut next = || words.next().ok_or(Error::new(EINVAL))?;
                let (entry, start, end) = (next()?, next()?, next()?);
                if !Arc::ptr_eq(&context, &*crate::context::current()) {
                    return Err(Error::new(EPERM));
                }
                let registration = [entry, start, end];
                if registration != [0; 3]
                    && !(start < end && end <= crate::USER_END_OFFSET && (start..end).contains(&entry))
                {
                    return Err(Error::new(EINVAL));
                }
                // The context first, then this CPU's mirror: a switch in between copies the
                // context's value into the mirror.
                context.write(token.token()).foreign_call = registration;
                crate::percpu::PercpuBlock::current()
                    .foreign_call
                    .set(registration);
                Ok(3 * size_of::<usize>())
            }
            ContextHandle::Start => {
                let cpu_id = {
                    let mut guard = context.write(token.token());
                    match guard.status {
                        ref mut status @ Status::HardBlocked {
                            reason: HardBlockedReason::NotYetStarted,
                        } => {
                            *status = Status::Runnable;
                            guard.cpu_id
                        }
                        _ => return Err(Error::new(EINVAL)),
                    }
                };

                wakeup_context(&context, cpu_id, &mut token.downgrade());
                Ok(buf.len())
            }
            ContextHandle::Filetable { .. } | ContextHandle::NewFiletable { .. } => {
                Err(Error::new(EBADF))
            }

            ContextHandle::CurrentFiletable => {
                let filetable_fd = buf.read_usize()?;
                let (hopefully_this_scheme, number) = extract_scheme_number(filetable_fd, token)?;
                verify_scheme(hopefully_this_scheme)?;

                let mut handles = HANDLES.write(token.token());
                if handles.get(&id).is_none() {
                    return Err(Error::new(EBADF));
                }
                let (filetable, replacement) = match handles.get(&number).ok_or(Error::new(EBADF))?
                {
                    Handle {
                        kind: ContextHandle::Filetable { filetable, .. },
                        ..
                    } => (filetable.upgrade().ok_or(Error::new(EOWNERDEAD))?, None),
                    Handle {
                        kind:
                            ContextHandle::NewFiletable {
                                filetable,
                                binary_format,
                                data,
                            },
                        ..
                    } => (
                        Arc::clone(filetable),
                        Some(Handle {
                            kind: ContextHandle::Filetable {
                                filetable: Arc::downgrade(filetable),
                                binary_format: *binary_format,
                                data: data.clone(),
                            },
                            context: Arc::clone(&context),
                        }),
                    ),
                    _ => return Err(Error::new(EBADF)),
                };

                if let Some(updated) = replacement {
                    let prev = handles.replace(number, updated);
                    assert!(prev.is_some());
                }
                let prev = handles.replace(
                    id,
                    Handle {
                        kind: ContextHandle::AwaitingFiletableChange { new_ft: filetable },
                        context,
                    },
                );
                assert!(prev.is_some());

                Ok(size_of::<usize>())
            }
            ContextHandle::CurrentAddrSpace => {
                let mut iter = buf.usizes();
                let addrspace_fd = iter.next().ok_or(Error::new(EINVAL))??;
                let sp = iter.next().ok_or(Error::new(EINVAL))??;
                let ip = iter.next().ok_or(Error::new(EINVAL))??;
                let arg1 = iter.next().transpose()?;

                let (hopefully_this_scheme, number) = extract_scheme_number(addrspace_fd, token)?;
                verify_scheme(hopefully_this_scheme)?;

                let mut handles = HANDLES.write(token.token());
                let addrspace = match handles.get(&number).ok_or(Error::new(EBADF))? {
                    Handle {
                        kind: ContextHandle::AddrSpace { addrspace },
                        ..
                    } => Arc::clone(addrspace),
                    _ => return Err(Error::new(EBADF)),
                };

                handles
                    .replace(
                        id,
                        Handle {
                            context,
                            kind: Self::AwaitingAddrSpaceChange {
                                new: addrspace,
                                new_sp: sp,
                                new_ip: ip,
                                arg1,
                            },
                        },
                    )
                    .ok_or(Error::new(EBADF))?;

                let written = if arg1.is_some() {
                    4 * size_of::<usize>()
                } else {
                    3 * size_of::<usize>()
                };

                Ok(written)
            }
            Self::MmapMinAddr(ref addrspace) => {
                let val = buf.read_usize()?;
                if val % PAGE_SIZE != 0 || val > crate::USER_END_OFFSET {
                    return Err(Error::new(EINVAL));
                }
                let mut lock_token = token.token();
                addrspace.acquire_write(lock_token.downgrade()).mmap_min = val;
                Ok(size_of::<usize>())
            }
            Self::SchedAffinity => {
                let cpuset = unsafe { buf.read_exact::<CpuSet>()? };

                context
                    .write(token.token())
                    .sched_affinity
                    .override_from(cpuset.raw());

                Ok(size_of_val(&cpuset))
            }
            ContextHandle::Status { privileged } => {
                let mut args = buf.usizes();

                let user_data = args.next().ok_or(Error::new(EINVAL))??;

                let context_verb =
                    ContextVerb::try_from_raw(user_data).ok_or(Error::new(EINVAL))?;

                match context_verb {
                    // TODO: lwp_park/lwp_unpark for bypassing procmgr?
                    ContextVerb::Unstop | ContextVerb::Stop if !privileged => {
                        Err(Error::new(EPERM))
                    }
                    ContextVerb::Stop => {
                        let mut guard = context.write(token.token());

                        match guard.status {
                            Status::Dead { .. } => return Err(Error::new(EOWNERDEAD)),
                            Status::HardBlocked {
                                reason: HardBlockedReason::AwaitingMmap { .. },
                            } => return Ok(size_of::<usize>()),
                            _ => (),
                        }
                        guard.status = Status::HardBlocked {
                            reason: HardBlockedReason::Stopped,
                        };
                        // TODO: wait for context to be switched away from, and/or IPI?
                        Ok(size_of::<usize>())
                    }
                    ContextVerb::Unstop => {
                        let mut guard = context.write(token.token());

                        if let Status::HardBlocked {
                            reason: HardBlockedReason::Stopped,
                        } = guard.status
                        {
                            guard.status = Status::Runnable;
                            let cpu_id = guard.cpu_id;
                            drop(guard);
                            wakeup_context(&context, cpu_id, &mut token.downgrade());
                        }
                        Ok(size_of::<usize>())
                    }
                    ContextVerb::Interrupt => {
                        unblock_context(&context, &mut token.token().downgrade());
                        Ok(size_of::<usize>())
                    }
                    ContextVerb::ForceKill => {
                        if context::is_current(&context) {
                            //trace!("FORCEKILL SELF {} {}", context.read().debug_id, context.read().pid);
                            // The following functionality simplifies the cleanup step when detached threads
                            // terminate.
                            if let Some(post_unmap) = args.next() {
                                let base = post_unmap?;
                                let size = args.next().ok_or(Error::new(EINVAL))??;

                                if size > 0 {
                                    let addrsp = AddrSpace::current()?;
                                    let res = addrsp.munmap(
                                        PageSpan::validate_nonempty(
                                            VirtualAddress::new(base),
                                            size,
                                        )
                                        .ok_or(Error::new(EINVAL))?,
                                        false,
                                        token,
                                    )?;
                                    handle_notify_files(res, token);
                                }
                            }
                            drop(context);
                            crate::syscall::exit_this_context(None, token);
                        } else {
                            let cpu_id = {
                                let mut ctxt = context.write(token.token());
                                //trace!("FORCEKILL NONSELF={} {}, SELF={}", ctxt.debug_id, ctxt.pid, context::current().read().debug_id);
                                if ctxt.status.is_dead() {
                                    return Ok(size_of::<usize>());
                                }
                                ctxt.status = context::Status::Runnable;
                                ctxt.being_sigkilled = true;
                                ctxt.cpu_id
                            };
                            wakeup_context(&context, cpu_id, &mut token.downgrade());
                            Ok(size_of::<usize>())
                        }
                    }
                }
            }
            ContextHandle::Attr => {
                let info = unsafe { buf.read_exact::<ProcSchemeAttrs>()? };

                let len = info
                    .debug_name
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(info.debug_name.len())
                    .min(crate::context::context::CONTEXT_NAME_CAPAC);

                let mut guard = context.write(token.token());
                let debug_name = core::str::from_utf8(&info.debug_name[..len])
                    .map_err(|_| Error::new(EINVAL))?;
                guard.name.clear();
                guard.name.push_str(debug_name);

                guard.pid = info.pid as usize;
                guard.euid = info.euid;
                guard.egid = info.egid;
                guard.prio = (info.prio as usize).min(39);

                #[cfg(feature = "profiling")]
                {
                    let debug_id = guard.debug_id;
                    let debug_name = guard.name;
                    drop(guard);
                    crate::profiling::DBG_ID_MAP
                        .write(token.token())
                        .insert(debug_id, debug_name);
                }
                Ok(size_of::<ProcSchemeAttrs>())
            }
            ContextHandle::OpenViaDup => {
                let mut args = buf.usizes();

                let user_data = args.next().ok_or(Error::new(EINVAL))??;

                let context_verb =
                    ContextVerb::try_from_raw(user_data).ok_or(Error::new(EINVAL))?;

                match context_verb {
                    ContextVerb::ForceKill => {
                        if context::is_current(&context) {
                            //trace!("FORCEKILL SELF {} {}", context.read().debug_id, context.read().pid);
                            // The following functionality simplifies the cleanup step when detached threads
                            // terminate.
                            if let Some(post_unmap) = args.next() {
                                let base = post_unmap?;
                                let size = args.next().ok_or(Error::new(EINVAL))??;

                                if size > 0 {
                                    let addrsp = AddrSpace::current()?;
                                    let res = addrsp.munmap(
                                        PageSpan::validate_nonempty(
                                            VirtualAddress::new(base),
                                            size,
                                        )
                                        .ok_or(Error::new(EINVAL))?,
                                        false,
                                        token,
                                    )?;
                                    handle_notify_files(res, token);
                                }
                            }
                            drop(context);
                            crate::syscall::exit_this_context(None, token);
                        } else {
                            Err(Error::new(EPERM))
                        }
                    }
                    _ => Err(Error::new(EINVAL)),
                }
            }
            _ => Err(Error::new(EBADF)),
        }
    }
    fn kreadoff(
        &self,
        _id: usize,
        context: Arc<ContextLock>,
        buf: UserSliceWo,
        offset: u64,
        token: &mut CleanLockToken,
    ) -> Result<usize> {
        match self {
            ContextHandle::Regs(kind) => {
                union Output {
                    float: FloatRegisters,
                    int: IntRegisters,
                    env: EnvRegisters,
                }

                let (output, size) = match kind {
                    RegsKind::Float => {
                        let context = context.read(token.token());
                        // NOTE: The kernel will never touch floats

                        (
                            Output {
                                float: context.get_fx_regs(),
                            },
                            size_of::<FloatRegisters>(),
                        )
                    }
                    RegsKind::Int => {
                        try_stop_context(context, token, |context, _| match context.regs() {
                            None => {
                                assert!(!context.running, "try_stop_context is broken, clearly");
                                println!(
                                    "{}:{}: Couldn't read registers from stopped process",
                                    file!(),
                                    line!()
                                );
                                Err(Error::new(ENOTRECOVERABLE))
                            }
                            Some(stack) => {
                                let mut regs = IntRegisters::default();
                                stack.save(&mut regs);
                                Ok((Output { int: regs }, size_of::<IntRegisters>()))
                            }
                        })?
                    }
                    RegsKind::Env => (
                        Output {
                            env: read_env_regs(context, token)?,
                        },
                        size_of::<EnvRegisters>(),
                    ),
                };

                let src_buf =
                    unsafe { slice::from_raw_parts(&output as *const _ as *const u8, size) };

                buf.copy_common_bytes_from_slice(src_buf)
            }
            ContextHandle::AddrSpace { addrspace } => {
                let Ok(offset) = usize::try_from(offset) else {
                    return Ok(0);
                };
                let grants_to_skip = offset / size_of::<GrantDesc>();

                // Output a list of grant descriptors, sufficient to allow relibc's fork()
                // implementation to fmap MAP_SHARED grants.
                let mut grants_read = 0;

                let mut dst = [GrantDesc::default(); 16];

                let mut token = token.token();
                let addr_space = addrspace.acquire_read(token.downgrade());
                for (dst, (grant_base, grant_info)) in dst
                    .iter_mut()
                    .zip(addr_space.grants.iter().skip(grants_to_skip))
                {
                    *dst = GrantDesc {
                        base: grant_base.start_address().data(),
                        size: grant_info.page_count() * PAGE_SIZE,
                        flags: grant_info.grant_flags(),
                        // The !0 is not a sentinel value; the availability of `offset` is
                        // indicated by the GRANT_SCHEME flag.
                        offset: grant_info.file_ref().map_or(!0, |f| f.base_offset as u64),
                    };
                    grants_read += 1;
                }
                drop(addr_space);
                for (src, chunk) in dst
                    .iter()
                    .take(grants_read)
                    .zip(buf.in_exact_chunks(size_of::<GrantDesc>()))
                {
                    chunk.copy_exactly(src)?;
                }

                Ok(grants_read * size_of::<GrantDesc>())
            }

            ContextHandle::Filetable { data, .. } => read_from(buf, data, offset),
            ContextHandle::MmapMinAddr(addrspace) => {
                let mmap_min = {
                    let mut token = token.token();
                    let addr = addrspace.acquire_read(token.downgrade());
                    addr.mmap_min
                };
                buf.write_usize(mmap_min)?;
                Ok(size_of::<usize>())
            }
            ContextHandle::SchedAffinity => {
                let mask = syscall::CpuSet::from_raw_mask(
                    context.read(token.token()).sched_affinity.to_raw(),
                );

                buf.copy_exactly(mask.as_bytes())?;
                Ok(size_of_val(&mask))
            } // TODO: Replace write() with SYS_SENDFD?
            ContextHandle::Status { .. } => {
                let status = {
                    let (status, being_sigkilled) = {
                        let context = context.read(token.token());
                        (context.status.clone(), context.being_sigkilled)
                    };
                    match status {
                        Status::Runnable | Status::Dead { excp: None } if being_sigkilled => {
                            ContextStatus::ForceKilled
                        }
                        Status::Dead { excp: None } => ContextStatus::Dead,
                        Status::Dead { excp: Some(excp) } => {
                            let (status, payload) =
                                buf.split_at(size_of::<usize>()).ok_or(Error::new(EINVAL))?;
                            status.copy_from_slice(
                                &(ContextStatus::UnhandledExcp as usize).to_ne_bytes(),
                            )?;
                            let len = payload.copy_common_bytes_from_slice(&excp)?;
                            return Ok(size_of::<usize>() + len);
                        }
                        Status::Runnable => ContextStatus::Runnable,
                        Status::Blocked => ContextStatus::Blocked,
                        Status::HardBlocked {
                            reason: HardBlockedReason::NotYetStarted,
                        } => ContextStatus::NotYetStarted,
                        Status::HardBlocked {
                            reason: HardBlockedReason::Stopped,
                        } => ContextStatus::Stopped,
                        _ => ContextStatus::Other,
                    }
                };
                buf.copy_common_bytes_from_slice(&(status as usize).to_ne_bytes())
            }
            ContextHandle::Attr => {
                let mut debug_name = [0; 32];
                let (euid, egid, pid, name, prio) = {
                    let c = context.read(token.token());
                    (c.euid, c.egid, c.pid as u32, c.name, c.prio as u32)
                };
                let min = name.len().min(debug_name.len());
                debug_name[..min].copy_from_slice(&name.as_bytes()[..min]);
                buf.copy_common_bytes_from_slice(&ProcSchemeAttrs {
                    pid,
                    euid,
                    egid,
                    prio,
                    debug_name,
                })
            }
            ContextHandle::ForeignCall => {
                let words = context.read(token.token()).foreign_call;
                let mut bytes = [0_u8; 3 * size_of::<usize>()];
                for (chunk, word) in bytes.chunks_exact_mut(size_of::<usize>()).zip(words) {
                    chunk.copy_from_slice(&word.to_ne_bytes());
                }
                buf.copy_common_bytes_from_slice(&bytes)
            }
            ContextHandle::Sighandler => {
                let data = match context.read(token.token()).sig {
                    Some(ref sig) => SetSighandlerData {
                        excp_handler: sig.excp_handler.map_or(0, NonZeroUsize::get),
                        user_handler: sig.user_handler.get(),
                        proc_control_addr: sig.procctl_off.into(),
                        thread_control_addr: sig.threadctl_off.into(),
                    },
                    None => SetSighandlerData::default(),
                };
                buf.copy_common_bytes_from_slice(&data)
            }

            // TODO: Find a better way to switch address spaces, since they also require switching
            // the instruction and stack pointer. Maybe remove `<pid>/regs` altogether and replace it
            // with `<pid>/ctx`
            _ => Err(Error::new(EBADF)),
        }
    }
    pub fn kcall(
        &self,
        fds: &[usize],
        payload: UserSliceRw,
        flags: CallFlags,
        metadata: &[u64],
        context: &Arc<ContextLock>,
        token: &mut CleanLockToken,
    ) -> Result<usize> {
        match self {
            ContextHandle::OpenViaDup => {
                let verb: u8 = (*metadata.first().ok_or(Error::new(EINVAL))?)
                    .try_into()
                    .map_err(|_| Error::new(EINVAL))?;
                let verb = ProcSchemeVerb::try_from_raw(verb).ok_or(Error::new(EINVAL))?;

                // FerrionOS lockdown (S13): any thread could raise its I/O privilege here. Port
                // I/O now comes from the memory scheme's `port-io` handle, which only a process
                // holding the hardware right reaches.
                Err(Error::new(match verb {
                    ProcSchemeVerb::Iopl => EPERM,
                    _ => EINVAL,
                }))
            }
            ContextHandle::Filetable {
                filetable,
                binary_format,
                ..
            } => {
                let op = syscall::flag::FileTableVerb::try_from_raw(
                    u8::try_from(*metadata.first().ok_or(Error::new(EINVAL))?)
                        .map_err(|_| Error::new(EINVAL))?,
                )
                .ok_or(Error::new(EINVAL))?;

                match op {
                    FileTableVerb::Close => {
                        let files = filetable.upgrade().ok_or(Error::new(EOWNERDEAD))?;
                        let payload_chunks = payload.in_exact_chunks(size_of::<usize>());
                        let fds = payload_chunks
                            .map(|chunk| {
                                let fd = chunk.read_usize()?;
                                Ok(FileHandle::from(fd))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        let num_fds = fds.len();
                        let files_to_close = files.write(token.token()).bulk_remove_files(&fds)?;
                        files_to_close.into_iter().for_each(|file| {
                            let _ = file.close(token);
                        });
                        Ok(num_fds)
                    }
                    FileTableVerb::Dup2 | FileTableVerb::Move => {
                        let mut it = payload.usizes();
                        let old = FileHandle(it.next().ok_or(Error::new(EINVAL)).flatten()?);
                        let new = FileHandle(it.next().ok_or(Error::new(EINVAL)).flatten()?);
                        if it.next().is_some() {
                            return Err(Error::new(EINVAL));
                        }

                        if old == new {
                            return Ok(0);
                        }
                        let file = {
                            let mut context = context.read(token.token());
                            let (context, mut token) = context.token_split();
                            context.remove_file(new, &mut token)
                        };
                        if let Some(file) = file {
                            file.close(token)?;
                        }

                        let mut context = context.read(token.token());
                        let (context, mut token) = context.token_split();
                        let file = match op {
                            FileTableVerb::Move => context.remove_file(old, &mut token),
                            _ => context.get_file(old, &mut token),
                        }
                        .ok_or(Error::new(EBADF))?;
                        context
                            .insert_file(new, file, &mut token)
                            .ok_or(Error::new(EMFILE))?;

                        Ok(0)
                    }
                    FileTableVerb::Resize => {
                        let files = filetable.upgrade().ok_or(Error::new(EOWNERDEAD))?;
                        let Some(&[which, size]) = metadata.get(1..3) else {
                            return Err(Error::new(EINVAL));
                        };
                        files
                            .write(token.token())
                            .resize(which as usize, size as usize)?;
                        Ok(size as usize)
                    }
                    FileTableVerb::Refresh => {
                        let filetable = filetable.upgrade().ok_or(Error::new(EOWNERDEAD))?;

                        let new_data = if *binary_format {
                            let mut data = Vec::new();
                            for index in filetable
                                .read(token.token())
                                .enumerate_fds()
                                .map(|(i, _)| i)
                            {
                                data.extend((index as u64).to_le_bytes());
                            }
                            data.into_boxed_slice()
                        } else {
                            use core::fmt::Write;
                            let mut data = String::new();
                            for index in filetable
                                .read(token.token())
                                .enumerate_fds()
                                .map(|(i, _)| i)
                            {
                                writeln!(data, "{}", index).unwrap();
                            }
                            data.into_bytes().into_boxed_slice()
                        };

                        let mut handles = HANDLES.write(token.token());
                        handles
                            .set_filetable_data(fds[0], new_data)
                            .ok_or(Error::new(EBADF))?;

                        Ok(0)
                    }
                    FileTableVerb::Swap => {
                        let mut it = payload.usizes();
                        let fd1 = FileHandle(it.next().ok_or(Error::new(EINVAL)).flatten()?);
                        let fd2 = FileHandle(it.next().ok_or(Error::new(EINVAL)).flatten()?);
                        if it.next().is_some() {
                            return Err(Error::new(EINVAL));
                        }

                        if fd1 == fd2 {
                            return Ok(0);
                        }
                        let mut context = context.read(token.token());
                        let (context, mut token) = context.token_split();
                        let file1_opt = context.remove_file(fd1, &mut token);
                        let file2_opt = context.remove_file(fd2, &mut token);
                        let (Some(file1), Some(file2)) = (file1_opt, file2_opt) else {
                            return Err(Error::new(EBADF));
                        };
                        context
                            .insert_file(fd1, file2, &mut token)
                            .expect("already removed, must succeed");
                        context
                            .insert_file(fd2, file1, &mut token)
                            .expect("already removed, must succeed");
                        Ok(0)
                    }

                    _ => Err(Error::new(EOPNOTSUPP)),
                }
            }
            ContextHandle::AddrSpace { addrspace } => {
                let op = syscall::flag::NumaVerb::try_from_raw(
                    *metadata.first().ok_or(Error::new(EINVAL))?,
                )
                .ok_or(Error::new(EINVAL))?;

                let NumaVerb::MemPolicy = op;

                if !numa::is_supported() {
                    return Err(Error::new(EOPNOTSUPP));
                }

                if flags.contains(CallFlags::WRITE) {
                    let mem_policy =
                        NumaMemoryPolicy::try_from(unsafe { payload.read_exact::<u64>()? })?;
                    let mut addrspace = addrspace.acquire_write(token.downgrade());
                    addrspace.table.utable.allocator_mut().0 = mem_policy;
                }
                if flags.contains(CallFlags::READ) {
                    let addrspace = addrspace.acquire_read(token.downgrade());
                    let mem_policy = (addrspace.table.utable.allocator().0 as u32).to_ne_bytes();
                    drop(addrspace);
                    payload.copy_from_slice(&mem_policy)?;
                }
                Ok(0)
            }
            ContextHandle::Attr => {
                let verb = *metadata.first().ok_or(Error::new(EINVAL))?;
                if verb != ATTR_CALL_SET_RUID {
                    return Err(Error::new(EINVAL));
                }
                let raw_ruid = *metadata.get(1).ok_or(Error::new(EINVAL))?;
                let ruid = u32::try_from(raw_ruid).map_err(|_| Error::new(EINVAL))?;
                context.write(token.token()).ruid = ruid;
                Ok(0)
            }
            _ => Err(Error::new(EBADF)),
        }
    }
}

fn write_env_regs(
    context: Arc<ContextLock>,
    regs: EnvRegisters,
    token: &mut CleanLockToken,
) -> Result<()> {
    if context::is_current(&context) {
        context::current()
            .write(token.token())
            .write_current_env_regs(regs)
    } else {
        try_stop_context(context, token, |context, _| context.write_env_regs(regs))
    }
}

fn read_env_regs(context: Arc<ContextLock>, token: &mut CleanLockToken) -> Result<EnvRegisters> {
    if context::is_current(&context) {
        context::current()
            .read(token.token())
            .read_current_env_regs()
    } else {
        try_stop_context(context, token, |context, _| context.read_env_regs())
    }
}
