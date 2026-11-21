# How ferrion-run starts a program in a narrower namespace

Status: accepted

## Context

`ferrion-run` starts a program with fewer schemes than its caller has and without the caller's open descriptors. It depends on relibc's runtime handles, which upstream marks ABI-unstable, and on libredox's namespace calls. `scripts/vm-test.py`'s `ferrion-run` check exercises it.

## Decision

### Policy, apart from the launch

`src/lib.rs` holds the policy and calls no OS function: option and profile parsing, the rule for standard descriptors and the descriptor bookkeeping. Its tests run on the build machine with `cargo test --lib`; the launch in `src/launch.rs` runs on FerrionOS only.

- Options stop at `--` or at the first argument that is not an option; everything after the program goes to it unchanged.
- A profile with an unknown key or version is an error, so a profile written for a later `ferrion-run` is refused instead of half applied.
- The `sudo` scheme is given back only when asked for; it is never the default.
- A standard descriptor may pass only when its path is a terminal, a pipe or a null device; a socket, a file on disk or any other scheme is refused. A closed standard descriptor passes nothing and is accepted.
- A file-table snapshot is a sequence of little-endian `u64` descriptor numbers.

### The launch

- `run` checks the standard descriptors, moves the process into a new namespace, closes every inherited descriptor except relibc's runtime handles, and executes the program in place; it returns only on failure. It starts no thread, so the file table cannot change while it works.
- relibc's handle on the process's file table is the one kernel `proc` handle besides the thread's whose size is not zero (only file-table handles carry a snapshot). When more than one handle looks like that, an inherited handle could be mistaken for it, and the launch is refused rather than guessed. exec drops that handle itself.
- The working directory's handle comes from the caller's namespace, so it is reopened through the new one, and the new handle is found by comparing the file table before and after.
- Inherited descriptors are closed through libredox's `close`, which goes through relibc, so relibc's runtime table forgets them too; otherwise exec would pass upper (tagged) descriptors on.

### Pins

relibc's process and thread handle getters are ABI-unstable, and `mkns`, `setns` and `getns` are relibc's namespace calls; the crate's libredox version is the one whose calls the relibc pinned in `recipes/core/relibc/recipe.toml` exports. Moving relibc's pin means checking these.

### The test helper

`ferrion-run-probe` exists for the `ferrion-run` check and is not for users. `hold-socket HOST:PORT [--stdin] -- PROGRAM` connects, keeps the socket open across exec as a POSIX descriptor and as an upper one, names both in `FERRION_RUN_PROBE_FDS` and executes the program (with `--stdin`, the socket is also standard input); `use-fd PATH` sends an HTTP request on each named descriptor and exits 0 when one answers; `connect HOST:PORT PATH` opens a new connection. It clears close-on-exec, which Rust sets by default, so that a socket ferrion-run failed to close would really pass.

## Consequences

- Starting a thread in `run`, or closing descriptors without going through relibc, lets descriptors reach the program.
- A relibc update can change the handles `runtime_file_table` relies on; the check in `scripts/vm-test.py` is the proof after such a move.

## Where

- `recipes/core/ferrion-run/crate/src/lib.rs`: `parse_args`, `parse_profile`, `child_schemes`, `check_stdio`, `parse_snapshot`, `new_handles`, `to_close`
- `recipes/core/ferrion-run/crate/src/launch.rs`: `run`, `prepare`, `runtime_file_table`
- `recipes/core/ferrion-run/crate/src/bin/ferrion-run-probe.rs`
- `recipes/core/ferrion-run/crate/Cargo.toml`: the `libredox` dependency
