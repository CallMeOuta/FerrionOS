# What the in-OS probes rely on, and how the test suites are packaged

Status: accepted

## Context

Each regression of a FerrionOS patch to the kernel, relibc or base has a probe under `recipes/tests/`, run inside the OS by `scripts/vm-test.py` ([`docs/testing.md`](../testing.md)). Several probes call system calls through relibc's raw entry points, and some must run as a particular user. The external suites (os-test, auto-test) are packaged as recipes too.

## Decision

### Raw calls

- relibc's entry points for the `call` and `fpath` system calls, and its libredox exports, return the negated error number on failure. `net-call-probe` passes `CallFlags::WRITE | CallFlags::READ` as plain bits, and `signal-handle-probe` uses the process manager's `PidfdCall::SendSignal` verb number, because the probes link no crate that defines them.
- `net-call-probe` builds a RecvMsg buffer that starts with the room for the name, then the payload, then ancillary data. It checks a plain `recv`, with no address and no flags, apart from `recv_from`, because it is a read of the socket; and it sends a datagram larger than the socket's whole send buffer (128 packets of 1,500 bytes). Its ports are above 1024, so `user` may bind them, and outside the stack's ephemeral range.

### Probe set-ups

- `scheme-short-read-probe` runs as root, because a `user` login may not open the scheme-creation capability. It serves a scheme of its own whose read at offset N answers with `min(N, len)` bytes of `FILL`, so the probe chooses the reply count through `pread`'s offset, and a forked child reads into buffers at offsets inside a page and across page boundaries: in the daemon's own address space the kernel maps nothing and the head and tail copies are never reached, and the scheme belongs to the process that made it. The buffer pattern is never zero and never `FILL`, so a changed byte is reported as zero, as the fill byte, or as foreign. It checks the kernel's `ferrion-scheme-short-read.patch`.
- `event-poll-probe` polls once before taking its baseline, so the thread's cached event queue exists by then.
- `lockdown-probe` reads physical address `0x1000`, inside the first megabyte of every PC; reads I/O port `0x80`, the POST diagnostic port, which works only once I/O privilege is raised; tries the `Iopl` verb on the caller's own thread handle, the route every driver used before item S13; and opens each hidden device scheme, where only "no such device", the namespace's answer, passes.
- `signal-handle-probe` runs in a child, because a shell makes each command a process group leader and a leader cannot start a session. It still writes a newline to `sudo`: `user`'s password is empty, so sudo does not read it, but images built before sudo stopped reading standard input need it.
- `dlopen-ctor` exports the host callback that the library's constructor resolves and calls through the real loader API. In `dlopen-tls`, the worker's write through a dynamic TLS pointer that starts non-null is the same refcount write a corrupted Rust `Arc` reaches.

### Suites

- `auto-test` runs the smaller suites first, so a regression shows sooner.
- `os-test-bins` builds the tests with `OS=` set to os-test's own name for this OS, and installs its optional packages in an exact order, sources before binaries, because the cookbook and the installer order packages by name while pkg orders by dependency, and os-test's Makefile needs the sources first.
- `os-test-result` has relibc as a dev-dependency, so it links the cookbook's relibc rather than the prefix's or the host's, and relibc can be rebuilt without updating the prefix.
- `os-test` installs its sources under `/home/user/os-test`: compiling them needs write access, so this package cannot choose between system and user paths.
- `virtio-fsd-selftest` uses base's source (`same_as`), because the selftest is a member of base's workspace (`base/selftests/virtio-fsd`).

## Consequences

A probe that linked a crate for these numbers would test that crate's view of the ABI, not relibc's. Running a root-only probe as `user`, or a `user` probe as root, makes it pass or fail for the wrong reason.

## Where

- `recipes/tests/net-call-probe/crate/src/main.rs`, `recipes/tests/signal-handle-probe/signal-handle-probe.c`, `recipes/tests/scheme-short-read-probe/`, `recipes/tests/event-poll-probe/crate/src/main.rs`, `recipes/tests/lockdown-probe/crate/src/main.rs`, `recipes/tests/dlopen-ctor/dlopen-ctor.c`, `recipes/tests/dlopen-tls/worker.c`
- `recipes/groups/auto-test/recipe.toml`, `recipes/tests/os-test-bins/recipe.toml`, `recipes/tests/os-test-result/recipe.toml`, `recipes/tests/os-test/recipe.toml`, `recipes/tests/virtio-fsd-selftest/recipe.toml`
