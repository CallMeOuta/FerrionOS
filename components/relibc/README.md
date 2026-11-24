# FerrionOS C Library (`relibc`)

The C standard library for FerrionOS (`components/relibc`), written in Rust. Derived from the upstream project at the revision recorded in [`components/upstream.toml`](../upstream.toml), under the license in [`LICENSE`](LICENSE).

`relibc` provides:

- C, POSIX, Linux, and BSD standard library functions and extensions
- The dynamic linker (`ld_so`)
- Low-level runtime and ioctl crates for system components (`redox-rt`, `redox-ioctl`, `generic-rt`)

It is designed primarily for FerrionOS as a memory-safer Rust alternative to C standard libraries written in C, and also supports Linux through the [sc](https://crates.io/crates/sc) crate.

## `redox-rt`

`redox-rt` is a runtime library that provides core POSIX runtime mechanics on FerrionOS, including `fork`, `exec`, and signal handling. `relibc` uses it as the backend in `src/platform/redox`.

## Repository layout

- `include` - C header files (primarily macros and variadic declarations that `cbindgen` does not generate)
- `src` - Library source files
- `src/c` - C helper files
- `src/crt0`, `src/crti`, `src/crtn` - C runtime startup and teardown objects
- `src/header` - Header implementations; each subdirectory has a `cbindgen.toml` generating C-to-Rust bindings and headers
- `src/ld_so` - Dynamic loader
- `src/platform` - Common and platform-specific backends (`src/platform/redox` for FerrionOS, `src/platform/linux` for Linux)
- `src/pthread` - POSIX threads implementation
- `src/sync` - Synchronization primitives
- `redox-rt`, `redox-ioctl`, `generic-rt`, `ld_so` - Workspace support crates
- `tests` - C test suite

## Building and checking in FerrionOS

Read [`agents/skills/component-sources.md`](../../agents/skills/component-sources.md) before modifying this tree.

Cook `relibc` from the repository root:

```sh
CI=1 COOKBOOK_LOGS=true make r.relibc
```

Cook dependents when shared interfaces or toolchain objects change:

- Run `CI=1 COOKBOOK_LOGS=true make c.relibc` when the cross-compilation toolchain copy must be updated.
- Run `CI=1 COOKBOOK_LOGS=true make r.relibc,base,drivers,initfs` after changes to `redox-ioctl`.
- Run `CI=1 COOKBOOK_LOGS=true make rp.relibc` to push into an existing image (with QEMU stopped), or `CI=1 COOKBOOK_LOGS=true make image` to rebuild the image.

Verify changes with the checks in [`scripts/vm-test.py`](../../scripts/vm-test.py) and [`docs/testing.md`](../../docs/testing.md):

- `python3 scripts/virtgpu-layout-check.py` after changing `redox-ioctl`
- `boot`, `shells`, `dlopen-ctor`, `dlopen-tls`, `dso-atexit`, `cond-clock`, `fd-primitives`, `fdtable-race`, `fork-filetable`, `exec-handles`, `file-locks`, and `shm-reuse`
- `canary-random`, `stack-smash-aborts`, `fortify-chk-aborts`, and `relro-got-readonly` for hardening invariants
- `config/auto-test.toml` to run the in-guest `relibc` C test suite

For contribution policy, see [`CONTRIBUTING.md`](../../CONTRIBUTING.md) and [`docs/code-policy.md`](../../docs/code-policy.md).

## Standalone build and tests

To build or run tests directly inside `components/relibc` outside the cookbook, install `cbindgen` (`cargo install cbindgen`) and `expect` from your host package manager, then run:

```sh
make all
```

Clean generated library objects and test binaries with `make clean`.

Use `./check.sh` to drive builds and the C test suite:

- `./check.sh` - Build without running tests
- `./check.sh --test` - Run all tests for `x86_64` using `redoxer`
- `./check.sh --test --host` - Run all tests on the Linux host
- `./check.sh --test --arch=aarch64` - Run all tests for a specific architecture (`x86_64`, `aarch64`, `riscv64gc`)
- `./check.sh --test=stdio/printf` - Run a single test (combinable with `--host` or `--arch`; statically linked on Linux, dynamically linked on FerrionOS)

Notes:

- Switching between target architectures or `--host` requires `make clean`.
- Running guest tests directly via `./check.sh` without `--host` requires `redoxer`.
- If a Linux host lacks target-prefixed binutils (such as `x86_64-linux-gnu-ar`) during host builds, override `CC`, `LD`, `AR`, and `NM` in `config.mk` for `x86_64-unknown-linux-gnu` to point at `gcc`, `ld`, `ar`, and `nm`.

## Supported operating systems and architectures

- Operating systems: FerrionOS and Linux
- FerrionOS architectures: `x86_64`, `aarch64`, and `riscv64gc`
