# How the Linux layer's probes and fixtures are built and packaged

Status: accepted

## Context

The Linux compatibility layer (`plans/linux-compat.md`) runs static Linux programs under `linux-run`, through a kernel hook for foreign system calls. No FerrionOS recipe can build Linux programs yet, and the cookbook strips executables it finds in a package's usual places.

## Decision

### Packaging

- Linux programs are packaged under `usr/share/` (`usr/share/linux-hello`, `usr/share/linux-busybox`, the foreign-call probe's Linux part), where the cookbook does not strip, because they are Linux files.
- `linux-hello-fixtures` holds the four static hello programs of LC0 (raw system calls, musl, glibc and Go, each printing `hello from linux`), built on the build host from the sources in `files/` with the pinned Debian tools in `files/toolchain.tsv` (`files/build-hellos.sh`), and kept as binaries.
- `linux-busybox-fixtures` holds Debian's static busybox from LC0's pinned root, the LC2 script and the output Linux gave for it; `files/SOURCE` says where each file comes from.
- Both fixture recipes refuse any file whose SHA-256 differs from `files/SHA256SUMS`.

### The LC2 script

`files/lc2.sh` exercises `busybox sh`: a loop, pipelines, a subshell, a here-document, file commands, a trap and background jobs. Its output is the same on every run and machine: it prints no dates, directory sizes, user names or host names. `EXPECT_UID` is the caller's user id, so `id -u` is checked without being printed.

### The foreign-call probe

- `foreign-call-probe` (native) checks the kernel hook's boundaries and its cost; `linux-probe` (a static Linux program without a C library, run under `linux-run`) checks register, FS and XMM crossings, Linux signal frames, `clone` and call costs. `config/x86_64/desktop.toml` installs them and `scripts/vm-test.py foreign-call` runs both.
- relibc's libredox exports return a negated errno for errors: a result above -4096 is an error.
- The entry stub stores the registers as it found them into `seen`, in a fixed order: rax rbx rcx rdx rsi rdi rbp rsp r8 to r15, rflags, xmm0 (low, high), fsbase, gsbase, then the count of entries.
- A forked child is a new context, so it registers the hook again before the probe checks that exec clears it.
- In a Linux x86-64 `ucontext_t`, `uc_mcontext` starts at offset 40 and the saved `rip` is its 17th word.

## Consequences

Packaging a Linux program under `usr/bin` gets it stripped as a FerrionOS executable. A fixture changed without its `SHA256SUMS` entry is refused at build time.

## Where

- `recipes/tests/linux-hello-fixtures/recipe.toml`, `recipes/tests/linux-busybox-fixtures/recipe.toml`, `recipes/tests/linux-busybox-fixtures/files/lc2.sh`
- `recipes/tests/foreign-call-probe/recipe.toml`, `foreign-call-probe.c` (`seen`), `linux-probe.c`
