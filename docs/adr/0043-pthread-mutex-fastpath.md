# Pthread mutex fast path and private futex wake

Status: accepted

## Context

Profiling `hardened_malloc` against Linux on FerrionOS showed that `relibc`'s `RlctMutex` (`src/sync/pthread_mutex.rs`) was the dominant cost on small allocations:

- `RlctMutex::unlock` stored `STATE_UNLOCKED` and unconditionally invoked `futex_wake(&self.inner, i32::MAX)` on every unlock (its `WAITING_BIT` tracking code was commented out), turning every uncontended unlock into a kernel system call.
- `RlctMutex::lock_inner` and `RlctMutex::try_lock` called `os_tid_invalid_after_fork()` on every acquisition, issuing a general-dynamic TLS lookup (`__tls_get_addr`) even for non-robust `PTHREAD_MUTEX_NORMAL` and `PTHREAD_MUTEX_DEFAULT` mutexes that never inspect an owner thread ID.
- The kernel's `SYS_FUTEX` handler (`src/syscall/futex.rs`) called `try_correcting_page_tables` (acquiring the address-space write lock, remapping PTEs, and flushing) and translated page tables even on `FUTEX_WAKE` for private `Provider::Allocated` memory whose `FutexKey::Private` depends only on the address space and virtual address.

## Decision

- `relibc` (`ferrion-pthread-mutex-fastpath.patch`) restores three-state `WAITING_BIT` tracking in `RlctMutex`: uncontended `lock` transitions `STATE_UNLOCKED -> owner` with a single `compare_exchange_weak` and makes no system call; a contending waiter sets `WAITING_BIT` only via `compare_exchange(thread, thread | WAITING_BIT)` when `thread & INDEX_MASK != 0` (never via `fetch_or` on a potentially unlocked word); once a thread has contended, it acquires with `owner | WAITING_BIT` so its subsequent unlock still wakes any remaining peer waiter; `unlock` atomically swaps `STATE_UNLOCKED` and calls `futex_wake(&self.inner, 1)` only when `WAITING_BIT` was set; a waiter that returns `ETIMEDOUT` calls `futex_wake(&self.inner, i32::MAX)` to drain its own stale kernel futex queue entry (the kernel leaves timed-out entries in the queue) and wake any peer so a subsequent `unlock` wake(1) cannot be absorbed by a stale entry and strand another sleeper.
- `relibc` (`ferrion-pthread-mutex-const-owner.patch`) uses the constant owner token `OWNER_ANON = 1` for non-robust `PTHREAD_MUTEX_NORMAL` and `PTHREAD_MUTEX_DEFAULT` mutexes, skipping `os_tid_invalid_after_fork()` and `__tls_get_addr` on every lock and `try_lock`. `PTHREAD_MUTEX_RECURSIVE`, `PTHREAD_MUTEX_ERRORCHECK`, and robust mutexes continue to record per-thread TIDs in `inner & INDEX_MASK`.
- The kernel (`ferrion-futex-wake-fastpath.patch`, appended after `ferrion-futex-private.patch`) fast-paths `FUTEX_WAKE` for private, user-accessible `Provider::Allocated` grants: it keeps the `USER_END_OFFSET` check and the existing read-lock-before-`FUTEXES` lock order, builds the exact `FutexKey::Private` key used by `FUTEX_WAIT` under the address-space read lock without calling `try_correcting_page_tables` or walking page tables, and lets every other grant (`PROT_NONE`, unmapped, shared) fall through to the existing path.

## Consequences

- Uncontended `pthread_mutex_lock`/`pthread_mutex_unlock` makes zero system calls and zero TLS lookups for `NORMAL` and `DEFAULT` mutexes, matching Linux's uncontended mutex cost model that `hardened_malloc` relies on.
- Because `OWNER_ANON` does not store a thread ID for non-robust `NORMAL`/`DEFAULT` mutexes, debugging tools inspecting `RlctMutex::inner` see `1` (or `1 | WAITING_BIT`) rather than a TID; POSIX specifies undefined behavior for unlocking an unowned `NORMAL`/`DEFAULT` mutex and defines ownership checks only for `ERRORCHECK`, `RECURSIVE`, and robust mutexes, which still store TIDs.
- Contended `FUTEX_WAKE` on private memory avoids taking the address-space write lock and touching page tables, keeping `FUTEX_WAIT` and `FUTEX_WAKE` keys identical.
- Any future change to `RlctMutex::unlock` that adds owner validation for `NORMAL` or `DEFAULT` mutexes must remove `OWNER_ANON` and restore per-thread TIDs.

## Where

- `recipes/core/relibc/ferrion-pthread-mutex-fastpath.patch`, `recipes/core/relibc/ferrion-pthread-mutex-const-owner.patch`, `recipes/core/relibc/recipe.toml`
- `recipes/core/kernel/ferrion-futex-wake-fastpath.patch`, `recipes/core/kernel/recipe.toml`
- `recipes/tests/hardened-malloc-probe/hardened-malloc-probe.c`, `scripts/vm-test.py` (`test_hardened_malloc_hotpath`)
