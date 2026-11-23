# Upper-table runtime handles across exec and posix_spawn

Status: accepted

## Context

On FerrionOS, the kernel file table is split into a POSIX lower table and a runtime-private upper table (`UPPER_FDTBL_TAG`), where `relibc` keeps process, thread, namespace, working-directory, and file-table handles. When a program is started via `exec` or `posix_spawn` — whether from a single-threaded or multithreaded caller, or across a privilege or confinement boundary — the new program must hold only the runtime handles handed to it in its auxiliary vector (`AT_REDOX_THR_FD`, `AT_REDOX_PROC_FD`, `AT_REDOX_NS_FD`, `AT_REDOX_CWD_FD`, `AT_REDOX_FILETABLE_FD`), and enforcement must happen on the exec'ing side rather than in the new program's start-up code.

## Decision

- **Upper table is runtime-private across exec and spawn (`ferrion-exec-handles.patch`):** Neither `exec` nor `posix_spawn` preserves arbitrary upper-table descriptors. Only the explicit auxv runtime handles are retained for the new program.
- **Exec-side enforcement in `fexec_impl` (`same_process`):** Open a temporary `filetable-binary` handle before arming `current-addrspace` and before taking locks, then hold `FILETABLE.inner -> FILETABLE.fd` (preserving [ADR 0049](0049-fork-file-table-and-grants.md)'s lock order) across a bounded kernel file-table read (`FileTableVerb::Refresh`, `lseek(0, SEEK_SET)`, bounded by `CONTEXT_MAX_FILES`), batch `FileTableVerb::Close` (with per-descriptor `SYS_CLOSE` fallback accepting `EBADF` only), and userspace table removal. Keep only the auxv handles (`thr_fd`, `proc_fd`, `ns_fd`, `cwd_fd`, `filetable_fd`) plus the handles `fexec_impl` still drops before switching address spaces (`addrspace_selection_fd`, the temporary `filetable-binary` handle, and `current_filetable().fd`). Any read, bound, or non-`EBADF` close error fails closed (`posix_exit(127)` under `feature = "proc"`, `abort()` without `proc`).
- **Spawn-side enforcement in `Sys::spawn`:** Never read or seek the `filetable-binary` handle handed to the new program (`new_file_table`), because `filetable-binary` handles are positioned and sharing its read offset with the child would leave the child starting at EOF with an empty runtime table. Instead, open a separate temporary handle (`child.thr_fd.dup_into_upper(b"filetable-binary")`), read the child's kernel file table (bounded by `CONTEXT_MAX_FILES`), drop the temporary handle, and close every upper-table descriptor outside `{child.thr_fd, proc_fd, ns_fd}` alongside `O_CLOEXEC` descriptors before cloning `new_file_table` and `cwd_fd`, failing closed with an error return so `ChildCleanupGuard` terminates and reaps the child.

## Consequences

- An exec'd or spawned program holds only the runtime handles exec gives it, without trusting the new program's start-up code and without modifying the kernel's file-table copy path.
- A statically linked program carries this rule only once it is recooked against a `relibc` that has it, and any static program that starts others across a user or confinement boundary must be recooked whenever `relibc`'s exec or spawn path changes.
- Any future runtime handle intentionally passed across `exec` or `posix_spawn` must be added to `ExtraInfo`, handed through auxv, and listed in the keep-set of `fexec_impl` and `Sys::spawn`.
- Because `fexec_impl` and `Sys::spawn` read the kernel file table once before closing, each exec and spawn performs one additional bounded table read (`FileTableVerb::Refresh` + `lseek` + `read`) through its own temporary handle.
- Stopping sibling threads during a direct `execve` from a multithreaded process is tracked separately (`.scratch/kernel/issues/04-exec-sibling-threads.md`).

## Where

- `recipes/core/relibc/ferrion-exec-handles.patch`, `recipes/core/relibc/recipe.toml`
- `recipes/tests/exec-handles-probe/exec-handles-probe.c`, `recipes/tests/exec-handles-probe/exec-handles-checker.c`, `recipes/tests/exec-handles-probe/recipe.toml`
- `config/x86_64/desktop.toml`, `scripts/vm-test.py` (`test_exec_handles`)
