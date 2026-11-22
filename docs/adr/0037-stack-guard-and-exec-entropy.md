# Per-process random stack guard, exec entropy and fortify checks in relibc

Status: accepted

## Context

FerrionOS's relibc previously defined `__stack_chk_guard` as an immutable constant (`0xd048c37519fcadfe` on 64-bit, placed in `.rodata`), never pushed `AT_RANDOM` during exec, returned `0` from `getauxval`, and provided no `__*_chk` fortify functions or headers. Every process therefore shared one public canary, and compiling with `-fstack-protector-strong` failed to link under the prebuilt cross GCC 13.2.0 because its spec emits `-lssp_nonshared -lssp`. Security item S8 closes those runtime gaps in relibc before S6's toggle and measurements decide default compiler flags and before S10 adds ASLR in the same exec path.

## Decision

- `redox_rt::proc::exec_random_bytes(buf: &mut [u8]) -> Result<()>` is the single exec-path entropy helper, shared by S8 (`AT_RANDOM_BYTES = 16`) and later S10 ASLR. It opens `/scheme/rand` in the upper descriptor table with `O_CLOEXEC`, enforces named upper bounds (`EXEC_RANDOM_MAX_BYTES = 256`, `EXEC_RANDOM_MAX_READS = 64`), retries `EINTR` and short reads, and returns an error on EOF or I/O failure.
- `fexec_impl` calls `exec_random_bytes` once per executed image (on the final call that builds the initial stack), copies the 16 bytes to page-aligned offset 0 of the child's args/envs/auxv mapping, and pushes `AT_RANDOM` pointing to them.
- When `/scheme/rand` cannot be opened, `fexec_impl` fails closed and never falls back silently to a constant; `posix_spawn` propagates that error from its interpreter `fexec_impl` call instead of panicking on `.unwrap()`. One narrow early-boot exemption applies for root `init` (`ruid == 0`, `euid == 0`, and `pid == 1 || ppid == 1` on `ENODEV`, or freestanding builds without the `proc` feature) so `init` can start `00_ipcd`, `00_logd`, `00_nulld` and `00_randd` before `00_randd` registers `/scheme/rand`; in that window `AT_RANDOM` is omitted rather than fabricated. The long-term policy for a spawner whose namespace lacks `rand` awaits the maintainer's decision in `.scratch/sec-s8/issues/01-spawner-without-rand.md`.
- `__stack_chk_guard` is mutable (`static mut`) in `.data` and exported as `extern uintptr_t __stack_chk_guard;` in `<stdlib.h>`. Every start-up path copies the first word of `AT_RANDOM` into it before protected code runs:
  - In dynamic executables, `ld.so` `stage2` calls `DSO::init_self_stack_chk_guard` before `Linker::load_program`, writing directly to `self_base + st_value` from `libc.so`'s own dynamic symbol table instead of touching `libc.so`'s unrelocated `GLOB_DAT` slot (`__stack_chk_guard` is in `dynlst.txt`). Because `libc.so`'s storage already holds the random guard before relocation, non-PIE `R_X86_64_COPY` relocations, PIE and shared-library `GLOB_DAT` relocations, and DSO `.init_array` constructors all observe the same random guard.
  - In `relibc_start_v1`, static and dynamic executables record the auxv pointer and copy `AT_RANDOM` into `__stack_chk_guard` before TLS, platform init, `.preinit_array`, `.init_array` and `main`.
- `getauxval` scans the saved auxv pointer up to `AUXV_SCAN_LIMIT = 128`, sets `ENOENT` when a tag is absent, and `<sys/auxv.h>` includes `<elf.h>` so `AT_*` constants are visible.
- relibc's `Makefile` installs empty `libssp.a` and `libssp_nonshared.a` beside `libdl.a`, `libpthread.a` and `librt.a`. The prebuilt cross GCC 13.2.0 does not set `TARGET_LIBC_PROVIDES_SSP`, so linking with `-fstack-protector-strong` appends `-lssp_nonshared -lssp` even though `libc.a` and `libc.so` already export `__stack_chk_guard` and `__stack_chk_fail` (and x86_64 `-fPIC` calls `__stack_chk_fail@PLT`). Providing empty archives in relibc fixes linking for every recipe without rebuilding the cross toolchain or working around it in individual recipes.
- Fortify support lives in a second relibc patch (`ferrion-fortify-chk.patch`). `src/fortify.rs` exports `__chk_fail` (logging and calling `abort`) and 19 `__*_chk` entry points covering memory/string (`__memcpy_chk`, `__memmove_chk`, `__memset_chk`, `__strcpy_chk`, `__stpcpy_chk`, `__strncpy_chk`, `__stpncpy_chk`, `__strcat_chk`, `__strncat_chk`), stdio (`__vsnprintf_chk`, `__snprintf_chk`, `__vsprintf_chk`, `__sprintf_chk`, `__fgets_chk`) and unistd (`__read_chk`, `__pread_chk`, `__readlink_chk`, `__readlinkat_chk`, `__getcwd_chk`).
- `include/features.h` sets `__USE_FORTIFY_LEVEL` when `_FORTIFY_SOURCE > 0`, `__OPTIMIZE__ > 0` and `__NO_FORTIFY` is unset: level 1 uses `__builtin_object_size(..., 0)`, level 2 uses `__builtin_object_size(..., 1)` for subobject-aware string/stdio checks and mode 0 for raw memory/read checks, and level 3 uses `__builtin_dynamic_object_size` when the compiler supports it (GCC 12+ or `__has_builtin`). `include/bits/fortify-{string,stdio,unistd}.h` provide inline wrappers included from `<string.h>`, `<stdio.h>` and `<unistd.h>`.
- Proof follows [0025](0025-tests-without-unit-tests.md) and [0026](0026-measured-targets.md): `scripts/vm-test.py` checks `canary-random`, `stack-smash-aborts` and `fortify-chk-aborts` on `config/stack-guard.toml` across static, non-PIE dynamic (`R_X86_64_COPY`), PIE dynamic (`R_X86_64_GLOB_DAT`) and DSO `.init_array` probes.

## Consequences

- Every process spawned after `00_randd` starts receives a distinct 64-bit stack guard from `/scheme/rand`, verified on `x86_64` `config/stack-guard.toml` (`canary-random` and `stack-smash-aborts` red before, 3/3 green after).
- A post-boot process whose namespace omits `rand` cannot `exec` until the maintainer chooses among the options in `.scratch/sec-s8/issues/01-spawner-without-rand.md`; the four pre-`randd` boot services (`00_ipcd`, `00_logd`, `00_nulld`, `00_randd`) and `init` itself start without `AT_RANDOM` under the current exemption.
- Programs compiled with `-O2 -D_FORTIFY_SOURCE=2` abort on runtime-length overflows across the fortified string, stdio and unistd calls (`fortify-chk-aborts` red before, 3/3 green after); programs built without `_FORTIFY_SOURCE` are unchanged.
- Turning `-fstack-protector-strong` and `-D_FORTIFY_SOURCE=2` on as default cookbook build flags waits for S6's per-recipe toggle and a CPU-bound measurement.
- When `gcc13` is next rebuilt for the cross toolchain, defining `TARGET_LIBC_PROVIDES_SSP` in its target config removes the `-lssp` / `-lssp_nonshared` linker flags, though keeping the empty archives in relibc remains harmless.
- S9 (RELRO) can rely on `ld.so` `stage2` finishing its write to `__stack_chk_guard` before `Linker::load_program` runs relocations and `PT_GNU_RELRO` protection; `__stack_chk_guard` stays in `.data`.

## Where

- `recipes/core/relibc/recipe.toml`, `recipes/core/relibc/ferrion-stack-guard.patch`, `recipes/core/relibc/ferrion-fortify-chk.patch`
- `recipes/tests/stack-guard-probe/`, `recipes/tests/fortify-probe/`, `config/stack-guard.toml`
- `scripts/vm-test.py`: `test_canary_random`, `test_stack_smash_aborts`, `test_fortify_chk_aborts`
- `.scratch/sec-s8/issues/01-spawner-without-rand.md`
