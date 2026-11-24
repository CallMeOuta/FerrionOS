# Developing `relibc` in FerrionOS

Contribution policy, commit rules, and testing requirements for this repository are defined in [`CONTRIBUTING.md`](../../CONTRIBUTING.md), [`docs/code-policy.md`](../../docs/code-policy.md), [`docs/testing.md`](../../docs/testing.md), and [`agents/skills/component-sources.md`](../../agents/skills/component-sources.md). Derived from the upstream project at the revision recorded in [`components/upstream.toml`](../upstream.toml), under the license in [`LICENSE`](LICENSE).

## Formatting

Format changes in `components/relibc` with `./fmt.sh` (using `rustfmt.toml` in this directory). New code FerrionOS adds carries no comments (`AGENTS.md`).

## Code style

While C ABI entry points require `unsafe`, push `unsafe` blocks and raw C pointer handling to the thin FFI boundary functions and keep internal implementations in safe Rust wherever possible.

### Use `Errno` and `.or_minus_one_errno()`

Internal functions should return `Result<T, Errno>` instead of writing `platform::ERRNO` and returning `-1` directly. Convert at the `extern "C"` boundary with `.or_minus_one_errno()`:

```rust
fn some_implementation(arg: c_int) -> Result<(), Errno> {
    if arg == 0 {
        Ok(())
    } else {
        Err(Errno(EOPNOTSUPP))
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn some_interface(arg: c_int) -> c_int {
    some_implementation(arg).or_minus_one_errno()
}
```

### Use safe wrappers for C strings and output buffers

Prefer `crate::c_str::{CStr, WStr}`, `NulTerminated`, `crate::out::Out`, and Rust slices over passing raw pointers through internal logic:

- `CStr` and `WStr` for NUL-terminated thin and wide strings
- `NulTerminated` for NUL-terminated arrays
- `Out<T>` for potentially uninitialized output buffers

### Keep opaque C types opaque

When POSIX does not specify the fields of a type (such as `FILE` or `pthread_t`), back it with an internal Rust struct and expose an opaque C-compatible wrapper matching its size and alignment rather than manipulating raw pointers everywhere.

### Use `platform::types` integer types

Use the C integer types from `crate::platform::types` (`c_int`, `c_long`, `c_char`, and so on) rather than Rust primitive integer widths so signatures remain portable across architectures.

## Building and verification

Follow [`components/relibc/README.md`](README.md) and [`agents/skills/component-sources.md`](../../agents/skills/component-sources.md) to cook with `CI=1 COOKBOOK_LOGS=true make r.relibc` and verify changes with the VM checks in [`scripts/vm-test.py`](../../scripts/vm-test.py) (and `python3 scripts/virtgpu-layout-check.py` after `redox-ioctl` changes). Per [ADR 0025](../../docs/adr/0025-tests-without-unit-tests.md), agents do not add unit tests; prove behavioral changes with VM, differential, or simulation checks.
