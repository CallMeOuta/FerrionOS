# Sparse page-table walks, resident memory accounting and deferred page-table freeing

Status: accepted

## Context

GrapheneOS `hardened_malloc` reserves 32 GiB per size class across 49 classes and 4 arenas (12.25 TiB total virtual address space) with `mmap(PROT_NONE)` and activates individual slabs with `mprotect`. Walking every 4 KiB virtual page in `Grant::unmap`, `Grant::remap`, `Grant::copy_mappings`, `Grant::transfer`, and `/scheme/sys/memstat` makes unmapping, forking, remapping, and inspecting a sparse grant linear in its virtual size rather than its populated tables. Eagerly mapping 16 zero pages in `Grant::zeroed` allocates intermediate page tables even for `PROT_NONE` reservations, and `/scheme/sys/context` previously reported virtual span size rather than resident memory.

## Decision

- `rmm::PageMapper` (`rmm/src/page/mapper.rs`) provides iterative, fixed-stack (`MAX_PAGE_LEVELS = 5`, no recursion) range walkers: `count_mapped_pages`, `try_remap_range_with_full`, `remap_range_with_full`, `unmap_range`, and `unmap_range_batch`. Each walker skips absent upper-level page-table entries, enforces canonical page-aligned bounds, and asserts that no huge-page entry appears in a user range (`count_mapped_pages` counts large entries if encountered).
- `unmap_range` and `unmap_range_batch` accept an optional `free_table: Option<impl FnMut(PhysicalAddress)>` sink. Passing `None` leaves intermediate tables intact without scanning or unlinking them (for in-place page discard). Passing `Some(sink)` scans every visited user subtable on pop and unlinks it only when all entries are non-present, reclaiming each emptied subtable at most once and never reclaiming top-level kernel entries (`A::KERNEL_SEPARATE_TABLE || top_idx < A::PAGE_ENTRIES / 2`).
- `Grant::unmap`, `Grant::remap`, `Grant::copy_mappings` (`CopyMappingsMode::Owned`), `Grant::transfer` (in bounded batches of `TRANSFER_BATCH_PAGES = 64`, asserting disjointness when `dst_mapper.is_none()`), `/scheme/sys/memstat`, and `/scheme/sys/context` use these walkers. `CopyMappingsMode::Borrowed` (`MAP_SHARED | MAP_ANONYMOUS`) remains page-by-page eager because unmapped shared anonymous pages must allocate shared frames at `fork()`.
- `Grant::zeroed` skips its eager `MAX_EAGER_PAGES` zero-frame loop when `!flags.has_user()`, so `PROT_NONE` grants allocate zero intermediate page tables on creation while accessible grants keep their existing behavior.
- `/scheme/sys/context` reports resident memory by counting present page-table entries per grant via `count_mapped_pages` (`PhysBorrowed` remains excluded). Present entries include eagerly mapped zero frames on accessible grants and count shared frames in each process that maps them.
- Emptied user page-table frames are returned to the frame allocator only after the TLB shootdown of the operation that emptied them has completed: `Grant::unmap` (both contiguous and non-contiguous branches) and `Grant::transfer` pass a table sink that atomically promotes the emptied table frame's `PageInfo` from `RC_USED_NOT_FREE` to `RefCount::One` (rejecting already-free or user-referenced frames) and queues `flusher.queue(table_frame, None, TlbShootdownActions::FREE)`. `Flusher::flush()` runs local invalidation and waits for remote CPU acknowledgements before calling `handle_free_action`, which drops that reference and returns the order-0 frame via `deallocate_frame`. `NopFlusher` frees immediately because it is used only for dead or not-yet-active address spaces (`AddrSpace::inner_drop`, `try_clone`), relying on the invariant that switching `CR3` without PCID clears paging-structure caches.
- A permanent ordering check tracks per-CPU atomic `live_flushers` (`+1` on the creating CPU in `Flusher::with_cpu_set` and `Flusher::reattach`, `-1` on that recorded owner CPU in `Drop` and `detach` so context migration across CPUs never underflows): any immediate table free through `TheFrameAllocator::free` while the current CPU's `live_flushers > 0` increments a global counter and logs one `error!` line with the count and caller location.

## Consequences

- `munmap`, `mprotect`, `fork` of private mappings, `r#move`, `/scheme/sys/memstat`, and `/scheme/sys/context` scale with populated page tables rather than virtual reservation size.
- Deferring page-table frame frees through `Flusher::queue` uses slots in `Flusher`'s existing 32-entry queue without modifying `Flusher` or `rmm`'s walker.
- If PCID support is added to the kernel in the future, `NopFlusher` and address-space teardown must be revisited because switching `CR3` with PCID enabled does not automatically flush paging-structure caches for other PCIDs.
- `Grant::copy_mappings` preserves its existing behavior of breaking out of the loop and returning a partial grant when `dst_mapper.map_phys` fails on OOM.

## Where

- `recipes/core/kernel/recipe.toml`
- `recipes/core/kernel/ferrion-sparse-page-tables.patch`
- `recipes/core/kernel/ferrion-defer-pt-free.patch`
- `recipes/tests/hardened-malloc-probe/hardened-malloc-probe.c`
- `scripts/vm-test.py` (`test_hardened_malloc_sparse`, `test_hardened_malloc_pt_order`)
