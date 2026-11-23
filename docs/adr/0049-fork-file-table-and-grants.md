# Multithreaded fork, exec and posix_spawn file-table synchronization and grant snapshots

Status: accepted

## Context

In `relibc` (`redox-rt` and `src/platform/redox`), `fork`, `exec`, and `posix_spawn` run while sibling threads open, close, duplicate, map, and allocate. The child must start with a file table that matches its kernel table, the allocator and runtime locks in a usable state, and only the descriptors it is meant to keep; a program started by `exec` or `posix_spawn` must receive no close-on-exec descriptor; a child whose setup fails must not be left behind; and `fork` must copy the parent's shared mappings it lists consistently.

## Decision

- **Fork lock order and allocator locking (`ferrion-fork-filetable.patch`):** `fork_inner` pre-reserves upper slots before locking, then holds `DYNAMIC_PROC_INFO -> FILETABLE.inner -> FILETABLE.fd -> ALLOCATOR -> SIGACTIONS_LOCK` across raw `SYS_DUP_INTO` (`"copy"`, `"addrspace"`, `"exclusive"`). Allocator hooks are installed through a single `AtomicPtr<ForkAllocHooks>`. All parent paths release locks in reverse order via RAII guards; the child unlocks all five locks in `child_hook_common` before closing `cur_filetable_fd` across `x86_64`, `aarch64`, and `riscv64gc`. `ALLOCATOR` is held across the address-space copy as glibc does so the child inherits a consistent allocator state.
- **Child file-table reconciliation (`ferrion-fork-filetable.patch`):** In `child_hook_common`, `reconcile_after_fork` seeks `kernel_filetable_fd` to offset 0 (`lseek(0, SEEK_SET)`), reads the child's kernel descriptor list via `FileTableVerb::Refresh` into a stack bitset (for descriptors below 4096), closes kernel-only slots other than handed-over fork handles (`new_thr_fd`, `new_proc_fd`, `new_filetable_fd`, `cur_filetable_fd`), drops table-only entries, and preserves `O_CLOEXEC` flags on surviving descriptors.
- **Fail-closed close-on-exec in `exec` and `posix_spawn` (`ferrion-fork-filetable.patch`):** Both `fexec_impl` and `Sys::spawn` ensure that exec'd and spawned programs receive no close-on-exec descriptors. `Sys::spawn` holds `FILETABLE.inner -> FILETABLE.fd` across pre-reserved raw `dup("copy")` and snapshotting close-on-exec descriptors, updates that list across `file_actions`, omits `executable` (not placed in `new_file_table`), excludes `ExtraInfo` handles, and clones `new_file_table` into the child after closing close-on-exec entries. Both paths attempt batch `FileTableVerb::Close` and fall back to per-descriptor close accepting `EBADF` only; any other close error fails closed (`posix_exit(127)` in `fexec_impl`; error return with child cleanup in `Sys::spawn`).
- **Descriptor ownership and `pipe2` (`ferrion-fork-filetable.patch`):** `fork_inner` consumes `new_filetable_weak_fd` with `take()` on its `CallFlags::FD` move; `execve` moves the raw descriptor out and forgets `image_file` before `FdGuard::to_upper()` so each descriptor has one owner on both success and error paths; `pipe2` sets `O_CLOEXEC` atomically on the write end.
- **Bounded cleanup of half-created children (`ferrion-fork-filetable.patch`, `ferrion-fork-grants.patch`):** `ChildCleanupGuard` is declared after `new_proc_fd` (so `proc_fd` remains open during drop), armed after `new_child_process` in both `fork_inner` and `Sys::spawn`, and disarmed only after `start` succeeds. On error, `Drop` sends `ProcCall::Exit` (`SIGKILL << 8`) directly on `proc_fd` (with `posix_kill(SIGKILL)` fallback) and polls `waitpid` with `WaitFlags::WNOHANG` up to `FORK_MAX_REAP_POLLS` (`200`) iterations separated by `FORK_REAP_SLEEP_NS` (`1 ms`), per [ADR 0024](0024-tigerstyle.md). If the bound is hit, `redox-rt` logs one line and leaves the child for later parent waits or `init`.
- **Bounded grant snapshot (`ferrion-fork-grants.patch`):** `fork_inner` reads the full parent grant list in `FORK_GRANT_READ_CHUNK` (`16`) chunks up to `FORK_MAX_GRANTS` (`16_384`) and `FORK_MAX_GRANT_READS` (`1025`), repeating up to `FORK_MAX_GRANT_SNAPSHOT_PASSES` (`32`) passes until two consecutive full reads match identically before mapping `GRANT_SCHEME | GRANT_SHARED` entries. `grant-fd-` names are formatted on the stack; `grant-fd` `EINVAL` (entry unmapped after snapshot) is skipped while `mmap_remote` `EEXIST` fails the fork.

## Consequences

- Multithreaded `fork+exec` and `posix_spawn` maintain consistent kernel and userspace file tables, pass no close-on-exec descriptors to exec'd or spawned programs, and leave no half-created `KB` children behind when setup fails.
- Calling `fork` from a signal handler that interrupted `malloc` can deadlock on `ALLOCATOR`, as glibc does (POSIX 2024 removed `fork` from async-signal-safe functions).
- Because `fork_inner` reads the parent grant list after copying the address space, `MAP_SHARED` mappings added or removed in that window are not covered; closing that window requires kernel support or a `MAP_SHARED` lock (tracked in `.scratch/firefox/issues/07-fork-shared-mapping-window.md`).
- Any future lock acquired inside `fork_inner` or `Sys::spawn` must preserve the order `DYNAMIC_PROC_INFO -> FILETABLE.inner -> FILETABLE.fd -> ALLOCATOR -> SIGACTIONS_LOCK`, and any new hand-over descriptor kept across `reconcile_after_fork` must be added to `keep_fds`.

## Where

- `recipes/core/relibc/ferrion-fork-filetable.patch`, `recipes/core/relibc/ferrion-fork-grants.patch`, `recipes/core/relibc/recipe.toml`
- `recipes/tests/fork-filetable-probe/fork-filetable-probe.c`, `recipes/tests/fork-filetable-probe/fork-filetable-checker.c`, `recipes/tests/fork-filetable-probe/recipe.toml`
- `config/x86_64/desktop.toml`, `scripts/vm-test.py` (`test_fork_filetable`)
