# RELRO and immediate binding in relibc and the image audit

Status: accepted

## Context

Rust's target specification requests full RELRO for FerrionOS executables, and C programs linked with `-Wl,-z,relro -Wl,-z,now` emit `PT_GNU_RELRO` together with `DT_BIND_NOW`, `DF_BIND_NOW` (`DT_FLAGS`), or `DF_1_NOW` (`DT_FLAGS_1`). Upstream relibc ignored all of them: `ld.so` re-protected only `PT_LOAD` segments after relocation, read only `LD_BIND_NOW` from the environment, skipped `self.is_me` (`libc.so`), and linked `libc.so` itself without `-Wl,-z,relro -Wl,-z,now`. In static start-up, `static_init` returned immediately after `PT_TLS` and never applied `PT_GNU_RELRO`. Without enforcement, overwriting a GOT or `.data.rel.ro` entry after start-up succeeded instead of faulting.

## Decision

- `src/ld_so/dso.rs` (`ferrion-relro-bind-now.patch`) parses `DT_BIND_NOW`, `DF_BIND_NOW` in `DT_FLAGS`, and `DF_1_NOW` in `DT_FLAGS_1` into `Dynamic::bind_now` and applies `Resolve::Now` per object in `DSO::relocate`. `lazy_relocate` returns early when `jmprel` or `pltrelsz` is zero and writes `got[1]` and `got[2]` only under `Resolve::Lazy`, avoiding writes into `.got` on objects where `-z now` folded `.got.plt` or omitted `DT_JMPREL`.
- After `PT_LOAD` protection, `DSO::relocate` protects every `PT_GNU_RELRO` range with `PROT_READ`, including for `self.is_me` (`libc.so` / `ld.so`) using its stored program headers. Both the start and end of the range are rounded down to page boundaries using checked arithmetic and a bounded program-header count (`MAX_RELRO_PHDRS = 256`), so trailing writable `.data` sharing a page with an unaligned RELRO end is never marked read-only.
- `static_init` in `src/ld_so/mod.rs` scans the auxiliary vector and program headers within explicit bounds (`MAX_STATIC_AUXV_ENTRIES = 1024`, `MAX_STATIC_PHDRS = 256`) without returning early on `PT_TLS`, derives `load_base` from `PT_PHDR` when present, initializes TLS, and calls the same `protect_relro_range` helper for `PT_GNU_RELRO`.
- relibc's `Makefile` links `libc.so` with `-Wl,-z,relro -Wl,-z,now`, so the dynamic linker and C library itself carries `PT_GNU_RELRO`, `DT_BIND_NOW`, and `DF_1_NOW` and enforces RELRO on its own mappings.
- `scripts/security-audit.py` adds `--root <dir>` and `--check files` using only the Python standard library. It walks a mounted image in sorted order without following symlinks (bounded by `MAX_IMAGE_ENTRIES`), flags setuid, setgid, and non-sticky world-writable entries, and parses ELF32/ELF64 headers for type, `PT_GNU_RELRO`, bind-now (`DT_BIND_NOW`, `DF_BIND_NOW`, `DF_1_NOW`), executable `PT_GNU_STACK`, and W+X `PT_LOAD` segments; relocatable `ET_REL` object files (`crt0.o`, `crti.o`, `crtn.o`) are reported as `info`.
- `recipes/tests/relro-got-readonly`, `config/relro.toml`, and `scripts/vm-test.py relro-got-readonly` prove the behavior on a booted image: a full-RELRO executable (`-Wl,-z,relro -Wl,-z,now`) binds before its first PLT call and faults with `SIGSEGV` on writes to its GOT, `.data.rel.ro`, and a companion shared object's RELRO; a partial-RELRO executable (`-Wl,-z,relro -Wl,-z,lazy`) keeps lazy PLT resolution and a writable `.got.plt` while faulting on `.data.rel.ro`; and a static executable (`-static -Wl,-z,relro`) faults on `.data.rel.ro` while keeping mutable `.data` writable.

## Consequences

- On the `x86_64` `minimal` image audited with `--check files`, 78 of 89 linked ELF files carry `PT_GNU_RELRO` and 48 of 57 dynamic ELF files carry bind-now (all 47 dynamic Rust PIE binaries plus `/usr/lib/libc.so`), with zero setuid, setgid, non-sticky world-writable, executable `PT_GNU_STACK`, or W+X `PT_LOAD` findings; overwriting a protected GOT or `.data.rel.ro` slot makes exploitation harder by faulting with `SIGSEGV`.
- Nine dynamic C and GCC binaries and shared libraries in `minimal` still fail the audit until compiler and linker hardening defaults land under S6: `/usr/bin/bunzip2`, `/usr/bin/bzcat`, `/usr/bin/bzip2`, `/usr/bin/bzip2recover`, `/usr/lib/libbz2.so.1.0.8`, `/usr/lib/libgcc_s.so.1`, `/usr/lib/liblzma.so.5.2.13`, `/usr/lib/libonig.so.5.5.0` (no `PT_GNU_RELRO`, no bind-now), and `/usr/lib/libstdc++.so.6.0.32` (`PT_GNU_RELRO` present, no bind-now), tracked in `.scratch/sec-s9/issues/01-minimal-c-elf-relro-bind-now.md`. Three freestanding kernel ELF images (`/usr/lib/boot/kernel`, `kernel.all`, `kernel.sym`) also report no `PT_GNU_RELRO`.
- No access is widened and no toggle is added: binaries without `PT_GNU_RELRO` or bind-now continue to load and run with their existing permissions and lazy binding.

## Where

- `recipes/core/relibc/recipe.toml`, `recipes/core/relibc/ferrion-relro-bind-now.patch`
- `scripts/security-audit.py` (`check_files`, `parse_elf`)
- `recipes/tests/relro-got-readonly/`, `config/relro.toml`, `scripts/vm-test.py` `test_relro_got_readonly`
