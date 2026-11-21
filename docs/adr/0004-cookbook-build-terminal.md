# Build scripts run on a pseudo-terminal the cookbook owns

Status: accepted

## Context

The cookbook runs each recipe's build script on a pseudo-terminal, so that tools which colour or size their output for a terminal behave as they would interactively, and it copies that output to the terminal interface and to the recipe's log. The pty code in `src/cook/pty.rs` began as a copy of the portable-pty crate, which was not flexible enough for two output streams; it runs on Linux, macOS, FreeBSD and FerrionOS.

## Decision

- `setup_pty` returns three handles: the pty's output for the screen, the log's output, and one writer that feeds both. `setup_pty_with_stdin` returns the pty's input as a fourth. Callers keep the screen and the log apart.
- A new pty is 80 columns by 24 rows, the standard terminal size, until something resizes it.
- `openpty` passes the window size through mutable pointers, because BSD-derived systems declare those arguments mutable.
- `openpty` marks both descriptors close-on-exec only after wrapping them in `UnixMasterPty` and `UnixSlavePty`, so a failing `cloexec` still drops and closes them. Each of those types owns its descriptor and closes it on drop.
- In `PtyPair` the slave is declared before the master so that it is dropped first: Rust drops fields in declaration order (RFC 1857).
- A read that fails with `EIO` means the slave side has closed, and `read` reports it as end of file, so `read_to_string` and similar calls end normally.
- `spawn_command` resets the signal dispositions the child would inherit before it executes the program, and then discards the slave-side handles `Command` keeps in the `Child`: the cookbook talks to the child through the master side only. Those handles are empty today; moving them out keeps that true if `Command` changes.

## Consequences

- Reordering the fields or wrapping the descriptors later reintroduces descriptor leaks on error paths.
- A platform whose pty reports a closed slave other than with `EIO` would make a build look like it never ends.

## Where

- `src/cook/pty.rs`: `setup_pty`, `setup_pty_with_stdin`, `openpty`, `PtyPair`, `UnixMasterPty`, `UnixSlavePty`, `read`, `spawn_command`
