use core::{marker::PhantomData, num::NonZeroU8, ops::ControlFlow};

use crate::{
    Arch, FrameAllocator, PageEntry, PageFlags, PageFlush, PageTable, PhysicalAddress, TableKind,
    VirtualAddress,
};

const MAX_PAGE_LEVELS: usize = 5;

struct WalkFrame<A> {
    table: PageTable<A>,
    next_idx: usize,
    end_idx: usize,
    reclaim_allowed: bool,
}

fn validate_range<A: Arch>(start: VirtualAddress, end: VirtualAddress) -> Option<(usize, usize)> {
    assert!(A::PAGE_LEVELS >= 1 && A::PAGE_LEVELS <= MAX_PAGE_LEVELS);
    let start_u = start.data();
    let end_u = end.data();
    assert_eq!(start_u & A::PAGE_OFFSET_MASK, 0);
    assert_eq!(end_u & A::PAGE_OFFSET_MASK, 0);
    assert!(start_u <= end_u);
    if start_u == end_u {
        return None;
    }
    let last_page = end_u - A::PAGE_SIZE;
    assert_eq!(start_u & A::PAGE_ADDRESS_MASK, start_u);
    assert_eq!(last_page & A::PAGE_ADDRESS_MASK, last_page);
    Some((start_u, last_page))
}

fn level_bounds<A: Arch>(table: &PageTable<A>, start_u: usize, last_page: usize) -> (usize, usize) {
    let shift = table.level() * A::PAGE_ENTRY_SHIFT + A::PAGE_SHIFT;
    let table_base = table.base().data();
    let table_span_minus_page = A::PAGE_ENTRIES
        .wrapping_shl(shift as u32)
        .wrapping_sub(A::PAGE_SIZE);
    let table_last_page = table_base.wrapping_add(table_span_minus_page);
    let sub_start = if start_u > table_base {
        start_u
    } else {
        table_base
    };
    let sub_last = if last_page < table_last_page {
        last_page
    } else {
        table_last_page
    };
    assert!(sub_start <= sub_last);
    let first_idx = (sub_start - table_base) >> shift;
    let end_idx = (sub_last - table_base) >> shift;
    assert!(first_idx <= end_idx && end_idx < A::PAGE_ENTRIES);
    (first_idx, end_idx)
}

fn is_large_entry<A: Arch>(level: usize, entry: PageEntry<A>) -> bool {
    let Some(level_nz) = NonZeroU8::new(level as u8) else {
        return false;
    };
    A::entry_flag_large(level_nz).is_some_and(|flag| (entry.data() & flag) != 0)
}

pub struct PageMapper<A, F> {
    table_kind: TableKind,
    table_addr: PhysicalAddress,
    allocator: F,
    _phantom: PhantomData<fn() -> A>,
}

impl<A: Arch, F> PageMapper<A, F> {
    unsafe fn new(table_kind: TableKind, table_addr: PhysicalAddress, allocator: F) -> Self {
        Self {
            table_kind,
            table_addr,
            allocator,
            _phantom: PhantomData,
        }
    }

    pub unsafe fn current(table_kind: TableKind, allocator: F) -> Self {
        unsafe {
            let table_addr = A::table(table_kind);
            Self::new(table_kind, table_addr, allocator)
        }
    }

    pub fn is_current(&self) -> bool {
        self.table().phys() == A::table(self.table_kind)
    }

    pub unsafe fn make_current(&self) {
        unsafe {
            A::set_table(self.table_kind, self.table_addr);
        }
    }

    pub fn table(&self) -> PageTable<A> {
        // SAFETY: The only way to initialize a PageMapper is via new(), and we assume it upholds
        // all necessary invariants for this to be safe.
        unsafe { PageTable::new(VirtualAddress::new(0), self.table_addr, A::PAGE_LEVELS - 1) }
    }

    pub fn allocator(&self) -> &F {
        &self.allocator
    }

    pub fn allocator_mut(&mut self) -> &mut F {
        &mut self.allocator
    }

    fn visit<T>(
        &self,
        virt: VirtualAddress,
        f: impl FnOnce(&mut PageTable<A>, usize) -> T,
    ) -> Option<T> {
        let mut table = self.table();
        loop {
            let i = table.index_of(virt)?;

            match (NonZeroU8::new(table.level().try_into().unwrap()), unsafe {
                table.entry(i)
            }) {
                // leaf
                (None, _) => return Some(f(&mut table, i)),
                // leaf (large/huge page)
                (Some(level), Some(entry))
                    if A::entry_flag_large(level).is_some_and(|f| f & entry.data() != 0) =>
                {
                    return Some(f(&mut table, i))
                }
                // intermediate; continue to the next-level paging structure
                _ => table = unsafe { table.next(i)? },
            }
        }
    }

    pub fn translate(&self, virt: VirtualAddress) -> Option<(PhysicalAddress, PageFlags<A>)> {
        let entry = self.visit(virt, |p1, i| unsafe { p1.entry(i) })??;
        Some((entry.address().ok()?, entry.flags()))
    }

    pub unsafe fn remap_with_full(
        &mut self,
        virt: VirtualAddress,
        f: impl FnOnce(PhysicalAddress, PageFlags<A>) -> Option<(PhysicalAddress, PageFlags<A>)>,
    ) -> Option<(PageFlags<A>, PhysicalAddress, PageFlush<A>)> {
        unsafe {
            self.visit(virt, |p1, i| {
                let old_entry = p1.entry(i)?;
                let old_phys = old_entry.address().ok()?;
                let old_flags = old_entry.flags();
                let (new_phys, new_flags) = f(old_phys, old_flags)?;
                // TODO: Higher-level PageEntry::new interface?
                // TODO: check this can't break alignment for large/huge pages
                let new_entry = PageEntry::new(new_phys.data(), new_flags.data());
                p1.set_entry(i, new_entry);
                Some((old_flags, old_phys, PageFlush::new(virt)))
            })
            .flatten()
        }
    }

    pub unsafe fn remap_with(
        &mut self,
        virt: VirtualAddress,
        map_flags: impl FnOnce(PageFlags<A>) -> PageFlags<A>,
    ) -> Option<(PageFlags<A>, PhysicalAddress, PageFlush<A>)> {
        unsafe {
            self.remap_with_full(virt, |same_phys, old_flags| {
                Some((same_phys, map_flags(old_flags)))
            })
        }
    }

    pub unsafe fn remap(
        &mut self,
        virt: VirtualAddress,
        flags: PageFlags<A>,
    ) -> Option<PageFlush<A>> {
        unsafe { self.remap_with(virt, |_| flags).map(|(_, _, flush)| flush) }
    }

    pub fn count_mapped_pages(&self, start: VirtualAddress, end: VirtualAddress) -> usize {
        let Some((start_u, last_page)) = validate_range::<A>(start, end) else {
            return 0;
        };
        let root = self.table();
        let (first_idx, end_idx) = level_bounds::<A>(&root, start_u, last_page);
        let mut stack: [Option<WalkFrame<A>>; MAX_PAGE_LEVELS] = [None, None, None, None, None];
        stack[0] = Some(WalkFrame {
            table: root,
            next_idx: first_idx,
            end_idx,
            reclaim_allowed: false,
        });
        let mut depth: usize = 1;
        let mut mapped_pages: usize = 0;

        while depth > 0 {
            let Some(top) = stack[depth - 1].as_mut() else {
                unreachable!();
            };
            if top.next_idx > top.end_idx {
                stack[depth - 1] = None;
                depth -= 1;
                continue;
            }
            let idx = top.next_idx;
            top.next_idx += 1;
            let Some(entry) = (unsafe { top.table.entry(idx) }) else {
                continue;
            };
            if !entry.present() {
                continue;
            }
            let level = top.table.level();
            if level == 0 {
                mapped_pages += 1;
            } else if is_large_entry(level, entry) {
                let shift = level * A::PAGE_ENTRY_SHIFT + A::PAGE_SHIFT;
                let entry_start = top.table.base().data() + (idx << shift);
                let entry_last = entry_start + ((1usize << shift) - A::PAGE_SIZE);
                let lo = if start_u > entry_start {
                    start_u
                } else {
                    entry_start
                };
                let hi = if last_page < entry_last {
                    last_page
                } else {
                    entry_last
                };
                mapped_pages += ((hi - lo) >> A::PAGE_SHIFT) + 1;
            } else {
                let Some(child) = (unsafe { top.table.next(idx) }) else {
                    continue;
                };
                let (c_first, c_end) = level_bounds::<A>(&child, start_u, last_page);
                assert!(depth < A::PAGE_LEVELS);
                stack[depth] = Some(WalkFrame {
                    table: child,
                    next_idx: c_first,
                    end_idx: c_end,
                    reclaim_allowed: false,
                });
                depth += 1;
            }
        }

        mapped_pages
    }

    pub unsafe fn try_remap_range_with_full<E>(
        &mut self,
        start: VirtualAddress,
        end: VirtualAddress,
        mut remap: impl FnMut(PhysicalAddress, PageFlags<A>) -> Option<(PhysicalAddress, PageFlags<A>)>,
        mut on_remapped: impl FnMut(
            VirtualAddress,
            PhysicalAddress,
            PageFlags<A>,
        ) -> Result<ControlFlow<()>, E>,
    ) -> Result<(), E> {
        let Some((start_u, last_page)) = validate_range::<A>(start, end) else {
            return Ok(());
        };
        let root = self.table();
        let (first_idx, end_idx) = level_bounds::<A>(&root, start_u, last_page);
        let mut stack: [Option<WalkFrame<A>>; MAX_PAGE_LEVELS] = [None, None, None, None, None];
        stack[0] = Some(WalkFrame {
            table: root,
            next_idx: first_idx,
            end_idx,
            reclaim_allowed: false,
        });
        let mut depth: usize = 1;

        while depth > 0 {
            let Some(top) = stack[depth - 1].as_mut() else {
                unreachable!();
            };
            if top.next_idx > top.end_idx {
                stack[depth - 1] = None;
                depth -= 1;
                continue;
            }
            let idx = top.next_idx;
            top.next_idx += 1;
            let Some(entry) = (unsafe { top.table.entry(idx) }) else {
                continue;
            };
            if !entry.present() {
                continue;
            }
            let level = top.table.level();
            if level == 0 {
                let Ok(old_phys) = entry.address() else {
                    continue;
                };
                let old_flags = entry.flags();
                let Some((new_phys, new_flags)) = remap(old_phys, old_flags) else {
                    continue;
                };
                let new_entry = PageEntry::new(new_phys.data(), new_flags.data());
                unsafe {
                    top.table.set_entry(idx, new_entry);
                }
                let virt = top.table.entry_base(idx).expect("leaf index in bounds");
                if let ControlFlow::Break(()) = on_remapped(virt, old_phys, old_flags)? {
                    return Ok(());
                }
            } else {
                assert!(
                    !is_large_entry(level, entry),
                    "huge page encountered in user range remap"
                );
                let Some(child) = (unsafe { top.table.next(idx) }) else {
                    continue;
                };
                let (c_first, c_end) = level_bounds::<A>(&child, start_u, last_page);
                assert!(depth < A::PAGE_LEVELS);
                stack[depth] = Some(WalkFrame {
                    table: child,
                    next_idx: c_first,
                    end_idx: c_end,
                    reclaim_allowed: false,
                });
                depth += 1;
            }
        }

        Ok(())
    }

    pub unsafe fn remap_range_with_full(
        &mut self,
        start: VirtualAddress,
        end: VirtualAddress,
        remap: impl FnMut(PhysicalAddress, PageFlags<A>) -> Option<(PhysicalAddress, PageFlags<A>)>,
        mut on_remapped: impl FnMut(VirtualAddress, PhysicalAddress, PageFlags<A>),
    ) {
        let _ = unsafe {
            self.try_remap_range_with_full::<core::convert::Infallible>(
                start,
                end,
                remap,
                |virt, phys, old_flags| {
                    on_remapped(virt, phys, old_flags);
                    Ok(ControlFlow::Continue(()))
                },
            )
        };
    }

    unsafe fn unmap_range_inner<FTable>(
        &mut self,
        start: VirtualAddress,
        end: VirtualAddress,
        max_leaves: usize,
        mut on_leaf: impl FnMut(VirtualAddress, PhysicalAddress, u8, PageFlags<A>),
        mut free_table: Option<FTable>,
    ) -> VirtualAddress
    where
        FTable: FnMut(PhysicalAddress),
    {
        assert!(max_leaves >= 1);
        let Some((start_u, last_page)) = validate_range::<A>(start, end) else {
            return end;
        };
        let root = self.table();
        let (first_idx, end_idx) = level_bounds::<A>(&root, start_u, last_page);
        let mut stack: [Option<WalkFrame<A>>; MAX_PAGE_LEVELS] = [None, None, None, None, None];
        stack[0] = Some(WalkFrame {
            table: root,
            next_idx: first_idx,
            end_idx,
            reclaim_allowed: false,
        });
        let mut depth: usize = 1;
        let mut unmapped_count: usize = 0;

        while depth > 0 {
            let Some(top) = stack[depth - 1].as_mut() else {
                unreachable!();
            };
            if top.next_idx > top.end_idx {
                let finished = stack[depth - 1].take().expect("active walk frame");
                depth -= 1;
                if depth > 0
                    && finished.reclaim_allowed
                    && let Some(ref mut free_fn) = free_table
                {
                    let is_empty = !(0..A::PAGE_ENTRIES).any(|j| {
                        unsafe { finished.table.entry(j) }.is_some_and(|e| e.present())
                    });
                    if is_empty {
                        let parent = stack[depth - 1].as_mut().expect("parent walk frame");
                        let parent_idx = parent.next_idx - 1;
                        unsafe {
                            parent.table.set_entry(parent_idx, PageEntry::new(0, 0));
                        }
                        free_fn(finished.table.phys());
                    }
                }
                continue;
            }
            let idx = top.next_idx;
            top.next_idx += 1;
            let Some(entry) = (unsafe { top.table.entry(idx) }) else {
                continue;
            };
            if !entry.present() {
                continue;
            }
            let level = top.table.level();
            if level == 0 {
                let Ok(phys) = entry.address() else {
                    continue;
                };
                let virt = top.table.entry_base(idx).expect("leaf index in bounds");
                if unmapped_count == max_leaves {
                    return virt;
                }
                let flags = entry.flags();
                unsafe {
                    top.table.set_entry(idx, PageEntry::new(0, 0));
                }
                unmapped_count += 1;
                on_leaf(virt, phys, 0, flags);
            } else {
                assert!(
                    !is_large_entry(level, entry),
                    "huge page encountered in user range unmap"
                );
                let child_reclaim = if depth == 1 {
                    A::KERNEL_SEPARATE_TABLE || idx < A::PAGE_ENTRIES / 2
                } else {
                    top.reclaim_allowed
                };
                let Some(child) = (unsafe { top.table.next(idx) }) else {
                    continue;
                };
                let (c_first, c_end) = level_bounds::<A>(&child, start_u, last_page);
                assert!(depth < A::PAGE_LEVELS);
                stack[depth] = Some(WalkFrame {
                    table: child,
                    next_idx: c_first,
                    end_idx: c_end,
                    reclaim_allowed: child_reclaim,
                });
                depth += 1;
            }
        }

        end
    }

    pub unsafe fn unmap_range<FTable>(
        &mut self,
        start: VirtualAddress,
        end: VirtualAddress,
        on_leaf: impl FnMut(VirtualAddress, PhysicalAddress, u8, PageFlags<A>),
        free_table: Option<FTable>,
    ) where
        FTable: FnMut(PhysicalAddress),
    {
        let next = unsafe { self.unmap_range_inner(start, end, usize::MAX, on_leaf, free_table) };
        assert_eq!(next, end);
    }

    pub unsafe fn unmap_range_batch<FTable>(
        &mut self,
        start: VirtualAddress,
        end: VirtualAddress,
        batch: &mut [(VirtualAddress, PhysicalAddress, u8, PageFlags<A>)],
        free_table: Option<FTable>,
    ) -> (usize, VirtualAddress)
    where
        FTable: FnMut(PhysicalAddress),
    {
        assert!(!batch.is_empty());
        let mut count: usize = 0;
        let next = unsafe {
            self.unmap_range_inner(
                start,
                end,
                batch.len(),
                |virt, phys, lvl, flags| {
                    batch[count] = (virt, phys, lvl, flags);
                    count += 1;
                },
                free_table,
            )
        };
        (count, next)
    }
}

impl<A: Arch, F: FrameAllocator> PageMapper<A, F> {
    pub unsafe fn create(table_kind: TableKind, mut allocator: F) -> Option<Self> {
        unsafe {
            let table_addr = allocator.allocate_one()?;
            let mut table = Self::new(table_kind, table_addr, allocator);

            match (table_kind, A::KERNEL_SEPARATE_TABLE) {
                (TableKind::Kernel, false) => {
                    // Pre-allocate all kernel top-level page table entries so that when
                    // the page table is copied, these entries are synced between processes.
                    for i in A::PAGE_ENTRIES / 2..A::PAGE_ENTRIES {
                        let phys = table
                            .allocator
                            .allocate_one()
                            .expect("failed to map page table");
                        let flags = A::ENTRY_FLAG_DEFAULT_TABLE;
                        table
                            .table()
                            .set_entry(i, PageEntry::new(phys.data(), flags));
                    }
                }
                (TableKind::User, false) => {
                    // Copy higher half (kernel) mappings
                    let active_ktable = PageMapper::current(TableKind::Kernel, ());
                    for i in A::PAGE_ENTRIES / 2..A::PAGE_ENTRIES {
                        if let Some(entry) = active_ktable.table().entry(i) {
                            table.table().set_entry(i, entry);
                        }
                    }
                }
                (_, true) => {
                    // There is a separate page table for the kernel. No need to copy the kernel
                    // mappings to the user page table.
                }
            }

            Some(table)
        }
    }

    pub unsafe fn map_phys(
        &mut self,
        virt: VirtualAddress,
        phys: PhysicalAddress,
        flags: PageFlags<A>,
        requested_level: u8,
    ) -> Option<PageFlush<A>> {
        unsafe {
            // TODO: verify flags have correct bits, and that these are valid for the specific
            // level

            let required_align = A::PAGE_SHIFT + usize::from(requested_level) * A::PAGE_ENTRY_SHIFT;
            assert_eq!(
                (virt.data() >> required_align) << required_align,
                virt.data(),
                "unaligned virtual address"
            );
            assert_eq!(
                (phys.data() >> required_align) << required_align,
                phys.data(),
                "unaligned physical address"
            );

            let mut entry = PageEntry::new(phys.data(), flags.data());
            if let Some(level_nz) = NonZeroU8::new(requested_level) {
                let flag = A::entry_flag_large(level_nz)
                    .expect("trying to map at a level not supported by hw");
                entry.set_flags(entry.flags().custom_flag(flag, true));
            }

            let mut table = self.table();
            loop {
                let i = table.index_of(virt)?;
                if table.level() == usize::from(requested_level) {
                    //TODO: check for overwriting entry
                    table.set_entry(i, entry);
                    return Some(PageFlush::new(virt));
                }

                if let Some(entry) = table.entry(i)
                    && let Some(flag) = NonZeroU8::new(table.level().try_into().unwrap())
                        .and_then(A::entry_flag_large)
                {
                    assert_eq!(
                        entry.flags().data() & flag,
                        0,
                        "trying to interpret huge page as subtable"
                    );
                }

                let next = match table.next(i) {
                    Some(some) => some,
                    None => {
                        let next_phys = self.allocator.allocate_one()?;
                        //TODO: correct flags?
                        let flags = A::ENTRY_FLAG_DEFAULT_TABLE
                            | if virt.kind() == TableKind::User {
                                A::ENTRY_FLAG_TABLE_USER
                            } else {
                                0
                            };
                        table.set_entry(i, PageEntry::new(next_phys.data(), flags));
                        table.next(i)?
                    }
                };
                table = next;
            }
        }
    }

    pub unsafe fn map_linearly(
        &mut self,
        phys: PhysicalAddress,
        flags: PageFlags<A>,
        requested_level: u8,
    ) -> Option<(VirtualAddress, PageFlush<A>)> {
        unsafe {
            let virt = A::phys_to_virt(phys);
            self.map_phys(virt, phys, flags, requested_level)
                .map(|flush| (virt, flush))
        }
    }

    pub unsafe fn unmap_phys(
        &mut self,
        virt: VirtualAddress,
    ) -> Option<(PhysicalAddress, u8, PageFlags<A>, PageFlush<A>)> {
        //TODO: verify virt is aligned
        let mut table = self.table();

        let unmap_parents = A::KERNEL_SEPARATE_TABLE || table.index_of(virt)? < A::PAGE_ENTRIES / 2; // Is a userspace mapping

        unsafe {
            unmap_phys_inner(virt, &mut table, unmap_parents, &mut self.allocator)
                // FIXME: may need to construct multiple page flushes if the level is higher
                .map(|(pa, lvl, pf)| (pa, lvl, pf, PageFlush::new(virt)))
        }
    }
}

unsafe fn unmap_phys_inner<A: Arch>(
    virt: VirtualAddress,
    table: &mut PageTable<A>,
    unmap_parents: bool,
    allocator: &mut impl FrameAllocator,
) -> Option<(PhysicalAddress, u8, PageFlags<A>)> {
    unsafe {
        let i = table.index_of(virt)?;
        let entry_opt = table.entry(i);

        let mut subtable = match (NonZeroU8::new(table.level().try_into().unwrap()), entry_opt) {
            (None, _) => {
                table.set_entry(i, PageEntry::new(0, 0));
                let entry = entry_opt?;

                return Some((entry.address().ok()?, 0, entry.flags()));
            }
            (Some(lvl), Some(entry))
                if A::entry_flag_large(lvl).is_some_and(|f| f & entry.flags().data() != 0) =>
            {
                table.set_entry(i, PageEntry::new(0, 0));
                let entry = entry_opt?;

                return Some((entry.address().ok()?, lvl.get(), entry.flags()));
            }
            (Some(_), _) => table.next(i)?,
        };

        let res = unmap_phys_inner(virt, &mut subtable, unmap_parents, allocator)?;

        if unmap_parents {
            // TODO: Use a counter? This would reduce the remaining number of available bits, but could be
            // faster (benchmark is needed).
            let is_still_populated = (0..A::PAGE_ENTRIES)
                .map(|j| subtable.entry(j).expect("must be within bounds"))
                .any(|e| e.present());

            if !is_still_populated {
                allocator.free_one(subtable.phys());
                table.set_entry(i, PageEntry::new(0, 0));
            }
        }

        Some(res)
    }
}

impl<A, F: core::fmt::Debug> core::fmt::Debug for PageMapper<A, F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PageMapper")
            .field("frame", &self.table_addr)
            .field("allocator", &self.allocator)
            .finish()
    }
}
