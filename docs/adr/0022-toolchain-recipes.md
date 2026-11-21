# How the toolchain and language recipes cross-compile

Status: accepted

## Context

The compilers and language runtimes FerrionOS ships (GCC, binutils, LLVM, Rust, Python, Perl, PHP, Go, git, libtool) are cross-compiled on the build machine, often in two stages: a tool for the build machine first, then the program for FerrionOS. Their build systems read the same environment variables the cookbook sets for the target, and several of them assume they can run what they build.

## Decision

### llvm-config for the target

`bin/x86_64-unknown-redox-llvm-config` (and the aarch64 and i586 names, which link to it) wraps the build machine's `llvm-config`, because an `llvm-config` built for FerrionOS cannot run on the build machine. It filters out the architectures that do not match `$TARGET` and maps library names to the target sysroot, including the static libraries a platform without dynamic linking uses.

### GCC, binutils and the C++ runtime

- `gcc13` and `binutils-gdb` do not pass `--enable-shared` for the build-machine toolchain, which would link it against shared dependencies.
- The GCC bootstrap skips the target `libgcc` and `libstdc++`, which need relibc, not yet available at that stage; `gcc13` removes the shared runtime libraries from its stage so they do not conflict with the `libgcc` and `libstdcxx` packages.
- `libstdcxx` built with `host:` stages nothing, so it never replaces the build machine's `libstdc++`.
- `llvm-rt21` builds libc++abi with libgcc's unwinder, because libgcc already contains one.

### LLVM

- `llvm21` builds only the LLVM library and the tools Rust needs; LLVM's standalone build script (in llvm-zorg) shows how to build the rest.
- `llvm21` fetches every object up front, because the server times out on the blob request of a treeless checkout.
- `llvm18` limits its native tablegen build to 16 jobs, because it fails with more.

### Rust

- Link-time optimization is configured through Rust's own `config.toml`, not through the cookbook's flags (rust-lang/rust issue 147902).
- The build forces `libz-sys` to link zlib dynamically.
- The target toolchain variables are unset before `x.py`, so they do not poison the stage-1 compiler, which is built for the build machine.
- `SSL_CERT_FILE` is set to the distribution's CA bundle where one exists, because curl does not find it on some hosts.

### Other runtimes and tools

- `libtool` is built with `CFLAGS` unset, because libtool saves the absolute sysroot path, which holds nothing on the target.
- `libgmp` is built from a copy of its source, and its autotools are regenerated only for the target build: regenerating them for a `host:` build fails.
- `git` is built with `NO_IPV6` (not implemented), `NO_MMAP` (mmap writes extra null padding), `NO_SETITIMER` (deprecated) and `NO_UNIX_SOCKETS` (its socket use needs inotify).
- `php84`'s cleanup tolerates a missing `phar` link, which exists only with shells other than bash.
- `python312` replaces its second, identical `libpython3.12.a` with a symlink (about 60 MB) and rewrites the cross-compiled `sysconfigdata` paths so they work on the target; the wip `python315` does the same (about 73 MB).
- The wip `python39` builds the same Python version for the build machine first, as Python's cross-compilation requires, with `env -i` so the FerrionOS toolchain variables do not reach it, and removes that build by hand on a fresh target build, because the cookbook's clean does not.
- The wip `go` copies its source into the build directory, because Go does not build out of tree, and writes the `VERSION` file before `make.bash` so `-trimpath` works.
- The wip `perl5` builds in a copy of the source, because perl-cross's out-of-tree build is broken, and calls perl-cross's `./configure` directly, whose familiar-looking flags can mean something else.

## Consequences

Leaving the target variables set for a stage-1 build, or `--enable-shared` on a build-machine toolchain, produces a compiler that links or runs against the wrong libraries; the failure shows only later, in another recipe.

## Where

- `bin/x86_64-unknown-redox-llvm-config`
- `recipes/dev/gcc13`, `recipes/dev/binutils-gdb`, `recipes/libs/libstdcxx`, `recipes/dev/llvm-rt21`, `recipes/dev/llvm21`, `recipes/dev/llvm18`, `recipes/dev/rust`: `[build] script` of each
- `recipes/dev/libtool`, `recipes/libs/libgmp`, `recipes/dev/git`, `recipes/dev/php84`, `recipes/dev/python312`, `recipes/wip/dev/python315`, `recipes/wip/dev/lang/python39`, `recipes/wip/dev/lang/go`, `recipes/wip/dev/lang/perl5`
