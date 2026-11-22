# In-place private anonymous page discard, madvise and POSIX abort

Status: accepted

## Context

GrapheneOS `hardened_malloc` (security roadmap S14) relies on three OS behaviours that FerrionOS lacked: `madvise(MADV_DONTNEED_LOCKED)` must discard physical pages of private anonymous regions in place while keeping the virtual reservation and protection flags intact so the next touch faults in zeroes; `madvise` must write nothing to stderr (previously `relibc` logged a `todo_skip!` line on every call, breaking exact-stderr tests and slowing slab purges); and `abort()` must raise `SIGABRT` (`-6`) with empty stderr rather than logging `[ERROR ... ] Abort` and trapping `SIGILL` (`-4`).

Without real in-place discard, `hardened_malloc` (32 MiB x 1 arena) retains 62,832 KiB (`default`) and 51,188 KiB (`light`) after freeing all benchmark allocations (compared with 14,868 KiB `default` and 2,608 KiB `light` on Linux), logs ~231 KiB of `todo_skip!` lines per benchmark run (`med_ms` 8,852 ms `default`, 8,863 ms `light`), and passes only 31 of 58 strict `test_smc.py` tests because the 27 abort tests receive `SIGILL` plus log output.

## Decision

### Single-purpose kernel syscall `SYS_MADVISE` (219)

The kernel exposes a single direct system call `SYS_MADVISE = 219` (`address`, `size`, `flags`) that discards private anonymous pages in place when `flags == 0` and rejects any non-zero `flags` or non-page-aligned `address` with `EINVAL`. A zero `size` returns `0` after validating `address` and `flags`.

- A direct system call is chosen over overloading `SYS_MPROTECT`/`SYS_MREMAP` flags (which would conflate protection changes or moves with discard) and over a proc-scheme `ADDRSPACE_OP_*` command (which would require an open address-space handle and scheme dispatch on every allocator slab purge, and would fail in namespaces without that scheme).
- `relibc` pins its `redox_syscall` dependency to upstream's kernel git (`7824a967`) rather than FerrionOS's kernel `syscall/` crate, so `relibc` defines `const SYS_MADVISE: usize = 219` locally in `src/platform/redox/mod.rs` and invokes `syscall::syscall3`, matching `syscall/src/number.rs` in the kernel.
- **Upstream rebase risk:** number `219` is unused in FerrionOS's kernel (`125`, `155`, `158`, `162`, `240`, `265`) and in upstream's pinned `redox_syscall` crate. If a future upstream rebase assigns `219` to another system call, the rebase updates the single constant in both `kernel/syscall/src/number.rs` and `relibc/src/platform/redox/mod.rs` together (or adopts upstream's discard/`madvise` number if upstream adds one) and rebuilds the kernel and `relibc` in lockstep. After the monorepo cutover (ADR 0027), the kernel and `relibc` share one repository and can share a single constant directly.

### Grant validation and page-table discard (`AddrSpaceWrapper::discard_pages`)

`AddrSpaceWrapper::discard_pages` takes the address-space write lock, allocates nothing on the heap, and validates every grant overlapping the requested `PageSpan` before unmapping any page so a provider or pin error leaves no partial state:

- **Private anonymous (`Provider::Allocated { cow_file_ref: None, phys_contiguous: false }`), not pinned:** present leaf pages in the intersection are unmapped via `rmm`'s `PageMapper::unmap_range` with no table sink (`free_table: None`) and queued on `Flusher::with_cpu_set(&self.used_by, &self.tlb_ack)` with `TlbShootdownActions::FREE`. Frames (including copy-on-write frames shared with a fork child and the shared zero frame) are released only after TLB shootdown via `handle_free_action`. The `Grant` stays in `UserGrants` unsplit with `mapped: true` and its existing `PageFlags` (including `PROT_NONE`), so the next permitted touch faults a fresh zero page through `correct_inner` -> `map_zeroed`, and a `PROT_NONE` grant continues to fault with `SIGSEGV`.
- **Pinned grants (`is_pinned()`):** refused with `EBUSY` before discarding anything, because a scheme may be reading or writing the pinned frames.
- **Private file mappings (`Provider::Allocated { cow_file_ref: Some(_), .. }`), physically contiguous DMA grants (`phys_contiguous: true`), and `Provider::PhysBorrowed`:** refused with `EINVAL` before discarding anything, because `correct_inner` would fault zeroes instead of re-reading a private file and DMA or borrowed physical frames cannot be faulted back lazily.
- **Shared grants (`Provider::AllocatedShared`, `Provider::External`, `Provider::FmapBorrowed`):** left untouched and accepted, matching what Linux programs observe after `MADV_DONTNEED` on shared mappings.
- **Unmapped holes (`mapped / hole / mapped`):** all valid private anonymous portions in the span are discarded and flushed, then `ENOMEM` is returned, matching Linux `madvise(2)`.
- **Page tables kept allocated and sparse walk:** discard clears present leaf entries through `rmm`'s `PageMapper::unmap_range` with no table sink (`free_table: None`), asserting `lvl == 0` on every unmapped leaf. Absent upper-level entries are skipped so sparse gigabyte reservations stay proportional to populated tables, and no page table is freed during discard, matching Linux `MADV_DONTNEED`, avoiding page-table teardown on slab reuse, and avoiding any page-table free before a TLB shootdown completes.

### `relibc` `madvise`, `posix_madvise` and `<sys/mman.h>`

- `src/header/sys_mman/mod.rs` defines `MADV_FREE = 8` and `MADV_DONTNEED_LOCKED = 24` alongside `MADV_DONTNEED = 4` so C callers (including `hardened_malloc`) receive them from `<sys/mman.h>`.
- `Sys::madvise` in `src/platform/redox/mod.rs` removes `todo_skip!` and writes nothing to stderr. It rejects unaligned `addr` with `EINVAL`, rounds `len` up to `PAGE_SIZE` (overflow -> `EINVAL`), dispatches `MADV_DONTNEED`, `MADV_DONTNEED_LOCKED`, and `MADV_FREE` to `SYS_MADVISE` with `flags = 0`, returns `0` for pure advisory hints (`MADV_NORMAL`, `MADV_RANDOM`, `MADV_SEQUENTIAL`, `MADV_WILLNEED`, `MADV_DONTDUMP`, `MADV_DODUMP`), and returns `EINVAL` for unknown or behaviour-promising advice (such as `MADV_WIPEONFORK`, `MADV_DONTFORK`, or `MADV_REMOVE`) so callers such as OpenSSL never receive a silent success for an unsupported guarantee.
- `posix_madvise` validates `advice` against `POSIX_MADV_NORMAL..=POSIX_MADV_DONTNEED` (returning `EINVAL` directly, not `-1`) and is a no-op on FerrionOS so `POSIX_MADV_DONTNEED` (`4`) never destroys memory contents, matching POSIX and glibc.

### `relibc` POSIX `abort()`

`abort()` in `src/header/stdlib/mod.rs` removes `log::error!("Abort")`, unblocks `SIGABRT` in the calling thread (`sigprocmask(SIG_UNBLOCK, ...)`), and calls `raise(SIGABRT)`. If `raise` returns (a catching handler returned or `SIGABRT` was `SIG_IGN`), it installs `SIG_DFL` via `sigaction`, unblocks `SIGABRT`, and calls `raise(SIGABRT)` again, falling back to `intrinsics::abort()` only if still alive. It performs no heap allocation, touches no stdio streams, takes no allocator locks, and writes nothing to stderr, making it safe to call from `hardened_malloc`'s `fatal_error` while allocator locks are held.

## Consequences

- Purged `hardened_malloc` slabs and quarantined large regions return physical frames immediately instead of leaking until process exit, at the expected cost of zero-fill page faults when purged slabs are later reused (matching Linux).
- `hardened_malloc`'s upstream 58-test suite (`test_smc.py` via `hardened-malloc-probe run-suite`) can run strictly with exact `SIGABRT` (`-6`) and exact stderr assertions.
- Rebase coordination is required only for syscall number `219` until the monorepo cutover unifies the kernel and `relibc` syscall definitions.
- `mremap` remains separate and deferred until discard and `abort` land.

## Where

- `recipes/core/kernel/ferrion-madvise-discard.patch` (`syscall/src/number.rs`, `src/syscall/mod.rs`, `src/syscall/process.rs`, `src/context/memory.rs`)
- `recipes/core/relibc/ferrion-madvise.patch` (`src/header/sys_mman/mod.rs`, `src/platform/redox/mod.rs`)
- `recipes/core/relibc/ferrion-abort-sigabrt.patch` (`src/header/stdlib/mod.rs`)
- `recipes/tests/hardened-malloc-probe/hardened-malloc-probe.c` (`discard` subcommand) and `scripts/vm-test.py` (`test_hardened_malloc_discard`)
