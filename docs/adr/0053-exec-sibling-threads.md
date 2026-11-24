# Ending sibling threads on exec

Status: accepted

## Context

Exec ends every other thread of the process before the new program runs. On FerrionOS, `exec` runs in userspace (`relibc`'s `redox-rt::proc::fexec_impl`) and `procmgr` (`base`'s `bootstrap/src/procmgr.rs`) owns every thread's kernel status handle and thread-death event subscription, while `libredox` continues to come from crates.io as an unmodified upstream dependency.

## Decision

- **FerrionOS-owned `procmgr` thread-handle verb range (`0x4645_xxxx`):** FerrionOS reserves thread-handle call verb numbers in the `0x4645_xxxx` namespace, outside upstream's dense `ThreadCall` discriminants starting at `0`, so `libredox` stays unmodified from crates.io without vendoring or `[patch.crates-io]` changes. `THREAD_CALL_TERMINATE_SIBLINGS = 0x4645_0001` is matched on `Handle::Thread` in `procmgr` before `ThreadCall::try_from_raw` and sent raw by `redox-rt`. The two constants must match across `components/relibc` and `components/base`, and can merge into one shared definition in-tree as a follow-up.
- **`procmgr` sibling teardown (`components/base/bootstrap/src/procmgr.rs`):** On `0x4645_0001` over `Handle::Thread`, `procmgr` treats the handle's thread as the survivor (not necessarily the calling thread: a thread invoking the verb on a sibling's handle within the same process ends every thread except that sibling, itself included; `relibc` only passes its own thread handle). It rejects callers whose PID does not match the thread's process (`EPERM`), rejects processes with more than `MAX_EXEC_SIBLINGS` (`1024`) threads before sending any kill (`E2BIG`), and returns `Ok(0)` immediately when only the surviving thread exists. Otherwise it marks sibling teardown active (refusing `new-thread` and `fork` with `EAGAIN`), writes `ContextVerb::ForceKill` to every other thread's status handle, and waits asynchronously in `PendingState::AwaitingSiblingsTermination` driven by kernel thread-death events until only the surviving thread remains. The wait has no timer and is cancelled by process exit, `SIGKILL`, or request cancellation ([ADR 0024](0024-tigerstyle.md)).
- **`relibc` lock order and fail-closed exec (`components/relibc/redox-rt/src/proc.rs`, `components/relibc/src/platform/redox/mod.rs`):** `Sys::execve` and `Sys::fexecve` register `ForkAllocHooks` alongside `Sys::fork`. In `fexec_impl` (`same_process == true`), after all fallible image, interpreter, stack, and auxiliary-vector setup succeeds and `tmp_disable_signals()` is active, `fexec_impl` acquires the five [ADR 0049](0049-fork-file-table-and-grants.md) locks in order (`DYNAMIC_PROC_INFO -> FILETABLE.inner -> FILETABLE.fd -> ALLOCATOR -> SIGACTIONS_LOCK`), sends `0x4645_0001` on `thread_fd` (retrying `EINTR` up to `EXEC_SIBLINGS_MAX_RETRIES = 16`, failing closed via `posix_exit(127)` on any error or exhausted retries), and releases all five locks once `procmgr` replies that every sibling has ended. The existing close-on-exec and handle-cleanup tail ([ADR 0050](0050-exec-runtime-handles.md)) then runs with no other thread able to hold those locks, and `deactivate_tcb(thread_fd)` also fails closed via `posix_exit(127)` so `exec` cannot return an error after siblings have already been ended.

## Consequences

- `relibc` and `base` ship together: because `relibc` fails closed on `EINVAL` when `procmgr` does not recognize `0x4645_0001`, installing a new `relibc` (`make p.relibc` alone) into an older image without the matching `base` breaks every `exec`.
- Statically linked programs that call `exec` directly from a multithreaded process pick up this rule only after being recooked against the updated `relibc`.
- Single-threaded `exec` takes one additional fast-path `procmgr` round trip on `thread_fd` (returning immediately when `threads.len() == 1`); `posix_spawn` does not take the `same_process` path.
- A sibling that never ends (tracked in `.scratch/kernel/issues/05-forcekill-lost-wakeup.md`) holds `exec` until the process is killed; there is no timer by design.
- A sibling killed mid-`fork` between `new_child_process` and `start` can leave a never-started child process holding a copy of the file table that `waitpid(-1)` can block on (tracked in `.scratch/kernel/issues/06-exec-during-fork-orphans-child.md`).
- A process with more than `1024` threads cannot `exec` (exits `127`).
- The allocator hook is installed by `relibc`'s `Sys::execve` and `Sys::fexecve`, so an `exec` through `redox-rt` without `relibc`'s platform layer (such as `bootstrap`) runs without the allocator lock, which is safe only because it is single-threaded.
- Any future FerrionOS-owned `procmgr` thread-handle verb takes the next value in `0x4645_xxxx` and is recorded alongside `0x4645_0001`.

## Where

- `components/base/bootstrap/src/procmgr.rs`
- `components/relibc/redox-rt/src/proc.rs`, `components/relibc/src/platform/redox/mod.rs`
- `recipes/tests/exec-handles-probe/exec-handles-probe.c`, `recipes/tests/exec-handles-probe/exec-handles-checker.c`, `scripts/vm-test.py` (`test_exec_handles`)
