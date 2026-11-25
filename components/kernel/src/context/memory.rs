use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use arrayvec::ArrayVec;
use core::{
    cmp,
    fmt::Debug,
    mem::ManuallyDrop,
    num::NonZeroUsize,
    ops::{Bound, Deref},
    sync::atomic::{AtomicU32, Ordering},
};
use rmm::{Arch as _, FrameAllocator, FrameCount, PageFlush};
use smallvec::SmallVec;
use syscall::{error::*, flag::MapFlags, GrantFlags, MunmapFlags, NumaMemoryPolicy};

use crate::{
    context::file::LockedFileDescription,
    cpu_set::LogicalCpuSet,
    memory::{
        deallocate_contiguous, deallocate_frame, get_page_info, init_frame, the_zeroed_frame,
        AddRefError, Enomem, Frame, Page, PageFlags, PageInfo, PageMapper, RaiiFrame, RefCount,
        RefKind, RmmA, TableKind, TheFrameAllocator, VirtualAddress, PAGE_SIZE,
    },
    percpu::PercpuBlock,
    scheme::KernelSchemes,
    sync::{
        CleanLockToken, LockToken, RwLock, RwLockReadGuard, RwLockUpgradableGuard,
        RwLockWriteGuard, L4, L5,
    },
};

use super::context::HardBlockedReason;

pub const MMAP_MIN_DEFAULT: usize = PAGE_SIZE;

fn defer_user_table_free(table_phys: rmm::PhysicalAddress, flusher: &mut impl GenericFlusher) {
    let table_frame = Frame::containing(table_phys);
    let info = get_page_info(table_frame).expect("page-table frame must have PageInfo");
    assert!(
        info.promote_unreferenced_to_one(),
        "page-table frame must be allocated and unreferenced"
    );
    flusher.queue(table_frame, None, TlbShootdownActions::FREE);
}

fn prot_accessible(flags: MapFlags) -> bool {
    flags.intersects(MapFlags::PROT_READ | MapFlags::PROT_WRITE | MapFlags::PROT_EXEC)
}
pub fn page_flags(flags: MapFlags) -> PageFlags<RmmA> {
    PageFlags::new()
        .user(prot_accessible(flags))
        .execute(flags.contains(MapFlags::PROT_EXEC))
        .write(flags.contains(MapFlags::PROT_WRITE))
}
pub fn map_flags(page_flags: PageFlags<RmmA>) -> MapFlags {
    if !page_flags.has_user() {
        return MapFlags::empty();
    }
    let mut flags = MapFlags::PROT_READ;
    if page_flags.has_write() {
        flags |= MapFlags::PROT_WRITE;
    }
    if page_flags.has_execute() {
        flags |= MapFlags::PROT_EXEC;
    }
    flags
}

pub struct UnmapResult {
    pub file_desc: Option<GrantFileRef>,
    pub size: usize,
    pub flags: MunmapFlags,
}
pub type UnmapVec = SmallVec<[UnmapResult; 16]>;

impl UnmapResult {
    pub fn unmap(mut self, token: &mut CleanLockToken) -> Result<()> {
        let Some(GrantFileRef {
            base_offset,
            description,
        }) = self.file_desc.take()
        else {
            return Ok(());
        };

        // TODO: This is not ideal, the lock must be held until try_close(), however that would break borrowing rules.
        // Proper unmap operation would be a recursive operation, since closing a file can trigger another unmap().
        // We should refactor Result of munmap() to handle unmap and closing files recursively.
        let (scheme_res, number) = {
            let desc = description.write(token.token());
            (desc.scheme_ref.upgrade(), desc.number)
        };

        let funmap_result = scheme_res
            .and_then(|scheme| scheme.kfunmap(number, base_offset, self.size, self.flags, token));

        if let Ok(fd) = Arc::try_unwrap(description) {
            fd.into_inner().try_close(token)?;
        }
        funmap_result?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct AddrSpaceWrapper {
    pub inner: RwLock<L5, AddrSpace>,
    pub tlb_ack: AtomicU32,
    pub used_by: LogicalCpuSet,
}
impl AddrSpaceWrapper {
    pub fn new() -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            inner: RwLock::new(AddrSpace::new()?),
            tlb_ack: AtomicU32::new(0),
            used_by: LogicalCpuSet::empty(),
        }))
    }
    pub fn acquire_read<'a>(
        &'a self,
        lock_token: LockToken<'a, L4>,
    ) -> RwLockReadGuard<'a, L5, AddrSpace> {
        self.inner.read(lock_token)
    }
    pub fn acquire_upgradeable_read<'a>(
        &'a self,
        lock_token: LockToken<'a, L4>,
    ) -> RwLockUpgradableGuard<'a, L5, AddrSpace> {
        self.inner.upgradeable_read(lock_token)
    }
    pub fn acquire_write<'a>(
        &'a self,
        lock_token: LockToken<'a, L4>,
    ) -> RwLockWriteGuard<'a, L5, AddrSpace> {
        self.inner.write(lock_token)
    }
    pub unsafe fn acquire_reupgradeable_read<'a>(
        &'a self,
        lock_token: LockToken<'a, L5>,
    ) -> RwLockUpgradableGuard<'a, L5, AddrSpace> {
        unsafe { self.inner.reupgradeable_read(lock_token) }
    }
    pub unsafe fn acquire_rewrite<'a>(
        &'a self,
        lock_token: LockToken<'a, L5>,
    ) -> RwLockWriteGuard<'a, L5, AddrSpace> {
        unsafe { self.inner.rewrite(lock_token) }
    }
    pub fn into_drop(self, token: &mut CleanLockToken) {
        self.inner.into_inner().into_drop(token);
    }
}

#[derive(Debug)]
pub struct AddrSpace {
    pub table: Table,
    pub grants: UserGrants,
    /// Lowest offset for mmap invocations where the user has not already specified the offset
    /// (using MAP_FIXED/MAP_FIXED_NOREPLACE). Cf. Linux's `/proc/sys/vm/mmap_min_addr`, but with
    /// the exception that we have a memory safe kernel which doesn't have to protect itself
    /// against null pointers, so fixed mmaps to address zero are still allowed.
    pub mmap_min: usize,
}
impl AddrSpaceWrapper {
    /// Attempt to clone an existing address space so that all mappings are copied (CoW).
    pub fn try_clone(&self, token: &mut CleanLockToken) -> Result<Arc<AddrSpaceWrapper>> {
        let mut token = token.token();
        let mut guard = self.acquire_write(token.downgrade());
        let guard = &mut *guard;

        let mut new_arc = AddrSpaceWrapper::new()?;

        let new =
            Arc::get_mut(&mut new_arc).expect("expected new address space Arc not to be aliased");

        let _this_mapper = &mut guard.table.utable;
        let this_mapper = &mut guard.table.utable;
        let mut this_flusher = Flusher::with_cpu_set(&self.used_by, &self.tlb_ack);

        for (grant_base, grant_info) in guard.grants.iter() {
            let new_grant = match grant_info.provider {
                // No, your temporary UserScheme mappings will not be kept across forks.
                Provider::External {
                    is_pinned_userscheme_borrow: true,
                    ..
                }
                | Provider::AllocatedShared {
                    is_pinned_userscheme_borrow: true,
                    ..
                } => continue,

                // No, physically contiguous driver memory won't either.
                Provider::Allocated {
                    phys_contiguous: true,
                    ..
                } => continue,

                Provider::PhysBorrowed { base } => Grant::physmap(
                    base,
                    PageSpan::new(grant_base, grant_info.page_count),
                    grant_info.flags,
                    &mut new.inner.get_mut().table.utable,
                    &mut NopFlusher,
                )?,
                Provider::Allocated {
                    ref cow_file_ref,
                    phys_contiguous: false,
                } => Grant::copy_mappings(
                    grant_base,
                    grant_base,
                    grant_info.page_count,
                    grant_info.flags,
                    this_mapper,
                    &mut new.inner.get_mut().table.utable,
                    &mut this_flusher,
                    &mut NopFlusher,
                    CopyMappingsMode::Owned {
                        cow_file_ref: cow_file_ref.clone(),
                    },
                )?,
                // TODO: Merge Allocated and AllocatedShared, and make CopyMappingsMode a field?
                Provider::AllocatedShared {
                    is_pinned_userscheme_borrow: false,
                } => Grant::copy_mappings(
                    grant_base,
                    grant_base,
                    grant_info.page_count,
                    grant_info.flags,
                    this_mapper,
                    &mut new.inner.get_mut().table.utable,
                    &mut this_flusher,
                    &mut NopFlusher,
                    CopyMappingsMode::Borrowed,
                )?,

                // MAP_SHARED grants are retained by reference, across address space clones (the
                // "fork" analogue from monolithic kernels).
                Provider::External {
                    ref address_space,
                    src_base,
                    ..
                } => Grant::borrow_grant(
                    Arc::clone(address_space),
                    src_base,
                    grant_base,
                    grant_info,
                    &mut new.inner.get_mut().table.utable,
                    &mut NopFlusher,
                    false,
                )?,
                Provider::FmapBorrowed { .. } => continue,
            };

            new.inner.get_mut().grants.insert(new_grant);
        }
        Ok(new_arc)
    }
    pub fn mprotect(
        &self,
        requested_span: PageSpan,
        flags: MapFlags,
        token: &mut CleanLockToken,
    ) -> Result<()> {
        let mut token = token.token();
        let mut guard = self.acquire_write(token.downgrade());
        let guard = &mut *guard;

        let mapper = &mut guard.table.utable;
        let mut flusher = Flusher::with_cpu_set(&self.used_by, &self.tlb_ack);

        // TODO: Remove allocation (might require BTreeMap::set_key or interior mutability).
        let regions = guard
            .grants
            .conflicts(requested_span)
            .map(|(base, info)| {
                if info.is_pinned() {
                    Err(Error::new(EBUSY))
                } else {
                    Ok(PageSpan::new(base, info.page_count))
                }
            })
            .collect::<Vec<_>>();

        for grant_span_res in regions {
            let grant_span = grant_span_res?;

            let grant = guard
                .grants
                .remove_containing(grant_span.base)
                .expect("grant cannot magically disappear while we hold the lock!");
            //info!("Mprotecting {:#?} to {:#?} in {:#?}", grant, flags, grant_span);
            let intersection = grant_span.intersection(requested_span);

            let (before, mut grant, after) = grant
                .extract(intersection)
                .expect("failed to extract grant");
            //info!("Sliced into\n\n{:#?}\n\n{:#?}\n\n{:#?}", before, grant, after);

            if let Some(before) = before {
                guard.grants.insert(before);
            }
            // FIXME: andypython: should this be done at the end?
            if let Some(after) = after {
                guard.grants.insert(after);
            }

            if !grant.info.can_have_flags(flags) {
                guard.grants.insert(grant);
                return Err(Error::new(EACCES));
            }

            let new_flags = grant
                .info
                .flags()
                // TODO: Require a capability in order to map executable memory?
                .user(prot_accessible(flags))
                .execute(flags.contains(MapFlags::PROT_EXEC))
                .write(flags.contains(MapFlags::PROT_WRITE));

            // TODO: Allow enabling/disabling read access on architectures which allow it. On
            // x86_64 with protection keys (although only enforced by userspace), and AArch64 (I
            // think), execute-only memory is also supported.

            grant.remap(mapper, &mut flusher, new_flags);
            //info!("Mprotect grant became {:#?}", grant);
            guard.grants.insert(grant);
        }
        Ok(())
    }
    #[must_use = "needs to notify files"]
    pub fn munmap(
        &self,
        requested_span: PageSpan,
        unpin: bool,
        token: &mut CleanLockToken,
    ) -> Result<UnmapVec> {
        let mut guard = self.acquire_write(token.downgrade());
        let guard = &mut *guard;

        let mut flusher = Flusher::with_cpu_set(&self.used_by, &self.tlb_ack);
        AddrSpace::munmap_inner(
            &mut guard.grants,
            &mut guard.table.utable,
            &mut flusher,
            requested_span,
            unpin,
        )
    }
    pub fn r#move(
        &self,
        src_opt: Option<(&AddrSpaceWrapper, &mut AddrSpace)>,
        src_span: PageSpan,
        requested_dst_base: Option<Page>,
        new_page_count: usize,
        new_flags: MapFlags,
        mut notify_files_out: Option<&mut UnmapVec>,
        token: LockToken<L5>,
    ) -> Result<Page> {
        let dst_lock = self;
        // SAFETY: This is moving data between two AddrSpace. Caller ensures the two is a different AddrSpace
        let mut dst = unsafe { dst_lock.acquire_rewrite(token) };
        let dst = &mut *dst;

        let mut src_flusher;
        let mut src_opt = match src_opt {
            Some((aw, a)) => {
                src_flusher = Flusher::with_cpu_set(&aw.used_by, &aw.tlb_ack);
                Some((&mut a.grants, &mut a.table.utable, &mut src_flusher))
            }
            None => None,
        };
        let mut dst_flusher = Flusher::with_cpu_set(&dst_lock.used_by, &dst_lock.tlb_ack);

        let dst_base = match requested_dst_base {
            Some(base) if new_flags.contains(MapFlags::MAP_FIXED_NOREPLACE) => {
                if dst
                    .grants
                    .conflicts(PageSpan::new(base, new_page_count))
                    .next()
                    .is_some()
                {
                    return Err(Error::new(EEXIST));
                }

                base
            }
            Some(base) if new_flags.contains(MapFlags::MAP_FIXED) => {
                let unpin = false;
                let notify_files = AddrSpace::munmap_inner(
                    &mut dst.grants,
                    &mut dst.table.utable,
                    &mut dst_flusher,
                    PageSpan::new(base, new_page_count),
                    unpin,
                )?;
                if let Some(notify_files_out) = notify_files_out.as_mut() {
                    notify_files_out.extend(notify_files);
                }

                base
            }
            _ => {
                dst.grants
                    .find_free(dst.mmap_min, cmp::max(new_page_count, src_span.count))
                    .ok_or(Error::new(ENOMEM))?
                    .base
            }
        };

        let (src_grants, src_mapper, src_flusher) = match &mut src_opt {
            Some((g, m, f)) => (&mut **g, &mut **m, &mut **f),
            None => (&mut dst.grants, &mut dst.table.utable, &mut dst_flusher),
        };

        if src_grants
            .conflicts(src_span)
            .any(|(_, g)| !g.can_extract(false))
        {
            return Err(Error::new(EBUSY));
        }
        if src_grants
            .conflicts(src_span)
            .any(|(_, g)| !g.can_have_flags(new_flags))
        {
            return Err(Error::new(EPERM));
        }
        if PageSpan::new(dst_base, new_page_count).intersects(src_span) {
            return Err(Error::new(EBUSY));
        }

        if new_page_count < src_span.count {
            let unpin = false;
            let notify_files = AddrSpace::munmap_inner(
                src_grants,
                src_mapper,
                src_flusher,
                PageSpan::new(
                    src_span.base.next_by(new_page_count),
                    src_span.count - new_page_count,
                ),
                unpin,
            )?;
            if let Some(notify_files_out) = notify_files_out.as_mut() {
                notify_files_out.extend(notify_files);
            }
        }

        let mut remaining_src_span =
            PageSpan::new(src_span.base, cmp::min(src_span.count, new_page_count));

        let to_remap = src_grants
            .conflicts(remaining_src_span)
            .map(|(b, _)| b)
            .collect::<Vec<_>>();

        let mut prev_grant_end = src_span.base;

        //while let Some(grant_base) = next(src_opt.as_mut().map(|s| &mut **s), dst, remaining_src_span) {
        for grant_base in to_remap {
            if prev_grant_end < grant_base {
                let hole_page_count = grant_base.offset_from(prev_grant_end);
                let hole_span = PageSpan::new(
                    dst_base.next_by(prev_grant_end.offset_from(src_span.base)),
                    hole_page_count,
                );
                dst.grants.insert(Grant::zeroed(
                    hole_span,
                    page_flags(new_flags),
                    &mut dst.table.utable,
                    &mut dst_flusher,
                    false,
                )?);
            }

            let src_grants = src_opt
                .as_mut()
                .map_or(&mut dst.grants, |(g, _, _)| &mut *g);
            let grant = src_grants
                .remove_containing(grant_base)
                .expect("grant cannot disappear");
            let grant_span = PageSpan::new(grant.base, grant.info.page_count());
            let (before, middle, after) = grant
                .extract(remaining_src_span.intersection(grant_span))
                .expect("called intersect(), must succeed");

            if let Some(before) = before {
                src_grants.insert(before);
            }
            if let Some(after) = after {
                src_grants.insert(after);
            }

            let dst_grant_base = dst_base.next_by(middle.base.offset_from(src_span.base));
            let middle_span = middle.span();

            dst.grants.insert(match src_opt.as_mut() {
                Some((_, other_mapper, other_flusher)) => middle.transfer(
                    dst_grant_base,
                    page_flags(new_flags),
                    other_mapper,
                    Some(&mut dst.table.utable),
                    other_flusher,
                    &mut dst_flusher,
                )?,
                None => middle.transfer(
                    dst_grant_base,
                    page_flags(new_flags),
                    &mut dst.table.utable,
                    None,
                    &mut dst_flusher,
                    &mut NopFlusher,
                )?,
            });

            prev_grant_end = middle_span.base.next_by(middle_span.count);
            let pages_advanced = prev_grant_end.offset_from(remaining_src_span.base);
            remaining_src_span =
                PageSpan::new(prev_grant_end, remaining_src_span.count - pages_advanced);
        }

        if prev_grant_end < src_span.base.next_by(new_page_count) {
            let last_hole_span = PageSpan::new(
                dst_base.next_by(prev_grant_end.offset_from(src_span.base)),
                new_page_count - prev_grant_end.offset_from(src_span.base),
            );
            dst.grants.insert(Grant::zeroed(
                last_hole_span,
                page_flags(new_flags),
                &mut dst.table.utable,
                &mut dst_flusher,
                false,
            )?);
        }

        Ok(dst_base)
    }
    /// Borrows a page from user memory, requiring that the frame be Allocated and read/write. This
    /// is intended to be used for user-kernel shared memory.
    pub fn borrow_frame_enforce_rw_allocated(
        self: &Arc<Self>,
        page: Page,
        token: &mut CleanLockToken,
    ) -> Result<RaiiFrame> {
        let mut lock_token = token.token();
        let guard = self.acquire_write(lock_token.downgrade());

        let (_start_page, info) = guard.grants.contains(page).ok_or(Error::new(EINVAL))?;

        if !info.can_have_flags(MapFlags::PROT_READ | MapFlags::PROT_WRITE) {
            return Err(Error::new(EPERM));
        }
        if !matches!(info.provider, Provider::Allocated { .. }) {
            return Err(Error::new(EPERM));
        }

        let mut guard_lock = None;
        let frame = if let Some((f, fl)) = guard.table.utable.translate(page.start_address())
            && fl.has_write()
        {
            Frame::containing(f)
        } else {
            let (frame, flush, new_guard) = correct_inner(self, guard, page, AccessMode::Write, 0)
                .map_err(|_| Error::new(ENOMEM))?;
            flush.flush();
            guard_lock = Some(new_guard);

            frame
        };

        let frame = match get_page_info(frame)
            .expect("missing page info for Allocated grant")
            .add_ref(RefKind::Shared)
        {
            Ok(_) => Ok(unsafe { RaiiFrame::new_unchecked(frame) }),
            Err(AddRefError::RcOverflow) => Err(Error::new(ENOMEM)),
            Err(AddRefError::SharedToCow) => unreachable!(),
            Err(AddRefError::CowToShared) => unreachable!(
                "if it was CoW, it was read-only, but in that case we already called correct_inner"
            ),
        };
        drop(guard_lock);

        frame
    }
}
impl AddrSpace {
    pub fn current() -> Result<Arc<AddrSpaceWrapper>> {
        PercpuBlock::current()
            .current_addrsp
            .borrow()
            .clone()
            .ok_or(Error::new(ESRCH))
    }

    pub fn new() -> Result<Self> {
        let utable = unsafe {
            PageMapper::create(
                TableKind::User,
                crate::memory::TheFrameAllocator(NumaMemoryPolicy::NodeLocalLeniant),
            )
            .ok_or(Error::new(ENOMEM))?
        };

        Ok(Self {
            grants: UserGrants::new(),
            table: Table { utable },
            mmap_min: MMAP_MIN_DEFAULT,
        })
    }
    fn munmap_inner(
        this_grants: &mut UserGrants,
        this_mapper: &mut PageMapper,
        this_flusher: &mut Flusher,
        mut requested_span: PageSpan,
        unpin: bool,
    ) -> Result<UnmapVec> {
        let mut notify_files = UnmapVec::new();

        let next = |grants: &mut UserGrants, span: PageSpan| {
            grants
                .conflicts(span)
                .map(|(base, info)| {
                    if info.is_pinned() && !unpin {
                        Err(Error::new(EBUSY))
                    } else if !info.can_extract(unpin) {
                        Err(Error::new(EINVAL))
                    } else {
                        Ok(PageSpan::new(base, info.page_count))
                    }
                })
                .next()
        };

        while let Some(conflicting_span_res) = next(this_grants, requested_span) {
            let conflicting_span = conflicting_span_res?;

            let mut grant = this_grants
                .remove_containing(conflicting_span.base)
                .expect("conflicting region didn't exist");
            if unpin {
                grant.info.unpin();
            }

            let intersection = conflicting_span.intersection(requested_span);

            requested_span = {
                // In the following diagrams [---> indicates a range of
                // base..base+count where the [ is at the base and > is at
                // base+count. In other words, the [ is part of the range and
                // the > is not part of the range.
                if conflicting_span.end() < requested_span.end() {
                    // [------>     conflicting_span
                    //    [-------> requested_span
                    //        [---> next requested_span
                    // or
                    //    [---->    conflicting_span
                    // [----------> requested_span
                    //         [--> next requested_span
                    PageSpan::new(
                        conflicting_span.end(),
                        requested_span.end().offset_from(conflicting_span.end()),
                    )
                } else {
                    // [----------> conflicting_span
                    //    [----->   requested_span
                    //              next requested_span
                    // or
                    //   [--------> conflicting_span
                    // [-------->   requested_span
                    //              next requested_span
                    PageSpan::empty()
                }
            };

            let (before, grant, after) = grant
                .extract(intersection)
                .expect("conflicting region shared no common parts");

            // Keep untouched regions
            if let Some(before) = before {
                this_grants.insert(before);
            }
            if let Some(after) = after {
                this_grants.insert(after);
            }

            // Remove irrelevant region
            // TODO: Lock ordering violation
            let mut token = unsafe { CleanLockToken::new() };
            let unmap_result = grant.unmap(this_mapper, this_flusher, &mut token);

            // Notify scheme that holds grant
            if unmap_result.file_desc.is_some() {
                notify_files.push(unmap_result);
            }
        }

        Ok(notify_files)
    }
    pub fn mmap_anywhere(
        &mut self,
        dst_lock: &AddrSpaceWrapper,
        page_count: NonZeroUsize,
        flags: MapFlags,
        map: impl FnOnce(Page, PageFlags<RmmA>, &mut PageMapper, &mut Flusher) -> Result<Grant>,
    ) -> Result<Page> {
        self.mmap(dst_lock, None, page_count, flags, None, map)
    }
    pub fn mmap(
        &mut self,
        dst_lock: &AddrSpaceWrapper,
        requested_base_opt: Option<Page>,
        page_count: NonZeroUsize,
        flags: MapFlags,
        notify_files_out: Option<&mut UnmapVec>,
        map: impl FnOnce(Page, PageFlags<RmmA>, &mut PageMapper, &mut Flusher) -> Result<Grant>,
    ) -> Result<Page> {
        assert_eq!(dst_lock.inner.as_mut_ptr(), self as *mut Self);

        let selected_span = match requested_base_opt {
            // TODO: Rename MAP_FIXED+MAP_FIXED_NOREPLACE to MAP_FIXED and
            // MAP_FIXED_REPLACE/MAP_REPLACE?
            Some(requested_base) => {
                let requested_span = PageSpan::new(requested_base, page_count.get());

                if flags.contains(MapFlags::MAP_FIXED_NOREPLACE) {
                    if self.grants.conflicts(requested_span).next().is_some() {
                        return Err(Error::new(EEXIST));
                    }
                    requested_span
                } else if flags.contains(MapFlags::MAP_FIXED) {
                    let unpin = false;
                    let mut notify_files = Self::munmap_inner(
                        &mut self.grants,
                        &mut self.table.utable,
                        &mut Flusher::with_cpu_set(&dst_lock.used_by, &dst_lock.tlb_ack),
                        requested_span,
                        unpin,
                    )?;
                    if let Some(notify_files_out) = notify_files_out {
                        notify_files_out.append(&mut notify_files);
                    }

                    requested_span
                } else {
                    self.grants
                        .find_free_near(self.mmap_min, page_count.get(), Some(requested_base))
                        .ok_or(Error::new(ENOMEM))?
                }
            }
            None => self
                .grants
                .find_free(self.mmap_min, page_count.get())
                .ok_or(Error::new(ENOMEM))?,
        };

        // TODO: Threads share address spaces, so not only the inactive flusher should be sending
        // out IPIs. IPIs will only be sent when downgrading mappings (i.e. when a stale TLB entry
        // will not be corrected by a page fault), and will furthermore require proper
        // synchronization.

        let grant = map(
            selected_span.base,
            page_flags(flags),
            &mut self.table.utable,
            &mut Flusher::with_cpu_set(&dst_lock.used_by, &dst_lock.tlb_ack),
        )?;
        self.grants.insert(grant);

        Ok(selected_span.base)
    }

    pub fn into_drop(self, token: &mut CleanLockToken) {
        let mut this = ManuallyDrop::new(self);
        this.inner_drop(token);
        unsafe {
            core::ptr::drop_in_place(&mut this.grants);
            core::ptr::drop_in_place(&mut this.table);
        }
    }

    fn inner_drop(&mut self, token: &mut CleanLockToken) {
        for mut grant in core::mem::take(&mut self.grants).into_iter() {
            // Unpinning the grant is allowed, because pinning only occurs in UserScheme calls to
            // prevent unmapping the mapped range twice (which would corrupt only the scheme
            // provider), but it won't be able to double free any range after this address space
            // has been dropped!
            grant.info.unpin();

            // TODO: Optimize away clearing the actual page tables? Since this address space is no
            // longer arc-rwlock wrapped, it cannot be referenced `External`ly by borrowing grants,
            // so it should suffice to iterate over PageInfos and decrement and maybe deallocate
            // the underlying pages (and send some funmaps).
            let res = { grant.unmap(&mut self.table.utable, &mut NopFlusher, token) };

            let _ = res.unmap(token);
        }
    }
}

pub struct AddrSpaceSwitchReadGuard {
    pub lock: RwLockReadGuard<'static, L5, AddrSpace>,
}

impl AddrSpaceSwitchReadGuard {
    /// # Safety
    ///
    /// Erases the lifetime so unsafe for obvious reasons.
    pub unsafe fn new(guard: RwLockReadGuard<'_, L5, AddrSpace>) -> Self {
        Self {
            // extend lifetime
            lock: unsafe {
                core::mem::transmute::<RwLockReadGuard<'_, _, _>, RwLockReadGuard<'_, _, _>>(guard)
            },
        }
    }
}
impl Deref for AddrSpaceSwitchReadGuard {
    type Target = RwLockReadGuard<'static, L5, AddrSpace>;

    fn deref(&self) -> &Self::Target {
        &self.lock
    }
}

#[derive(Debug)]
pub struct UserGrants {
    // Using a BTreeMap for its range method.
    inner: BTreeMap<Page, GrantInfo>,
    // Holes ordered by memory address for merging adjacent holes
    holes_by_addr: BTreeMap<VirtualAddress, usize>,
    // Holes ordered by size then start address for fast allocations
    holes_by_size: BTreeSet<(usize, VirtualAddress)>,
}

#[derive(Clone, Copy)]
pub struct PageSpan {
    pub base: Page,
    pub count: usize,
}
impl PageSpan {
    pub fn new(base: Page, count: usize) -> Self {
        Self { base, count }
    }
    pub fn empty() -> Self {
        Self {
            base: Page::containing_address(VirtualAddress::new(0)),
            count: 0,
        }
    }
    pub fn validate_nonempty(address: VirtualAddress, size: usize) -> Option<Self> {
        Self::validate(address, size).filter(|this| !this.is_empty())
    }
    pub fn validate(address: VirtualAddress, size: usize) -> Option<Self> {
        if !address.data().is_multiple_of(PAGE_SIZE) || !size.is_multiple_of(PAGE_SIZE) {
            return None;
        }
        if address.data().saturating_add(size) > crate::USER_END_OFFSET {
            return None;
        }

        Some(Self::new(
            Page::containing_address(address),
            size / PAGE_SIZE,
        ))
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn intersection(&self, with: PageSpan) -> PageSpan {
        Self::between(
            cmp::max(self.base, with.base),
            cmp::min(self.end(), with.end()),
        )
    }
    pub fn intersects(&self, with: PageSpan) -> bool {
        !self.intersection(with).is_empty()
    }
    pub fn slice(&self, inner_span: PageSpan) -> (Option<PageSpan>, PageSpan, Option<PageSpan>) {
        (self.before(inner_span), inner_span, self.after(inner_span))
    }
    pub fn pages(self) -> impl Iterator<Item = Page> {
        (0..self.count).map(move |i| self.base.next_by(i))
    }

    pub fn end(&self) -> Page {
        self.base.next_by(self.count)
    }

    /// Returns the span from the start of self until the start of the specified span.
    pub fn before(self, span: Self) -> Option<Self> {
        assert!(self.base <= span.base);
        Some(Self::between(self.base, span.base)).filter(|reg| !reg.is_empty())
    }

    /// Returns the span from the end of the given span until the end of self.
    pub fn after(self, span: Self) -> Option<Self> {
        assert!(span.end() <= self.end());
        Some(Self::between(span.end(), self.end())).filter(|reg| !reg.is_empty())
    }
    /// Returns the span between two pages, `[start, end)`, truncating to zero if end < start.
    pub fn between(start: Page, end: Page) -> Self {
        Self::new(
            start,
            end.start_address()
                .data()
                .saturating_sub(start.start_address().data())
                / PAGE_SIZE,
        )
    }
}

impl Default for UserGrants {
    fn default() -> Self {
        Self::new()
    }
}
impl Debug for PageSpan {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "[{:p}:{:p}, {} pages]",
            self.base.start_address().data() as *const u8,
            self.base
                .start_address()
                .add(self.count * PAGE_SIZE - 1)
                .data() as *const u8,
            self.count
        )
    }
}

impl UserGrants {
    pub fn new() -> Self {
        let mut holes_by_addr = BTreeMap::new();
        let mut holes_by_size = BTreeSet::new();

        let initial_offset = VirtualAddress::new(0);
        let initial_size = crate::USER_END_OFFSET;

        holes_by_addr.insert(initial_offset, initial_size);
        holes_by_size.insert((initial_size, initial_offset));

        Self {
            inner: BTreeMap::new(),
            holes_by_addr,
            holes_by_size,
        }
    }
    /// Internal helper to keep the two hole maps in sync
    fn insert_hole(&mut self, offset: VirtualAddress, size: usize) {
        self.holes_by_addr.insert(offset, size);
        self.holes_by_size.insert((size, offset));
    }
    /// Internal helper to keep the two hole maps in sync
    fn remove_hole(&mut self, offset: &VirtualAddress) -> Option<usize> {
        if let Some(size) = self.holes_by_addr.remove(offset) {
            self.holes_by_size.remove(&(size, *offset));
            Some(size)
        } else {
            None
        }
    }
    /// Internal helper to keep the two hole maps in sync
    fn resize_hole(&mut self, offset: &VirtualAddress, new_size: usize) {
        if let Some(size) = self.holes_by_addr.get_mut(offset) {
            self.holes_by_size.remove(&(*size, *offset));
            *size = new_size;
            self.holes_by_size.insert((new_size, *offset));
        }
    }

    /// Returns the grant, if any, which occupies the specified page
    pub fn contains(&self, page: Page) -> Option<(Page, &GrantInfo)> {
        self.inner
            .range(..=page)
            .next_back()
            .filter(|(base, info)| (**base..base.next_by(info.page_count)).contains(&page))
            .map(|(base, info)| (*base, info))
    }

    /// Returns an iterator over all grants that occupy some part of the
    /// requested region
    pub fn conflicts(&self, span: PageSpan) -> impl Iterator<Item = (Page, &'_ GrantInfo)> + '_ {
        let start = self.contains(span.base);

        // If there is a grant that contains the base page, start searching at the base of that
        // grant, rather than the requested base here.
        let start_span = start
            .map(|(base, info)| PageSpan::new(base, info.page_count))
            .unwrap_or(span);

        self.inner
            .range(start_span.base..)
            .take_while(move |(base, info)| PageSpan::new(**base, info.page_count).intersects(span))
            .map(|(base, info)| (*base, info))
    }
    // TODO: DEDUPLICATE CODE!
    pub fn conflicts_mut(
        &mut self,
        span: PageSpan,
    ) -> impl Iterator<Item = (Page, &'_ mut GrantInfo)> + '_ {
        let start = self.contains(span.base);

        // If there is a grant that contains the base page, start searching at the base of that
        // grant, rather than the requested base here.
        let start_span = start
            .map(|(base, info)| PageSpan::new(base, info.page_count))
            .unwrap_or(span);

        self.inner
            .range_mut(start_span.base..)
            .take_while(move |(base, info)| PageSpan::new(**base, info.page_count).intersects(span))
            .map(|(base, info)| (*base, info))
    }
    /// Return a free region with the specified size
    // TODO: Alignment (x86_64: 4 KiB, 2 MiB, or 1 GiB).
    // TODO: Support finding grant close to a requested address?
    pub fn find_free_near(
        &self,
        min: usize,
        page_count: usize,
        _near: Option<Page>,
    ) -> Option<PageSpan> {
        // Get first available hole, but do reserve the page starting from zero as most compiled
        // languages cannot handle null pointers safely even if they point to valid memory. If an
        // application absolutely needs to map the 0th page, they will have to do so explicitly via
        // MAP_FIXED/MAP_FIXED_NOREPLACE.
        // TODO: Allow explicitly allocating guard pages? Perhaps using mprotect or mmap with
        // PROT_NONE?

        let req_size = page_count * PAGE_SIZE;
        let (_, hole_start) = self
            .holes_by_size
            .range((req_size, VirtualAddress::new(0))..)
            .find(|&&(hole_size, hole_offset)| {
                // A hole might be large enough, but the usable
                // portion above `min` address might be too small.
                let usable_start = cmp::max(hole_offset.data(), min);
                let hole_end = hole_offset.data() + hole_size;
                usable_start + req_size <= hole_end
            })?;
        // Create new region
        Some(PageSpan::new(
            Page::containing_address(VirtualAddress::new(cmp::max(hole_start.data(), min))),
            page_count,
        ))
    }
    pub fn find_free(&self, min: usize, page_count: usize) -> Option<PageSpan> {
        self.find_free_near(min, page_count, None)
    }
    fn reserve(&mut self, base: Page, page_count: usize) {
        let start_address = base.start_address();
        let size = page_count * PAGE_SIZE;
        let end_address = base.start_address().add(size);

        let previous_hole = self
            .holes_by_addr
            .range(..start_address)
            .next_back()
            .map(|(&k, &v)| (k, v));

        if let Some((hole_offset, hole_size)) = previous_hole {
            let prev_hole_end = hole_offset.data() + hole_size;

            // Note that prev_hole_end cannot exactly equal start_address, since that would imply
            // there is another grant at that position already, as it would otherwise have been
            // larger.

            if prev_hole_end > start_address.data() {
                // hole_offset must be below (but never equal to) the start address due to the
                // `..start_address()` limit; hence, all we have to do is to shrink the
                // previous offset.
                self.resize_hole(&hole_offset, start_address.data() - hole_offset.data());
            }
            if prev_hole_end > end_address.data() {
                // The grant is splitting this hole in two, so insert the new one at the end.
                self.insert_hole(end_address, prev_hole_end - end_address.data());
            }
        }

        // Next hole
        if let Some(hole_size) = self.remove_hole(&start_address) {
            let remainder = hole_size - size;
            if remainder > 0 {
                self.insert_hole(end_address, remainder);
            }
        }
    }

    fn unreserve(&mut self, base: Page, page_count: usize) {
        let start_address = base.start_address();
        let size = page_count * PAGE_SIZE;
        let end_address = base.start_address().add(size);

        // The size of any possible hole directly after the to-be-freed region.
        let exactly_after_size = self.remove_hole(&end_address);
        // There was a range that began exactly prior to the to-be-freed region, so simply
        // increment the size such that it occupies the grant too. If in addition there was a grant
        // directly after the grant, include it too in the size.
        if let Some((hole_offset, _hole_size)) = self
            .holes_by_addr
            .range(..start_address)
            .next_back()
            .filter(|(offset, size)| offset.data() + **size == start_address.data())
            .map(|(&offset, &size)| (offset, size))
        {
            self.resize_hole(
                &hole_offset,
                end_address.data() - hole_offset.data() + exactly_after_size.unwrap_or(0),
            );
        } else {
            // There was no free region directly before the to-be-freed region, however will
            // now unconditionally insert a new free region where the grant was, and add that extra
            // size if there was something after it.
            self.insert_hole(start_address, size + exactly_after_size.unwrap_or(0));
        }
    }
    pub fn insert(&mut self, mut grant: Grant) {
        assert!(self
            .conflicts(PageSpan::new(grant.base, grant.info.page_count))
            .next()
            .is_none());
        self.reserve(grant.base, grant.info.page_count);

        let before_region = self
            .inner
            .range(..grant.base)
            .next_back()
            .filter(|(base, info)| {
                base.next_by(info.page_count) == grant.base
                    && info.can_be_merged_if_adjacent(&grant.info)
            })
            .map(|(base, info)| (*base, info.page_count));

        let after_region = self
            .inner
            .range(grant.span().end()..)
            .next()
            .filter(|(base, info)| {
                **base == grant.base.next_by(grant.info.page_count)
                    && info.can_be_merged_if_adjacent(&grant.info)
            })
            .map(|(base, info)| (*base, info.page_count));

        if let Some((before_base, before_page_count)) = before_region {
            grant.base = before_base;
            grant.info.page_count += before_page_count;

            core::mem::forget(self.inner.remove(&before_base));
        }
        if let Some((after_base, after_page_count)) = after_region {
            grant.info.page_count += after_page_count;

            core::mem::forget(self.inner.remove(&after_base));
        }

        self.inner.insert(grant.base, grant.info);
    }

    pub fn remove_containing(&mut self, page: Page) -> Option<Grant> {
        // Points to the gap *after* the greatest grant smaller than or equal to `page`.
        let mut cursor = self.inner.upper_bound_mut(Bound::Included(&page));
        let (&base, info) = cursor.peek_prev()?;

        if (base..base.next_by(info.page_count())).contains(&page) {
            let (base, info) = cursor.remove_prev().unwrap();
            self.unreserve(base, info.page_count());
            Some(Grant { base, info })
        } else {
            None
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (Page, &GrantInfo)> + '_ {
        self.inner.iter().map(|(base, info)| (*base, info))
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    pub fn into_iter(self) -> impl Iterator<Item = Grant> {
        self.inner
            .into_iter()
            .map(|(base, info)| Grant { base, info })
    }
}

#[derive(Debug)]
pub struct GrantInfo {
    page_count: usize,
    flags: PageFlags<RmmA>,
    // TODO: Rename to unmapped?
    mapped: bool,
    pub(crate) provider: Provider,
}

/// Enumeration of various types of grants.
#[derive(Debug)]
pub enum Provider {
    /// The grant is owned, but possibly CoW-shared.
    ///
    /// The pages this grant spans, need not necessarily be initialized right away, and can be
    /// populated either from zeroed frames, the CoW zeroed frame, or from a scheme fmap call, if
    /// mapped with MAP_LAZY. All frames must have an available PageInfo.
    Allocated {
        cow_file_ref: Option<GrantFileRef>,
        phys_contiguous: bool,
    },

    /// The grant is owned, but possibly shared.
    ///
    /// The pages may only be lazily initialized, if the address space has not yet been cloned (when forking).
    ///
    /// This type of grants is obtained from MAP_SHARED anonymous or `memory:` mappings, i.e.
    /// allocated memory that remains shared after address space clones.
    AllocatedShared { is_pinned_userscheme_borrow: bool },

    /// The grant is not owned, but borrowed from physical memory frames that do not belong to the
    /// frame allocator. The kernel will forbid borrowing any physical memory range, that the
    /// memory map has indicated is regular allocatable RAM.
    PhysBorrowed { base: Frame },

    /// The memory is borrowed directly from another address space.
    External {
        address_space: Arc<AddrSpaceWrapper>,
        src_base: Page,
        is_pinned_userscheme_borrow: bool,
    },

    /// The memory is MAP_SHARED borrowed from a scheme.
    ///
    /// Since the address space is not tracked here, all nonpresent pages must be present before
    /// the fmap operation completes, unless MAP_LAZY is specified. They are tracked using
    /// PageInfo, or treated as PhysBorrowed if any frame lacks a PageInfo.
    FmapBorrowed {
        file_ref: GrantFileRef,
        pin_refcount: usize,
        is_cow: bool,
    },
}

#[derive(Debug)]
pub struct Grant {
    pub(crate) base: Page,
    pub(crate) info: GrantInfo,
}

#[derive(Clone, Debug)]
pub struct GrantFileRef {
    pub description: Arc<LockedFileDescription>,
    pub base_offset: usize,
}

impl Grant {
    // TODO: PageCount newtype, to avoid confusion between bytes and pages?

    // `base` must be mapped by the caller.
    pub fn allocated_one_page_nomap(base: Page, flags: PageFlags<RmmA>) -> Grant {
        Grant {
            base,
            info: GrantInfo {
                page_count: 1,
                flags,
                mapped: true,
                provider: Provider::Allocated {
                    cow_file_ref: None,
                    phys_contiguous: false,
                },
            },
        }
    }

    // TODO: is_pinned
    pub fn allocated_shared_one_page(
        frame: Frame,
        page: Page,
        flags: PageFlags<RmmA>,
        mapper: &mut PageMapper,
        flusher: &mut Flusher,
        is_pinned: bool,
    ) -> Result<Grant> {
        let info = get_page_info(frame).expect("needs page info");

        // TODO:
        //
        // This may not necessarily hold, as even pinned memory can remain shared (e.g. proc:
        // borrow), but it would probably be possible to forbid borrowing memory there as well.
        // Maybe make it exclusive first using cow(), unless that is too expensive.
        //
        // assert_eq!(info.refcount(), RefCount::One);

        // Semantically, the page will be shared between the "context struct" and whatever
        // else.
        info.add_ref(RefKind::Shared)
            .expect("must be possible if previously Zero");

        unsafe {
            mapper
                .map_phys(page.start_address(), frame.base(), flags, 0)
                .ok_or(Error::new(ENOMEM))?
                .ignore();

            flusher.queue(frame, None, TlbShootdownActions::NEW_MAPPING);
        }

        Ok(Grant {
            base: page,
            info: GrantInfo {
                page_count: 1,
                flags,
                mapped: true,
                provider: Provider::AllocatedShared {
                    is_pinned_userscheme_borrow: is_pinned,
                },
            },
        })
    }

    pub fn physmap(
        phys: Frame,
        span: PageSpan,
        flags: PageFlags<RmmA>,
        mapper: &mut PageMapper,
        flusher: &mut impl GenericFlusher,
    ) -> Result<Grant> {
        const MAX_EAGER_PAGES: usize = 4096;

        for i in 0..span.count {
            if let Some(info) = get_page_info(phys.next_by(i)) {
                warn!("Driver tried to physmap the allocator-frame {phys:?} (info {info:?})!");
                return Err(Error::new(EPERM));
            }
        }

        for (i, page) in span.pages().enumerate().take(MAX_EAGER_PAGES) {
            let frame = phys.next_by(i);
            unsafe {
                let Some(result) =
                    mapper.map_phys(page.start_address(), frame.base(), flags.write(false), 0)
                else {
                    break;
                };
                result.ignore();

                flusher.queue(frame, None, TlbShootdownActions::NEW_MAPPING);
            }
        }

        Ok(Grant {
            base: span.base,
            info: GrantInfo {
                page_count: span.count,
                flags,
                mapped: true,
                provider: Provider::PhysBorrowed { base: phys },
            },
        })
    }
    pub fn zeroed_phys_contiguous(
        span: PageSpan,
        flags: PageFlags<RmmA>,
        mapper: &mut PageMapper,
        flusher: &mut Flusher,
        mem_policy: NumaMemoryPolicy,
    ) -> Result<Grant, Enomem> {
        let mut frame_allocator = TheFrameAllocator(mem_policy);
        let base = Frame::containing(
            frame_allocator
                .allocate(FrameCount::new(span.count))
                .ok_or(Enomem)?,
        );

        for (i, page) in span.pages().enumerate() {
            let frame = base.next_by(i);

            get_page_info(frame)
                .expect("PageInfo must exist for allocated frame")
                .refcount
                .store(RefCount::One.to_raw(), Ordering::Relaxed);

            unsafe {
                let result = mapper
                    .map_phys(page.start_address(), frame.base(), flags, 0)
                    .expect("TODO: page table OOM");
                result.ignore();

                flusher.queue(frame, None, TlbShootdownActions::NEW_MAPPING);
            }
        }

        Ok(Grant {
            base: span.base,
            info: GrantInfo {
                page_count: span.count,
                flags,
                mapped: true,
                provider: Provider::Allocated {
                    cow_file_ref: None,
                    phys_contiguous: true,
                },
            },
        })
    }
    pub fn zeroed(
        span: PageSpan,
        flags: PageFlags<RmmA>,
        mapper: &mut PageMapper,
        flusher: &mut Flusher,
        shared: bool,
    ) -> Result<Grant, Enomem> {
        const MAX_EAGER_PAGES: usize = 16;

        if flags.has_user() {
            let (the_frame, the_frame_info) = the_zeroed_frame();

            for page in span.pages().take(MAX_EAGER_PAGES) {
                unsafe {
                    the_frame_info
                        .add_ref(RefKind::Cow)
                        .expect("the static zeroed frame cannot be shared!");

                    let Some(result) = mapper.map_phys(
                        page.start_address(),
                        the_frame.base(),
                        flags.write(false),
                        0,
                    ) else {
                        break;
                    };
                    result.ignore();
                    flusher.queue(the_frame, None, TlbShootdownActions::NEW_MAPPING);
                }
            }
        }

        Ok(Grant {
            base: span.base,
            info: GrantInfo {
                page_count: span.count,
                flags,
                mapped: true,
                provider: if shared {
                    Provider::AllocatedShared {
                        is_pinned_userscheme_borrow: false,
                    }
                } else {
                    Provider::Allocated {
                        cow_file_ref: None,
                        phys_contiguous: false,
                    }
                },
            },
        })
    }

    // XXX: borrow_grant is needed because of the borrow checker (iterator invalidation), maybe
    // borrow_grant/borrow can be abstracted somehow?
    pub fn borrow_grant(
        src_address_space_lock: Arc<AddrSpaceWrapper>,
        src_base: Page,
        dst_base: Page,
        src_info: &GrantInfo,
        _mapper: &mut PageMapper,
        _dst_flusher: &mut impl GenericFlusher,
        _eager: bool,
    ) -> Result<Grant, Enomem> {
        Ok(Grant {
            base: dst_base,
            info: GrantInfo {
                page_count: src_info.page_count,
                flags: src_info.flags,
                mapped: true,
                provider: Provider::External {
                    src_base,
                    address_space: src_address_space_lock,
                    is_pinned_userscheme_borrow: false,
                },
            },
        })
    }

    pub fn borrow_fmap(
        span: PageSpan,
        new_flags: PageFlags<RmmA>,
        file_ref: GrantFileRef,
        src: Option<BorrowedFmapSource<'_>>,
        _lock: &AddrSpaceWrapper,
        mapper: &mut PageMapper,
        flusher: &mut Flusher,
    ) -> Result<Self> {
        let is_cow = src
            .as_ref()
            .is_some_and(|s| matches!(s.mode, MmapMode::Cow));
        if let Some(src) = src {
            let mut guard = src.addr_space_guard;
            let mut src_addrspace = &mut *guard;
            let mut src_flusher_state =
                Flusher::with_cpu_set(&src.addr_space_lock.used_by, &src.addr_space_lock.tlb_ack)
                    .detach();
            for dst_page in span.pages() {
                let src_page = src.src_base.next_by(dst_page.offset_from(span.base));

                let (frame, page_flags, is_cow) = match src.mode {
                    MmapMode::Shared => {
                        // TODO: Error code for "scheme responded with unmapped page"?
                        let (frame, page_flags) = match src_addrspace
                            .table
                            .utable
                            .translate(src_page.start_address())
                        {
                            Some((phys, page_flags)) => (Frame::containing(phys), page_flags),
                            // TODO: ensure the correct context is hardblocked, if necessary
                            None => {
                                let (frame, _, new_guard) = correct_inner(
                                    src.addr_space_lock,
                                    guard,
                                    src_page,
                                    AccessMode::Read,
                                    0,
                                )
                                .map_err(|_| Error::new(EIO))?;
                                let page_flags = new_guard
                                    .table
                                    .utable
                                    .translate(src_page.start_address())
                                    .unwrap()
                                    .1;
                                guard = new_guard;
                                (frame, page_flags)
                            }
                        };

                        (frame, page_flags, false)
                    }
                    MmapMode::Cow => unsafe {
                        let (frame, page_flags) = match guard
                            .table
                            .utable
                            .remap_with(src_page.start_address(), |flags| flags.write(false))
                        {
                            Some((page_flags, phys, _)) => (Frame::containing(phys), page_flags),
                            // TODO: ensure the correct context is hardblocked, if necessary
                            None => {
                                let (frame, _, new_guard) = correct_inner(
                                    src.addr_space_lock,
                                    guard,
                                    src_page,
                                    AccessMode::Read,
                                    0,
                                )
                                .map_err(|_| Error::new(EIO))?;
                                // FIXME correct_inner should read the page flags instead
                                let page_flags = new_guard
                                    .table
                                    .utable
                                    .translate(src_page.start_address())
                                    .unwrap()
                                    .1;
                                guard = new_guard;
                                (frame, page_flags)
                            }
                        };

                        (frame, page_flags, true)
                    },
                };
                src_addrspace = &mut *guard;

                let frame = if let Some(page_info) = get_page_info(frame) {
                    match page_info.add_ref(RefKind::Shared) {
                        Ok(()) => frame,
                        Err(AddRefError::CowToShared) => unsafe {
                            let CowResult {
                                new_frame: new_cow_frame,
                                old_frame,
                            } = cow(frame, page_info, RefKind::Shared)
                                .map_err(|_| Error::new(ENOMEM))?;

                            let (old_flags, _, _flush) = src_addrspace
                                .table
                                .utable
                                .remap_with_full(src_page.start_address(), |_, flags| {
                                    Some((new_cow_frame.base(), flags))
                                })
                                .expect("page did exist");

                            // TODO: flush.ignore() is correct, but seems to be amplifying a
                            // userspace race condition
                            //
                            //flush.ignore();

                            let mut src_flusher = Flusher::reattach(
                                &src.addr_space_lock.used_by,
                                src_flusher_state,
                            );
                            src_flusher.queue(
                                frame,
                                None,
                                TlbShootdownActions::change_of_flags(old_flags, new_flags),
                            );

                            if let Some(old_frame) = old_frame {
                                src_flusher.queue(old_frame, None, TlbShootdownActions::FREE);
                            }
                            src_flusher_state = src_flusher.detach();

                            // TODO: there used to be an additional remove_ref here, was that
                            // correct?

                            new_cow_frame
                        },
                        Err(AddRefError::SharedToCow) => unreachable!(),
                        Err(AddRefError::RcOverflow) => return Err(Error::new(ENOMEM)),
                    }
                } else {
                    frame
                };

                unsafe {
                    let flush = mapper
                        .map_phys(
                            dst_page.start_address(),
                            frame.base(),
                            new_flags
                                .write(new_flags.has_write() && !is_cow)
                                // FIXME make sure this stays in sync with the MemoryType flags
                                .uncacheable(page_flags.has_flag(RmmA::ENTRY_FLAG_UNCACHEABLE))
                                .device_memory(page_flags.has_flag(RmmA::ENTRY_FLAG_DEVICE_MEMORY))
                                .write_combining(
                                    page_flags.has_flag(RmmA::ENTRY_FLAG_WRITE_COMBINING),
                                ),
                            0,
                        )
                        .unwrap();
                    flush.ignore();

                    flusher.queue(frame, None, TlbShootdownActions::NEW_MAPPING);
                }
            }
        }

        Ok(Self {
            base: span.base,
            info: GrantInfo {
                page_count: span.count,
                mapped: true,
                flags: new_flags,
                provider: Provider::FmapBorrowed {
                    file_ref,
                    pin_refcount: 0,
                    is_cow,
                },
            },
        })
    }

    /// Borrow all pages in the range `[src_base, src_base+page_count)` from `src_address_space`,
    /// mapping them into `[dst_base, dst_base+page_count)`. The destination pages will lazily read
    /// the page tables of the source pages, but once present in the destination address space,
    /// pages that are unmaped or moved will not be made visible to the destination address space.
    pub fn borrow(
        src_address_space_lock: Arc<AddrSpaceWrapper>,
        src_address_space: &mut AddrSpace,
        src_base: Page,
        dst_base: Page,
        page_count: usize,
        map_flags: MapFlags,
        dst_mapper: &mut PageMapper,
        dst_flusher: &mut Flusher,
        eager: bool,
        _allow_phys: bool,
        is_pinned_userscheme_borrow: bool,
    ) -> Result<Grant> {
        let flags = page_flags(map_flags);

        const MAX_EAGER_PAGES: usize = 4096;

        let src_span = PageSpan::new(src_base, page_count);
        let mut prev_span = None;
        let mut src_has_cow_fmap = false;

        for (src_grant_base, src_grant) in src_address_space.grants.conflicts(src_span) {
            let grant_span = PageSpan::new(src_grant_base, src_grant.page_count);
            let prev_span = prev_span.replace(grant_span);

            if prev_span.is_none() && src_grant_base > src_base {
                warn!(
                    "Grant too far away, prev_span {:?} src_base {:?} grant base {:?} grant {:#?}",
                    prev_span, src_base, src_grant_base, src_grant
                );
                return Err(Error::new(EINVAL));
            } else if let Some(prev) = prev_span
                && prev.end() != src_grant_base
            {
                warn!(
                    "Hole between grants, prev_span {:?} src_base {:?} grant base {:?} grant {:#?}",
                    prev_span, src_base, src_grant_base, src_grant
                );
                return Err(Error::new(EINVAL));
            }

            if !src_grant.can_have_flags(map_flags) {
                return Err(Error::new(EPERM));
            }

            if matches!(
                src_grant.provider,
                Provider::FmapBorrowed { is_cow: true, .. }
            ) {
                src_has_cow_fmap = true;
            }
        }

        let Some(last_span) = prev_span else {
            warn!("Called Grant::borrow, but no grants were there!");
            return Err(Error::new(EINVAL));
        };

        if last_span.end() < src_span.end() {
            warn!("Requested end page too far away from last grant");
            return Err(Error::new(EINVAL));
        }
        if eager {
            let mut src_flusher = (flags.has_write() && src_has_cow_fmap).then(|| {
                Flusher::with_cpu_set(
                    &src_address_space_lock.used_by,
                    &src_address_space_lock.tlb_ack,
                )
            });

            for (i, page) in PageSpan::new(src_base, page_count)
                .pages()
                .enumerate()
                .take(MAX_EAGER_PAGES)
            {
                let Some((phys, src_pte_flags)) = src_address_space
                    .table
                    .utable
                    .translate(page.start_address())
                else {
                    continue;
                };

                if flags.has_write() && src_has_cow_fmap && !src_pte_flags.has_write() {
                    let cow_grant_flags =
                        src_address_space.grants.contains(page).and_then(|(_, g)| {
                            matches!(g.provider, Provider::FmapBorrowed { is_cow: true, .. })
                                .then_some(g.flags)
                        });
                    if let Some(src_grant_flags) = cow_grant_flags {
                        assert!(src_grant_flags.has_write());
                        let src_frame = Frame::containing(phys);
                        let info = get_page_info(src_frame).ok_or(Error::new(EFAULT))?;
                        let CowResult {
                            new_frame,
                            old_frame,
                        } = cow(src_frame, info, RefKind::Shared)
                            .map_err(|_| Error::new(ENOMEM))?;
                        unsafe {
                            if let Some((_, _, flush)) = src_address_space
                                .table
                                .utable
                                .remap_with_full(page.start_address(), |_, _| {
                                    Some((new_frame.base(), src_grant_flags))
                                })
                            {
                                flush.flush();
                            }
                        }
                        if let Some(old_frame) = old_frame {
                            src_flusher
                                .as_mut()
                                .expect("src_flusher created for writable cow borrow")
                                .queue(old_frame, None, TlbShootdownActions::FREE);
                        }
                        unsafe {
                            let Some(flush) = dst_mapper.map_phys(
                                dst_base.next_by(i).start_address(),
                                new_frame.base(),
                                flags.write(true),
                                0,
                            ) else {
                                dst_flusher.queue(new_frame, None, TlbShootdownActions::FREE);
                                return Err(Error::new(ENOMEM));
                            };
                            flush.ignore();

                            dst_flusher.queue(new_frame, None, TlbShootdownActions::NEW_MAPPING);
                        }
                        continue;
                    }
                }

                let writable = match get_page_info(Frame::containing(phys)) {
                    None => true,
                    Some(i) => {
                        if i.add_ref(RefKind::Shared).is_err() {
                            continue;
                        };

                        i.allows_writable()
                    }
                };

                unsafe {
                    let flush = dst_mapper
                        .map_phys(
                            dst_base.next_by(i).start_address(),
                            phys,
                            flags.write(flags.has_write() && writable),
                            0,
                        )
                        .ok_or(Error::new(ENOMEM))?;
                    flush.ignore();

                    dst_flusher.queue(
                        Frame::containing(phys),
                        None,
                        TlbShootdownActions::NEW_MAPPING,
                    );
                }
            }
        }

        for (_, src_grant) in src_address_space.grants.conflicts_mut(src_span) {
            if let Provider::FmapBorrowed {
                ref mut pin_refcount,
                ..
            } = src_grant.provider
            {
                *pin_refcount += 1;
            }
        }

        Ok(Grant {
            base: dst_base,
            info: GrantInfo {
                page_count,
                flags,
                mapped: true,
                provider: Provider::External {
                    address_space: src_address_space_lock,
                    src_base,
                    is_pinned_userscheme_borrow,
                },
            },
        })
    }
    pub fn copy_mappings(
        src_base: Page,
        dst_base: Page,
        page_count: usize,
        flags: PageFlags<RmmA>,
        src_mapper: &mut PageMapper,
        dst_mapper: &mut PageMapper,
        src_flusher: &mut Flusher,
        dst_flusher: &mut impl GenericFlusher,
        mode: CopyMappingsMode,
    ) -> Result<Grant, Enomem> {
        match mode {
            CopyMappingsMode::Owned { .. } => {
                let start = src_base.start_address();
                let end = src_base.next_by(page_count).start_address();
                unsafe {
                    src_mapper.try_remap_range_with_full(
                        start,
                        end,
                        |same_phys, old_flags| Some((same_phys, old_flags.write(false))),
                        |src_virt, phys, _old_flags| {
                            let frame = Frame::containing(phys);
                            src_flusher.queue(frame, None, TlbShootdownActions::REVOKE_WRITE);

                            let src_page_info = get_page_info(frame)
                                .expect("allocated page was not present in the global page array");
                            let src_frame = match src_page_info.add_ref(RefKind::Cow) {
                                Ok(()) => frame,
                                Err(AddRefError::CowToShared) => {
                                    unreachable!("CowToShared cannot occur in Owned copy_mappings")
                                }
                                Err(AddRefError::SharedToCow) => {
                                    if src_page_info.add_ref(RefKind::Shared)
                                        == Err(AddRefError::RcOverflow)
                                    {
                                        return Err(Enomem);
                                    }
                                    let CowResult {
                                        new_frame,
                                        old_frame,
                                    } = cow(frame, src_page_info, RefKind::Cow)
                                        .map_err(|_| Enomem)?;
                                    if let Some(old_frame) = old_frame {
                                        src_flusher.queue(
                                            old_frame,
                                            None,
                                            TlbShootdownActions::FREE,
                                        );
                                    }
                                    new_frame
                                }
                                Err(AddRefError::RcOverflow) => return Err(Enomem),
                            };

                            let page_idx =
                                Page::containing_address(src_virt).offset_from(src_base);
                            let dst_page = dst_base.next_by(page_idx).start_address();
                            let Some(map_result) = (unsafe {
                                dst_mapper.map_phys(
                                    dst_page,
                                    src_frame.base(),
                                    flags.write(false),
                                    0,
                                )
                            }) else {
                                return Ok(core::ops::ControlFlow::Break(()));
                            };
                            unsafe {
                                map_result.ignore();
                            }
                            dst_flusher.queue(
                                src_frame,
                                None,
                                TlbShootdownActions::NEW_MAPPING,
                            );
                            Ok(core::ops::ControlFlow::Continue(()))
                        },
                    )?;
                }
            }
            CopyMappingsMode::Borrowed => {
                for page_idx in 0..page_count {
                    let src_page = src_base.next_by(page_idx);
                    let dst_page = dst_base.next_by(page_idx).start_address();

                    let src_frame = if let Some((phys, _)) =
                        src_mapper.translate(src_page.start_address())
                    {
                        Frame::containing(phys)
                    } else {
                        let must_be_zero = true;
                        let new_frame =
                            init_frame(RefCount::One, must_be_zero).expect("TODO: handle OOM");
                        let src_flush = unsafe {
                            src_mapper
                                .map_phys(src_page.start_address(), new_frame.base(), flags, 0)
                                .expect("TODO: handle OOM")
                        };
                        unsafe {
                            src_flush.ignore();
                        }
                        src_flusher.queue(new_frame, None, TlbShootdownActions::NEW_MAPPING);
                        new_frame
                    };

                    let src_frame = {
                        let src_page_info = get_page_info(src_frame)
                            .expect("allocated page was not present in the global page array");

                        match src_page_info.add_ref(RefKind::Shared) {
                            Ok(()) => src_frame,
                            Err(AddRefError::CowToShared) => {
                                let CowResult {
                                    new_frame,
                                    old_frame,
                                } = cow(src_frame, src_page_info, RefKind::Shared)
                                    .map_err(|_| Enomem)?;
                                if let Some(old_frame) = old_frame {
                                    src_flusher.queue(old_frame, None, TlbShootdownActions::FREE);
                                }
                                unsafe {
                                    if let Some((_flags, phys, flush)) = src_mapper
                                        .remap_with_full(src_page.start_address(), |_, f| {
                                            Some((new_frame.base(), f))
                                        })
                                    {
                                        flush.flush();
                                        src_flusher.queue(
                                            Frame::containing(phys),
                                            None,
                                            TlbShootdownActions::MOVE,
                                        );
                                    }
                                }
                                new_frame
                            }
                            Err(AddRefError::SharedToCow) => {
                                unreachable!("SharedToCow cannot occur in Borrowed copy_mappings")
                            }
                            Err(AddRefError::RcOverflow) => return Err(Enomem),
                        }
                    };

                    let Some(map_result) = (unsafe {
                        dst_mapper.map_phys(
                            dst_page,
                            src_frame.base(),
                            flags.write(flags.has_write()),
                            0,
                        )
                    }) else {
                        break;
                    };
                    unsafe {
                        map_result.ignore();
                    }
                    dst_flusher.queue(src_frame, None, TlbShootdownActions::NEW_MAPPING);
                }
            }
        }

        Ok(Grant {
            base: dst_base,
            info: GrantInfo {
                page_count,
                flags,
                mapped: true,
                provider: match mode {
                    CopyMappingsMode::Owned { cow_file_ref } => Provider::Allocated {
                        cow_file_ref,
                        phys_contiguous: false,
                    },
                    CopyMappingsMode::Borrowed => Provider::AllocatedShared {
                        is_pinned_userscheme_borrow: false,
                    },
                },
            },
        })
    }
    /// Move a grant between two address spaces.
    pub fn transfer(
        mut self,
        dst_base: Page,
        flags: PageFlags<RmmA>,
        src_mapper: &mut PageMapper,
        mut dst_mapper: Option<&mut PageMapper>,
        src_flusher: &mut Flusher,
        dst_flusher: &mut impl GenericFlusher,
    ) -> Result<Grant> {
        assert!(!self.info.is_pinned());
        let src_span = self.span();
        let dst_span = PageSpan::new(dst_base, self.info.page_count);
        if dst_mapper.is_none() {
            assert!(!src_span.intersects(dst_span));
        }

        const TRANSFER_BATCH_PAGES: usize = 64;
        let mut batch = [(
            VirtualAddress::new(0),
            rmm::PhysicalAddress::new(0),
            0u8,
            PageFlags::new(),
        ); TRANSFER_BATCH_PAGES];
        let mut cur = self.base.start_address();
        let end = self.base.next_by(self.info.page_count).start_address();

        while cur < end {
            let (count, next_cur) = unsafe {
                src_mapper.unmap_range_batch(
                    cur,
                    end,
                    &mut batch,
                    Some(|table_phys| {
                        defer_user_table_free(table_phys, src_flusher);
                    }),
                )
            };
            assert!(count <= batch.len());
            assert!(next_cur > cur);
            cur = next_cur;

            for &(src_virt, phys, lvl, _flags) in &batch[..count] {
                assert_eq!(lvl, 0, "no userspace pages have huge mappings");
                src_flusher.queue(Frame::containing(phys), None, TlbShootdownActions::MOVE);
                let src_page = Page::containing_address(src_virt);
                let dst_page = dst_base.next_by(src_page.offset_from(self.base));
                let dst_mapper_ref = dst_mapper.as_deref_mut().unwrap_or(&mut *src_mapper);
                let flush = unsafe {
                    dst_mapper_ref
                        .map_phys(dst_page.start_address(), phys, flags, 0)
                        .expect("TODO: OOM")
                };
                unsafe {
                    flush.ignore();
                }
                dst_flusher.queue(
                    Frame::containing(phys),
                    None,
                    TlbShootdownActions::NEW_MAPPING,
                );
            }
        }

        self.base = dst_base;
        Ok(self)
    }

    // Caller must check this doesn't violate access rights for e.g. shared memory.
    pub fn remap(
        &mut self,
        mapper: &mut PageMapper,
        flusher: &mut Flusher,
        new_flags: PageFlags<RmmA>,
    ) {
        assert!(self.info.mapped);

        let start = self.base.start_address();
        let end = self.base.next_by(self.info.page_count).start_address();
        unsafe {
            mapper.remap_range_with_full(
                start,
                end,
                |same_phys, old_flags| {
                    if !old_flags.has_write() && new_flags.has_write() {
                        Some((same_phys, new_flags.write(false)))
                    } else {
                        Some((same_phys, new_flags))
                    }
                },
                |_virt, phys, old_flags| {
                    flusher.queue(
                        Frame::containing(phys),
                        None,
                        TlbShootdownActions::change_of_flags(old_flags, new_flags),
                    );
                },
            );
        }

        self.info.flags = new_flags;
    }

    #[must_use = "will not unmap itself"]
    pub fn unmap(
        mut self,
        mapper: &mut PageMapper,
        flusher: &mut impl GenericFlusher,
        token: &mut CleanLockToken,
    ) -> UnmapResult {
        assert!(self.info.mapped);
        assert!(!self.info.is_pinned());

        if let Provider::External {
            ref address_space,
            src_base,
            ..
        } = self.info.provider
        {
            let mut guard = address_space.acquire_write(token.downgrade());

            for (_, grant) in guard
                .grants
                .conflicts_mut(PageSpan::new(src_base, self.info.page_count))
            {
                match grant.provider {
                    Provider::FmapBorrowed {
                        ref mut pin_refcount,
                        ..
                    } => {
                        *pin_refcount = pin_refcount
                            .checked_sub(1)
                            .expect("fmap pinning code is wrong")
                    }
                    _ => continue,
                }
            }
        }

        let is_phys_contiguous = matches!(
            self.info.provider,
            Provider::Allocated {
                phys_contiguous: true,
                ..
            }
        );

        // TODO: Add old debug assertions back, into Flusher.
        let is_fmap_shared = match self.info.provider {
            Provider::Allocated { .. } => Some(false),
            Provider::AllocatedShared { .. } => None,
            Provider::External { .. } => None,
            Provider::PhysBorrowed { .. } => None,
            Provider::FmapBorrowed { .. } => Some(true),
        };

        if is_phys_contiguous {
            let (phys_base, _) = mapper.translate(self.base.start_address()).unwrap();
            let base_frame = Frame::containing(phys_base);
            let start = self.base.start_address();
            let end = self.base.next_by(self.info.page_count).start_address();
            let mut unmapped_pages: usize = 0;

            unsafe {
                mapper.unmap_range(
                    start,
                    end,
                    |virt, phys, lvl, _flags| {
                        assert_eq!(lvl, 0);
                        let i = Page::containing_address(virt).offset_from(self.base);
                        assert_eq!(phys, base_frame.next_by(i).base());
                        unmapped_pages += 1;
                    },
                    Some(|table_phys| {
                        defer_user_table_free(table_phys, flusher);
                    }),
                );
            }
            assert_eq!(
                unmapped_pages,
                self.info.page_count,
                "all physborrowed grants must be fully Present in the page tables"
            );

            flusher.queue(
                base_frame,
                Some(NonZeroUsize::new(self.info.page_count).unwrap()),
                TlbShootdownActions::FREE,
            );
        } else {
            const UNMAP_BATCH_PAGES: usize = 64;
            let mut batch = [(
                VirtualAddress::new(0),
                rmm::PhysicalAddress::new(0),
                0u8,
                PageFlags::new(),
            ); UNMAP_BATCH_PAGES];
            let mut cur = self.base.start_address();
            let end = self.base.next_by(self.info.page_count).start_address();
            while cur < end {
                let (count, next_cur) = unsafe {
                    mapper.unmap_range_batch(
                        cur,
                        end,
                        &mut batch,
                        Some(|table_phys| {
                            defer_user_table_free(table_phys, flusher);
                        }),
                    )
                };
                assert!(count <= batch.len());
                assert!(next_cur > cur);
                cur = next_cur;
                for &(_virt, phys, lvl, _flags) in &batch[..count] {
                    assert_eq!(lvl, 0);
                    flusher.queue(Frame::containing(phys), None, TlbShootdownActions::FREE);
                }
            }
        }

        self.info.mapped = false;

        // Dummy value, won't be read.
        let provider = core::mem::replace(
            &mut self.info.provider,
            Provider::AllocatedShared {
                is_pinned_userscheme_borrow: false,
            },
        );

        let mut munmap_flags = MunmapFlags::empty();
        munmap_flags.set(
            MunmapFlags::NEEDS_SYNC,
            is_fmap_shared.unwrap_or(false) && self.info.flags.has_write(),
        );

        UnmapResult {
            size: self.info.page_count * PAGE_SIZE,
            file_desc: match provider {
                Provider::Allocated { cow_file_ref, .. } => cow_file_ref,
                Provider::FmapBorrowed { file_ref, .. } => Some(file_ref),
                _ => None,
            },
            flags: munmap_flags,
        }
    }

    /// Extract out a region into a separate grant. The return value is as
    /// follows: (before, new split, after). Before and after may be `None`,
    /// which occurs when the split off region is at the start or end of the
    /// page respectively.
    ///
    /// # Panics
    ///
    /// Panics if the start or end addresses of the region is not aligned to the
    /// page size. To round up the size to the nearest page size, use `.round()`
    /// on the region.
    ///
    /// Also panics if the given region isn't completely contained within the
    /// grant. Use `grant.intersect` to find a sub-region that works.
    pub fn span(&self) -> PageSpan {
        PageSpan::new(self.base, self.info.page_count)
    }
    pub fn extract(mut self, span: PageSpan) -> Option<(Option<Grant>, Grant, Option<Grant>)> {
        assert!(self.info.can_extract(false));

        let (before_span, this_span, after_span) = self.span().slice(span);

        let before_grant = before_span.map(|span| Grant {
            base: span.base,
            info: GrantInfo {
                flags: self.info.flags,
                mapped: self.info.mapped,
                page_count: span.count,
                provider: match self.info.provider {
                    Provider::External {
                        ref address_space,
                        src_base,
                        ..
                    } => Provider::External {
                        address_space: Arc::clone(address_space),
                        src_base,
                        is_pinned_userscheme_borrow: false,
                    },
                    Provider::Allocated {
                        ref cow_file_ref, ..
                    } => Provider::Allocated {
                        cow_file_ref: cow_file_ref.clone(),
                        phys_contiguous: false,
                    },
                    Provider::AllocatedShared { .. } => Provider::AllocatedShared {
                        is_pinned_userscheme_borrow: false,
                    },
                    Provider::PhysBorrowed { base } => Provider::PhysBorrowed { base },
                    Provider::FmapBorrowed {
                        ref file_ref,
                        is_cow,
                        ..
                    } => Provider::FmapBorrowed {
                        file_ref: file_ref.clone(),
                        pin_refcount: 0,
                        is_cow,
                    },
                },
            },
        });

        let middle_page_offset = before_grant.as_ref().map_or(0, |g| g.info.page_count);

        match self.info.provider {
            Provider::PhysBorrowed { ref mut base } => *base = base.next_by(middle_page_offset),
            Provider::FmapBorrowed {
                ref mut file_ref, ..
            }
            | Provider::Allocated {
                cow_file_ref: Some(ref mut file_ref),
                ..
            } => file_ref.base_offset += middle_page_offset * PAGE_SIZE,
            Provider::Allocated {
                cow_file_ref: None, ..
            }
            | Provider::AllocatedShared { .. }
            | Provider::External { .. } => (),
        }

        let after_grant = after_span.map(|span| Grant {
            base: span.base,
            info: GrantInfo {
                flags: self.info.flags,
                mapped: self.info.mapped,
                page_count: span.count,
                provider: match self.info.provider {
                    Provider::Allocated {
                        cow_file_ref: None, ..
                    } => Provider::Allocated {
                        cow_file_ref: None,
                        phys_contiguous: false,
                    },
                    Provider::AllocatedShared { .. } => Provider::AllocatedShared {
                        is_pinned_userscheme_borrow: false,
                    },
                    Provider::Allocated {
                        cow_file_ref: Some(ref file_ref),
                        ..
                    } => Provider::Allocated {
                        cow_file_ref: Some(GrantFileRef {
                            base_offset: file_ref.base_offset + this_span.count * PAGE_SIZE,
                            description: Arc::clone(&file_ref.description),
                        }),
                        phys_contiguous: false,
                    },
                    Provider::External {
                        ref address_space,
                        src_base,
                        ..
                    } => Provider::External {
                        address_space: Arc::clone(address_space),
                        src_base,
                        is_pinned_userscheme_borrow: false,
                    },

                    Provider::PhysBorrowed { base } => Provider::PhysBorrowed {
                        base: base.next_by(this_span.count),
                    },
                    Provider::FmapBorrowed {
                        ref file_ref,
                        is_cow,
                        ..
                    } => Provider::FmapBorrowed {
                        file_ref: GrantFileRef {
                            base_offset: file_ref.base_offset + this_span.count * PAGE_SIZE,
                            description: Arc::clone(&file_ref.description),
                        },
                        pin_refcount: 0,
                        is_cow,
                    },
                },
            },
        });

        self.base = this_span.base;
        self.info.page_count = this_span.count;

        Some((before_grant, self, after_grant))
    }
}
impl GrantInfo {
    pub fn is_pinned(&self) -> bool {
        matches!(
            self.provider,
            Provider::External {
                is_pinned_userscheme_borrow: true,
                ..
            } | Provider::AllocatedShared {
                is_pinned_userscheme_borrow: true,
                ..
            } | Provider::FmapBorrowed {
                pin_refcount: 1..,
                ..
            }
        )
    }
    pub fn can_extract(&self, unpin: bool) -> bool {
        (!self.is_pinned() || unpin)
            | matches!(
                self.provider,
                Provider::Allocated {
                    phys_contiguous: true,
                    ..
                }
            )
    }
    pub fn unpin(&mut self) {
        if let Provider::External {
            ref mut is_pinned_userscheme_borrow,
            ..
        }
        | Provider::AllocatedShared {
            ref mut is_pinned_userscheme_borrow,
            ..
        } = self.provider
        {
            *is_pinned_userscheme_borrow = false;
        }
    }

    pub fn flags(&self) -> PageFlags<RmmA> {
        self.flags
    }
    pub fn page_count(&self) -> usize {
        self.page_count
    }
    pub fn can_have_flags(&self, flags: MapFlags) -> bool {
        // TODO: read (some architectures support execute-only pages)
        let is_downgrade = (self.flags.has_write() || !flags.contains(MapFlags::PROT_WRITE))
            && (self.flags.has_execute() || !flags.contains(MapFlags::PROT_EXEC));

        match self.provider {
            Provider::Allocated { .. } => true,
            _ => is_downgrade,
        }
    }

    pub fn can_be_merged_if_adjacent(&self, with: &Self) -> bool {
        if self.mapped != with.mapped || self.flags.data() != with.flags.data() {
            return false;
        }

        match (&self.provider, &with.provider) {
            (
                Provider::Allocated {
                    cow_file_ref: None,
                    phys_contiguous: false,
                },
                Provider::Allocated {
                    cow_file_ref: None,
                    phys_contiguous: false,
                },
            ) => true,
            //(Provider::PhysBorrowed { base: ref lhs }, Provider::PhysBorrowed { base: ref rhs }) => lhs.next_by(self.page_count) == rhs.clone(),
            //(Provider::External { address_space: ref lhs_space, src_base: ref lhs_base, cow: lhs_cow, .. }, Provider::External { address_space: ref rhs_space, src_base: ref rhs_base, cow: rhs_cow, .. }) => Arc::ptr_eq(lhs_space, rhs_space) && lhs_cow == rhs_cow && lhs_base.next_by(self.page_count) == rhs_base.clone(),
            _ => false,
        }
    }
    pub fn grant_flags(&self) -> GrantFlags {
        let mut flags = GrantFlags::empty();
        // TODO: has_read
        flags.set(GrantFlags::GRANT_READ, true);

        flags.set(GrantFlags::GRANT_WRITE, self.flags.has_write());
        flags.set(GrantFlags::GRANT_EXEC, self.flags.has_execute());

        // TODO: Set GRANT_LAZY

        match self.provider {
            Provider::External {
                is_pinned_userscheme_borrow,
                ..
            } => {
                flags.set(GrantFlags::GRANT_PINNED, is_pinned_userscheme_borrow);
                flags |= GrantFlags::GRANT_SHARED;
            }
            Provider::Allocated {
                ref cow_file_ref,
                phys_contiguous,
            } => {
                // !GRANT_SHARED is equivalent to "GRANT_PRIVATE"
                flags.set(GrantFlags::GRANT_SCHEME, cow_file_ref.is_some());
                flags.set(GrantFlags::GRANT_PHYS_CONTIGUOUS, phys_contiguous);
            }
            Provider::AllocatedShared {
                is_pinned_userscheme_borrow,
            } => {
                flags |= GrantFlags::GRANT_SHARED;
                flags.set(GrantFlags::GRANT_PINNED, is_pinned_userscheme_borrow);
            }
            Provider::PhysBorrowed { .. } => {
                flags |= GrantFlags::GRANT_SHARED | GrantFlags::GRANT_PHYS;
            }
            Provider::FmapBorrowed { .. } => {
                flags |= GrantFlags::GRANT_SHARED | GrantFlags::GRANT_SCHEME;
            }
        }

        flags
    }
    pub fn file_ref(&self) -> Option<&GrantFileRef> {
        match self.provider {
            Provider::FmapBorrowed { ref file_ref, .. }
            | Provider::Allocated {
                cow_file_ref: Some(ref file_ref),
                ..
            } => Some(file_ref),
            _ => None,
        }
    }
}

impl Drop for GrantInfo {
    #[track_caller]
    fn drop(&mut self) {
        // XXX: This will not show the address...
        assert!(
            !self.mapped,
            "Grant dropped while still mapped: {:#x?}",
            self
        );
    }
}

pub const DANGLING: usize = 1 << (usize::BITS - 2);

#[derive(Debug)]
pub struct Table {
    pub utable: PageMapper,
}

impl Drop for AddrSpace {
    fn drop(&mut self) {
        let mut token = unsafe { CleanLockToken::new() };
        self.inner_drop(&mut token);
        #[cfg(feature = "drop_panic")]
        {
            panic!("AddrSpace dropped");
        }
    }
}

impl Drop for Table {
    fn drop(&mut self) {
        if self.utable.is_current() {
            // TODO: Do not flush (we immediately context switch after exit(), what else is there
            // to do?). Instead, we can garbage-collect such page tables in the idle kernel context
            // before it waits for interrupts. Or maybe not, depends on what future benchmarks will
            // indicate.
            unsafe {
                RmmA::set_table(TableKind::User, super::empty_cr3());
            }
        }
        unsafe {
            deallocate_frame(Frame::containing(self.utable.table().phys()));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AccessMode {
    Read,
    Write,
    InstrFetch,
}

#[derive(Debug)]
pub enum PfError {
    Segv,
    Oom,
    Killed,
    NonfatalInternalError,
    // TODO: Handle recursion limit by mapping a zeroed page? Or forbid borrowing borrowed memory,
    // and ensure pages are mapped at grant time?
    RecursionLimitExceeded,
}

pub struct CowResult {
    /// New frame, which has been given an exclusive reference the caller can use.
    pub new_frame: Frame,

    /// Old frame. The caller must decrease its refcount if present, after it has shot down the TLB
    /// of other CPUs properly.
    pub old_frame: Option<Frame>,
}

/// Consumes an existing reference to old_frame, and then returns an exclusive frame, with refcount
/// either preinitialized to One or Shared(2) depending on initial_ref_kind. This may be the same
/// frame, or (if the refcount is modified simultaneously) a new frame whereas the old frame is
/// deallocated.
fn cow(
    old_frame: Frame,
    old_info: &PageInfo,
    initial_ref_kind: RefKind,
) -> Result<CowResult, PfError> {
    let old_refcount = old_info.refcount();
    assert!(old_refcount.is_some());

    let initial_rc = match initial_ref_kind {
        RefKind::Cow => RefCount::One,
        RefKind::Shared => RefCount::Shared(NonZeroUsize::new(2).unwrap()),
    };

    if old_refcount == Some(RefCount::One) {
        // We were lucky; the frame was already exclusively owned, so the refcount cannot be
        // modified unless we modify it. This is the special case where the old_frame returned is
        // None.

        if initial_ref_kind == RefKind::Shared {
            old_info
                .refcount
                .store(initial_rc.to_raw(), Ordering::Relaxed);
        }
        return Ok(CowResult {
            new_frame: old_frame,
            old_frame: None,
        });
    }

    let new_frame;

    if old_frame == the_zeroed_frame().0 {
        let must_be_zero = true;
        new_frame = init_frame(initial_rc, must_be_zero)?;
    } else {
        let must_be_zero = false;
        new_frame = init_frame(initial_rc, must_be_zero)?;
        unsafe {
            copy_frame_to_frame_directly(new_frame, old_frame);
        }
    };

    Ok(CowResult {
        new_frame,
        old_frame: Some(old_frame),
    })
}

fn map_zeroed(
    mapper: &mut PageMapper,
    page: Page,
    page_flags: PageFlags<RmmA>,
    _writable: bool,
) -> Result<Frame, PfError> {
    let must_be_zero = true;
    let new_frame = init_frame(RefCount::One, must_be_zero)?;

    unsafe {
        mapper
            // TODO: huge pages?
            .map_phys(page.start_address(), new_frame.base(), page_flags, 0)
            .ok_or(PfError::Oom)?
            .ignore();
    }

    Ok(new_frame)
}

pub unsafe fn copy_frame_to_frame_directly(dst: Frame, src: Frame) {
    // Optimized exact-page-size copy function?

    // TODO: For new frames, when the kernel's linear phys=>virt mappings are 4k, this is almost
    // guaranteed to cause either one (or two) TLB misses.

    let dst = RmmA::phys_to_virt(dst.base()).data() as *mut u8;
    let src = RmmA::phys_to_virt(src.base()).data() as *const u8;

    unsafe {
        dst.copy_from_nonoverlapping(src, PAGE_SIZE);
    }
}

pub fn try_correcting_page_tables(
    faulting_page: Page,
    access: AccessMode,
    token: &mut CleanLockToken,
) -> Result<(), PfError> {
    let Ok(addr_space) = AddrSpace::current() else {
        debug!("User page fault without address space being set.");
        return Err(PfError::Segv);
    };

    let mut lock_token = token.token();
    let addr_space_lock = addr_space.acquire_write(lock_token.downgrade());

    let (_, flush, _) = correct_inner(&addr_space, addr_space_lock, faulting_page, access, 0)?;

    flush.flush();

    Ok(())
}

// TODO: maybe refactor the return type into a struct/typedef?
#[expect(clippy::type_complexity)]
/// XXX: This require passing L5 addr_space_guard.
/// Caller must ensure there's no other lock being held at this point.
/// Caller also need to provide clean token for the new AddrSpace.
fn correct_inner<'l>(
    addr_space_lock: &'l Arc<AddrSpaceWrapper>,
    mut addr_space: RwLockWriteGuard<'l, L5, AddrSpace>,
    faulting_page: Page,
    access: AccessMode,
    recursion_level: u32,
) -> Result<(Frame, PageFlush<RmmA>, RwLockWriteGuard<'l, L5, AddrSpace>), PfError> {
    let mut flusher = Flusher::with_cpu_set(&addr_space_lock.used_by, &addr_space_lock.tlb_ack);

    let Some((grant_base, grant_info)) = addr_space.grants.contains(faulting_page) else {
        debug!("Lacks grant");
        return Err(PfError::Segv);
    };

    let pages_from_grant_start = faulting_page.offset_from(grant_base);

    let grant_flags = grant_info.flags();
    match access {
        _ if !grant_flags.has_user() => {
            debug!("Access, but grant was PROT_NONE.");
            return Err(PfError::Segv);
        }
        AccessMode::Read => (),

        AccessMode::Write if !grant_flags.has_write() => {
            debug!("Write, but grant was not PROT_WRITE.");
            return Err(PfError::Segv);
        }
        AccessMode::InstrFetch if !grant_flags.has_execute() => {
            debug!("Instuction fetch, but grant was not PROT_EXEC.");
            return Err(PfError::Segv);
        }

        _ => (),
    }

    // By now, the memory at the faulting page is actually valid, but simply not yet mapped, either
    // at all, or with the required flags.

    let faulting_entry_opt = addr_space
        .table
        .utable
        .translate(faulting_page.start_address())
        .map(|(phys, page_flags)| (Frame::containing(phys), page_flags));
    let faulting_frame_opt = faulting_entry_opt.map(|(frame, _)| frame);
    let faulting_pageinfo_opt = faulting_frame_opt.map(|frame| (frame, get_page_info(frame)));

    // TODO: Aligned readahead? AMD Zen3+ CPUs can smash 4 4k pages that are 16k-aligned, into a
    // single TLB entry, thus emulating 16k pages albeit with higher page table overhead. With the
    // correct madvise information, allocating 4 contiguous pages and mapping them together, might
    // be a useful future optimization.
    //
    // TODO: Readahead backwards, i.e. MAP_GROWSDOWN.

    let mut allow_writable = true;
    let mut override_flags: Option<PageFlags<RmmA>> = None;

    let frame = match grant_info.provider {
        Provider::Allocated { .. } | Provider::AllocatedShared { .. }
            if access == AccessMode::Write =>
        {
            match faulting_pageinfo_opt {
                Some((_, None)) => unreachable!("allocated page needs frame to be valid"),
                Some((frame, Some(info))) => {
                    if info.allows_writable() {
                        frame
                    } else {
                        let result = cow(frame, info, RefKind::Cow)?;
                        if let Some(old_frame) = result.old_frame {
                            flusher.queue(old_frame, None, TlbShootdownActions::FREE);
                        }
                        result.new_frame
                    }
                }
                _ => map_zeroed(
                    &mut addr_space.table.utable,
                    faulting_page,
                    grant_flags,
                    true,
                )?,
            }
        }

        Provider::Allocated { .. } | Provider::AllocatedShared { .. } => {
            match faulting_pageinfo_opt {
                Some((_, None)) => unreachable!("allocated page needs frame to be valid"),

                // TODO: Can this match arm even be reached? In other words, can the TLB cache
                // remember that pages are not present?
                Some((frame, Some(page_info))) => {
                    // Keep in mind that allow_writable must always be true if this code is reached
                    // for AllocatedShared, since shared pages cannot be mapped lazily (without
                    // using AddrSpace backrefs).
                    allow_writable = page_info.allows_writable();

                    frame
                }

                None => {
                    // TODO: the zeroed page first, readonly?
                    map_zeroed(
                        &mut addr_space.table.utable,
                        faulting_page,
                        grant_flags,
                        false,
                    )?
                }
            }
        }
        Provider::PhysBorrowed { base } => base.next_by(pages_from_grant_start),
        Provider::External {
            address_space: ref foreign_address_space,
            src_base,
            ..
        } => {
            let foreign_address_space = Arc::clone(foreign_address_space);

            if Arc::ptr_eq(addr_space_lock, &foreign_address_space) {
                return Err(PfError::NonfatalInternalError);
            }

            // XXX: This is cheating, but guaranteed from Arc::ptr_eq above we won't deadlock
            let mut free_token = unsafe { CleanLockToken::new() };
            let mut guard = foreign_address_space.acquire_upgradeable_read(free_token.downgrade());
            let src_page = src_base.next_by(pages_from_grant_start);

            match guard.grants.contains(src_page) {
                Some(_) => {
                    let (src_frame, src_pte_flags) =
                        match guard.table.utable.translate(src_page.start_address()) {
                            Some((phys, pte_flags)) => (Frame::containing(phys), pte_flags),
                            _ => {
                                // Grant was valid (TODO check), but we need to correct the underlying page.
                                // TODO: Access mode

                                // TODO: Reasonable maximum?
                                let new_recursion_level = recursion_level
                                    .checked_add(1)
                                    .filter(|new_lvl| *new_lvl < 16)
                                    .ok_or(PfError::RecursionLimitExceeded)?;

                                let guard_token = guard.into_token();
                                let addr_space_guard_token = addr_space.into_token();
                                drop(flusher);

                                // FIXME: Can this result in invalid address space state?
                                let ext_addrspace = &foreign_address_space;
                                let mut free_token = unsafe { CleanLockToken::new() };
                                let (frame, _, _) = {
                                    let g = ext_addrspace.acquire_write(free_token.downgrade());
                                    correct_inner(
                                        ext_addrspace,
                                        g,
                                        src_page,
                                        AccessMode::Read,
                                        new_recursion_level,
                                    )?
                                };

                                // SAFETY: Caller guarantees addr_space_guard is coming from this addr_space_lock
                                addr_space = unsafe {
                                    addr_space_lock.acquire_rewrite(addr_space_guard_token)
                                };
                                flusher = Flusher::with_cpu_set(
                                    &addr_space_lock.used_by,
                                    &addr_space_lock.tlb_ack,
                                );

                                // SAFETY: We guarantee that guard is coming from foreign_address_space
                                guard = unsafe {
                                    foreign_address_space.acquire_reupgradeable_read(guard_token)
                                };

                                let pte_flags = guard
                                    .table
                                    .utable
                                    .translate(src_page.start_address())
                                    .map_or(PageFlags::new(), |(_, f)| f);

                                (frame, pte_flags)
                            }
                        };

                    let cow_fmap_flags = if grant_flags.has_write() && !src_pte_flags.has_write() {
                        guard.grants.contains(src_page).and_then(|(_, g)| {
                            matches!(g.provider, Provider::FmapBorrowed { is_cow: true, .. })
                                .then_some(g.flags)
                        })
                    } else {
                        None
                    };

                    if let Some(src_grant_flags) = cow_fmap_flags {
                        let info = get_page_info(src_frame).ok_or(PfError::Segv)?;
                        let CowResult {
                            new_frame,
                            old_frame,
                        } = cow(src_frame, info, RefKind::Shared)?;
                        let mut guard = RwLockUpgradableGuard::upgrade(guard);
                        let mut src_flusher = Flusher::with_cpu_set(
                            &foreign_address_space.used_by,
                            &foreign_address_space.tlb_ack,
                        );
                        unsafe {
                            if let Some((_, _, flush)) = guard
                                .table
                                .utable
                                .remap_with_full(src_page.start_address(), |_, _| {
                                    Some((new_frame.base(), src_grant_flags))
                                })
                            {
                                flush.flush();
                            }
                        }
                        if let Some(old_frame) = old_frame {
                            src_flusher.queue(old_frame, None, TlbShootdownActions::FREE);
                        }
                        drop(src_flusher);
                        new_frame
                    } else {
                        let info =
                            get_page_info(src_frame).expect("all allocated frames need a PageInfo");

                        match info.add_ref(RefKind::Shared) {
                            Ok(()) => src_frame,
                            Err(AddRefError::CowToShared) => {
                                let CowResult {
                                    new_frame,
                                    old_frame,
                                } = cow(src_frame, info, RefKind::Shared)?;

                                if let Some(old_frame) = old_frame {
                                    flusher.queue(old_frame, None, TlbShootdownActions::FREE);
                                    flusher.flush();
                                }

                                let mut guard = RwLockUpgradableGuard::upgrade(guard);

                                // TODO: flusher
                                unsafe {
                                    guard
                                        .table
                                        .utable
                                        .remap_with_full(src_page.start_address(), |_, f| {
                                            Some((new_frame.base(), f))
                                        });
                                }

                                new_frame
                            }
                            Err(AddRefError::SharedToCow) => unreachable!(),
                            Err(AddRefError::RcOverflow) => return Err(PfError::Oom),
                        }
                    }
                }
                _ => {
                    // Grant did not exist, but we did own a Provider::External mapping, and cannot
                    // simply let the current context fail. TODO: But all borrowed memory shouldn't
                    // really be lazy though? TODO: Should a grant be created?

                    let mut guard = RwLockUpgradableGuard::upgrade(guard);

                    // TODO: Should this be called?
                    warn!("Mapped zero page since grant didn't exist");
                    map_zeroed(
                        &mut guard.table.utable,
                        src_page,
                        grant_flags,
                        access == AccessMode::Write,
                    )?
                }
            }
        }
        // TODO: NonfatalInternalError if !MAP_LAZY and this page fault occurs.
        Provider::FmapBorrowed {
            ref file_ref,
            is_cow,
            ..
        } => {
            if let Some((frame, existing_flags)) = faulting_entry_opt {
                if is_cow && access == AccessMode::Write {
                    let info = get_page_info(frame).ok_or(PfError::Segv)?;
                    let result = cow(frame, info, RefKind::Cow)?;
                    if let Some(old_frame) = result.old_frame {
                        flusher.queue(old_frame, None, TlbShootdownActions::FREE);
                    }
                    result.new_frame
                } else {
                    override_flags = Some(existing_flags);
                    frame
                }
            } else {
                let file_ref = file_ref.clone();
                let flags = map_flags(grant_info.flags());
                drop(flusher);
                let addr_space_guard_token = addr_space.into_token();

                // XXX: This is cheating, but guaranteed we won't deadlock because we've dropped addr_space_guard
                let mut token = unsafe { CleanLockToken::new() };

                let context_lock = Arc::clone(&*crate::context::current());
                if context_lock.read(token.token()).being_sigkilled {
                    return Err(PfError::Killed);
                }

                let (scheme_res, scheme_number) = {
                    let desc = &file_ref.description.read(token.token());
                    (desc.scheme_ref.upgrade(), desc.number)
                };
                let user_inner = scheme_res
                    .ok()
                    .and_then(|s| {
                        if let KernelSchemes::User(user) = s {
                            Some(user.inner)
                        } else {
                            None
                        }
                    })
                    .ok_or(PfError::Segv)?;

                const FMAP_WAIT_MAX_RESUMES: usize = 64;

                let offset =
                    file_ref.base_offset as u64 + (pages_from_grant_start * PAGE_SIZE) as u64;
                let tag = user_inner
                    .request_fmap(scheme_number, offset, 1, flags, &mut token)
                    .map_err(|_| PfError::Segv)?;

                let frame = {
                    let mut resolved = None;
                    for _ in 0..FMAP_WAIT_MAX_RESUMES {
                        let mut guard = context_lock.write(token.token());
                        if guard.being_sigkilled {
                            let _ = guard.fmap_ret.take();
                            drop(guard);
                            user_inner.cancel_fmap(tag, &context_lock, &mut token);
                            return Err(PfError::Killed);
                        }
                        if let Some(frame) = guard.fmap_ret.take() {
                            resolved = Some(frame);
                            break;
                        }
                        let _ = guard.hard_block(HardBlockedReason::AwaitingMmap {
                            file_ref: file_ref.clone(),
                        });
                        drop(guard);

                        super::switch(&mut token);
                    }
                    match resolved {
                        Some(frame) => frame,
                        None => {
                            let mut guard = context_lock.write(token.token());
                            let killed = guard.being_sigkilled;
                            let frame = guard.fmap_ret.take();
                            drop(guard);
                            if killed {
                                user_inner.cancel_fmap(tag, &context_lock, &mut token);
                                return Err(PfError::Killed);
                            }
                            if let Some(frame) = frame {
                                frame
                            } else {
                                user_inner.cancel_fmap(tag, &context_lock, &mut token);
                                return Err(PfError::Segv);
                            }
                        }
                    }
                };

                // SAFETY: Caller guarantees addr_space_guard is coming from this addr_space_lock
                addr_space = unsafe { addr_space_lock.acquire_rewrite(addr_space_guard_token) };
                flusher = Flusher::with_cpu_set(&addr_space_lock.used_by, &addr_space_lock.tlb_ack);

                info!("Got frame {:?} from external fmap", frame);

                frame
            }
        }
    };

    let new_flags = override_flags
        .unwrap_or_else(|| grant_flags.write(grant_flags.has_write() && allow_writable));
    let Some(flush) = (unsafe {
        addr_space
            .table
            .utable
            // TODO: huge pages?
            .map_phys(faulting_page.start_address(), frame.base(), new_flags, 0)
    }) else {
        // TODO
        return Err(PfError::Oom);
    };

    drop(flusher);
    Ok((frame, flush, addr_space))
}

#[derive(Debug)]
pub enum MmapMode {
    Cow,
    Shared,
}

pub struct BorrowedFmapSource<'a> {
    pub src_base: Page,
    pub mode: MmapMode,
    // TODO: There should be a method that obtains the lock from the guard.
    pub addr_space_lock: &'a Arc<AddrSpaceWrapper>,
    pub addr_space_guard: RwLockWriteGuard<'a, L5, AddrSpace>,
}

pub fn handle_notify_files(notify_files: UnmapVec, token: &mut CleanLockToken) {
    for file in notify_files {
        let _ = file.unmap(token);
    }
}

pub enum CopyMappingsMode {
    Owned { cow_file_ref: Option<GrantFileRef> },
    Borrowed,
}

// TODO: Check if polymorphism is worth it in terms of code size performance penalty vs optimized
// away checks.
pub trait GenericFlusher {
    // TODO: Don't require a frame unless FREE, require Page otherwise
    fn queue(
        &mut self,
        frame: Frame,
        phys_contiguous_count: Option<NonZeroUsize>,
        actions: TlbShootdownActions,
    );
}
pub struct NopFlusher;
impl GenericFlusher for NopFlusher {
    fn queue(
        &mut self,
        frame: Frame,
        phys_contiguous_count: Option<NonZeroUsize>,
        actions: TlbShootdownActions,
    ) {
        if actions.contains(TlbShootdownActions::FREE) {
            handle_free_action(frame, phys_contiguous_count);
        }
    }
}
fn handle_free_action(base: Frame, phys_contiguous_count: Option<NonZeroUsize>) {
    if let Some(count) = phys_contiguous_count {
        let count = count.get();
        let mut all_unreferenced = true;

        for i in 0..count {
            let new_rc = get_page_info(base.next_by(i))
                .expect("phys_contiguous frames all need PageInfos")
                .remove_ref();
            if new_rc.is_some() {
                all_unreferenced = false;
            }
        }

        if all_unreferenced {
            unsafe { deallocate_contiguous(base, count) };
        } else {
            for i in 0..count {
                let frame = base.next_by(i);
                let info = get_page_info(frame).expect("phys_contiguous frames all need PageInfos");
                if info.refcount().is_none() {
                    unsafe { deallocate_frame(frame) };
                }
            }
        }
    } else {
        let Some(info) = get_page_info(base) else {
            return;
        };
        if info.remove_ref().is_none() {
            unsafe {
                deallocate_frame(base);
            }
        }
    }
}
#[derive(Debug)]
struct FlusherState<'addrsp> {
    // TODO: what capacity?
    pagequeue: ArrayVec<PageQueueEntry, 32>,
    dirty: bool,

    ackword: &'addrsp AtomicU32,
    owner_cpu: crate::cpu_set::LogicalCpuId,
}

#[derive(Debug)]
enum PageQueueEntry {
    Free {
        base: Frame,
        phys_contiguous_count: Option<NonZeroUsize>,
    },
    Other {
        actions: TlbShootdownActions,
        //page: Page,
    },
}

#[derive(Debug)]
pub struct Flusher<'a, 'addrsp> {
    active_cpus: &'a LogicalCpuSet,
    state: FlusherState<'addrsp>,
}

impl<'a, 'addrsp> Flusher<'a, 'addrsp> {
    fn with_cpu_set(set: &'a LogicalCpuSet, ackword: &'addrsp AtomicU32) -> Self {
        crate::memory::mark_flusher_created();
        let percpu = PercpuBlock::current();
        let owner_cpu = percpu.cpu_id;
        let prev = percpu.live_flushers.fetch_add(1, Ordering::Relaxed);
        assert!(prev < u32::MAX, "live_flushers overflow");
        Self {
            active_cpus: set,
            state: FlusherState {
                pagequeue: ArrayVec::new(),
                dirty: false,
                ackword,
                owner_cpu,
            },
        }
    }
    fn reattach(active_cpus: &'a LogicalCpuSet, mut state: FlusherState<'addrsp>) -> Self {
        let percpu = PercpuBlock::current();
        let owner_cpu = percpu.cpu_id;
        let prev = percpu.live_flushers.fetch_add(1, Ordering::Relaxed);
        assert!(prev < u32::MAX, "live_flushers overflow");
        state.owner_cpu = owner_cpu;
        Self { active_cpus, state }
    }
    fn detach(mut self) -> FlusherState<'addrsp> {
        static DUMMY: AtomicU32 = AtomicU32::new(0);
        let owner_cpu = self.state.owner_cpu;
        let state = core::mem::replace(
            &mut self.state,
            FlusherState {
                pagequeue: ArrayVec::new(),
                ackword: &DUMMY,
                dirty: false,
                owner_cpu,
            },
        );
        let owner = crate::percpu::get_percpu_block(owner_cpu)
            .expect("flusher owner_cpu percpu block");
        let prev = owner.live_flushers.fetch_sub(1, Ordering::Relaxed);
        assert!(prev > 0, "live_flushers underflow");
        core::mem::forget(self);
        state
    }
    // NOTE: Lock must be held, which must be guaranteed by the caller.
    pub fn flush(&mut self) {
        let pages = core::mem::take(&mut self.state.pagequeue);

        #[expect(clippy::bool_comparison)]
        if pages.is_empty() && core::mem::replace(&mut self.state.dirty, false) == false {
            return;
        }

        self.state.ackword.store(0, Ordering::SeqCst);

        let mut affected_cpu_count = 0;

        let current_cpu_id = crate::cpu_id();

        for cpu_id in self.active_cpus.iter() {
            if cpu_id == current_cpu_id {
                continue;
            }

            crate::percpu::shootdown_tlb_ipi(Some(cpu_id));

            core::sync::atomic::fence(Ordering::SeqCst);

            if self.active_cpus.contains(cpu_id) {
                affected_cpu_count += 1;
            }
        }

        if self.active_cpus.contains(current_cpu_id) {
            rmm::PageFlushAll::<RmmA>::new().flush();
        }

        #[cfg(target_arch = "aarch64")]
        let mut stall_watch = crate::arch::misc::StallWatch::start(2);

        while self.state.ackword.load(Ordering::SeqCst) < affected_cpu_count {
            PercpuBlock::current().maybe_handle_tlb_shootdown();
            #[cfg(target_arch = "aarch64")]
            if stall_watch.stalled() {
                error!(
                    "TLB shootdown stalled on CPU {}: acknowledgements {}/{}",
                    current_cpu_id,
                    self.state.ackword.load(Ordering::SeqCst),
                    affected_cpu_count
                );
            }
            core::hint::spin_loop();
        }

        for entry in pages {
            match entry {
                PageQueueEntry::Free {
                    base,
                    phys_contiguous_count,
                } => {
                    handle_free_action(base, phys_contiguous_count);
                }
                PageQueueEntry::Other { actions } => {
                    // We currently invalidate everything on each flush
                    let _ = actions;
                }
            }
        }
    }
}
impl GenericFlusher for Flusher<'_, '_> {
    fn queue(
        &mut self,
        frame: Frame,
        phys_contiguous_count: Option<NonZeroUsize>,
        actions: TlbShootdownActions,
    ) {
        let actions = actions & !TlbShootdownActions::NEW_MAPPING;

        let entry = if actions.contains(TlbShootdownActions::FREE) {
            PageQueueEntry::Free {
                base: frame,
                phys_contiguous_count,
            }
        } else {
            PageQueueEntry::Other { actions }
        };
        self.state.dirty = true;

        if self.state.pagequeue.is_full() {
            self.flush();
        }
        self.state.pagequeue.push(entry);
    }
}
impl Drop for Flusher<'_, '_> {
    fn drop(&mut self) {
        self.flush();
        let owner = crate::percpu::get_percpu_block(self.state.owner_cpu)
            .expect("flusher owner_cpu percpu block");
        let prev = owner.live_flushers.fetch_sub(1, Ordering::Relaxed);
        assert!(prev > 0, "live_flushers underflow");
    }
}
bitflags::bitflags! {
    #[derive(Debug)]
    pub struct TlbShootdownActions: usize {
        // Delay the deallocation of one or more contiguous frames.
        const FREE = 1;

        // Revoke various access flags from a page
        const REVOKE_READ = 1 << 1;
        const REVOKE_WRITE = 1 << 2;
        const REVOKE_EXEC = 1 << 3;

        // Unmap a page from one address space without deallocating it.
        const MOVE = 1 << 4;

        // Add a new mapping to an address space.
        // Not really a TLB shootdown action on most architectures, so almost always a no-op.
        const NEW_MAPPING = 1 << 31;
    }
}
impl TlbShootdownActions {
    pub fn change_of_flags(old: PageFlags<RmmA>, new: PageFlags<RmmA>) -> Self {
        let mut this = Self::empty();
        this.set(Self::REVOKE_READ, old.has_user() && !new.has_user());
        this.set(Self::REVOKE_WRITE, old.has_write() && !new.has_write());
        this.set(Self::REVOKE_EXEC, old.has_execute() && !new.has_execute());
        this
    }
}

fn discard_present_pages_in_span(
    mapper: &mut PageMapper,
    flusher: &mut Flusher,
    span: PageSpan,
) {
    if span.is_empty() {
        return;
    }
    unsafe {
        mapper.unmap_range(
            span.base.start_address(),
            span.end().start_address(),
            |_, phys, lvl, _| {
                assert_eq!(lvl, 0);
                flusher.queue(Frame::containing(phys), None, TlbShootdownActions::FREE);
            },
            None::<fn(rmm::PhysicalAddress)>,
        );
    }
}

impl AddrSpaceWrapper {
    pub fn discard_pages(
        &self,
        requested_span: PageSpan,
        token: &mut CleanLockToken,
    ) -> Result<()> {
        let mut token = token.token();
        let mut guard = self.acquire_write(token.downgrade());
        let guard = &mut *guard;

        let mut covered_pages: usize = 0;
        for (grant_base, grant_info) in guard.grants.conflicts(requested_span) {
            if grant_info.is_pinned() {
                return Err(Error::new(EBUSY));
            }
            match grant_info.provider {
                Provider::Allocated {
                    cow_file_ref: None,
                    phys_contiguous: false,
                }
                | Provider::AllocatedShared { .. }
                | Provider::External { .. }
                | Provider::FmapBorrowed { .. } => {}
                Provider::Allocated { .. } | Provider::PhysBorrowed { .. } => {
                    return Err(Error::new(EINVAL));
                }
            }
            let grant_span = PageSpan::new(grant_base, grant_info.page_count);
            covered_pages = covered_pages
                .checked_add(grant_span.intersection(requested_span).count)
                .expect("discard covered pages overflow");
        }
        let has_hole = covered_pages < requested_span.count;

        let mapper = &mut guard.table.utable;
        let mut flusher = Flusher::with_cpu_set(&self.used_by, &self.tlb_ack);

        for (grant_base, grant_info) in guard.grants.conflicts(requested_span) {
            if !matches!(
                grant_info.provider,
                Provider::Allocated {
                    cow_file_ref: None,
                    phys_contiguous: false,
                }
            ) {
                continue;
            }
            assert!(grant_info.mapped);
            let grant_span = PageSpan::new(grant_base, grant_info.page_count);
            let intersection = grant_span.intersection(requested_span);
            discard_present_pages_in_span(mapper, &mut flusher, intersection);
        }

        drop(flusher);
        if has_hole {
            return Err(Error::new(ENOMEM));
        }
        Ok(())
    }
}
