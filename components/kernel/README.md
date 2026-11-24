# Kernel

The FerrionOS microkernel (`components/kernel`). Derived from the upstream project at the revision recorded in [`components/upstream.toml`](../upstream.toml), under the license in [`LICENSE`](LICENSE).

## Building and checking in FerrionOS

Read [`agents/skills/component-sources.md`](../../agents/skills/component-sources.md) before modifying this tree.

Cook the kernel package from the repository root:

```sh
CI=1 COOKBOOK_LOGS=true make r.kernel
```

Push a rebuilt kernel into an existing image with `CI=1 COOKBOOK_LOGS=true make rp.kernel` (with QEMU stopped), or rebuild the full image with `CI=1 COOKBOOK_LOGS=true make image`.

Verify changes with the VM checks in [`scripts/vm-test.py`](../../scripts/vm-test.py) (see [`docs/testing.md`](../../docs/testing.md)):

- `boot`, `shells`, and `logout` for general startup and login
- `scheme-short-read`, `event-poll`, `fork-wake`, `ipc-fd`, `signal-handle`, and `jit-signal` for kernel IPC, signals, and futexes
- `x2apic-cpus` for x86_64 application-processor startup
- `iommu` and `dma-outside-grant` for IOMMU translation and DMA grants
- `hardened-malloc-sparse`, `hardened-malloc-pt-order`, and `hardened-malloc-discard` for sparse page-table operations and page discard

For contribution policy, see [`CONTRIBUTING.md`](../../CONTRIBUTING.md) and [`docs/code-policy.md`](../../docs/code-policy.md).

## Requirements

- [`nasm`](https://nasm.us/) must be available on `PATH` at build time when building outside the build container.

## Building documentation

```sh
cargo doc --open --target x86_64-unknown-none
```

## Debugging

### QEMU

Running [QEMU](https://www.qemu.org) with `-s` listens on TCP port `1234` for a GDB or LLDB client. From the repository root, start QEMU paused for debugging with:

```sh
make qemu gdb=yes
```

Use `make qemu gdb=nonblock` to open the GDB stub without pausing at startup.

### GDB

Load the kernel debug symbols from the cooked kernel stage (`recipes/core/kernel/target/<target>/stage/usr/lib/boot/kernel.sym`, or `build/<arch>/kernel.sym` for a standalone build) and attach with [GDB](https://www.gnu.org/software/gdb/):

```text
(gdb) symbol-file recipes/core/kernel/target/x86_64-unknown-redox/stage/usr/lib/boot/kernel.sym
(gdb) target remote localhost:1234
```

### LLDB

With [LLDB](https://lldb.llvm.org/):

```text
(lldb) target create -s recipes/core/kernel/target/x86_64-unknown-redox/stage/usr/lib/boot/kernel.sym recipes/core/kernel/target/x86_64-unknown-redox/stage/usr/lib/boot/kernel
(lldb) gdb-remote localhost:1234
```

After connecting, set breakpoints and run `continue`. Consult the debugger manual for additional commands.

## Notes

- Prefer `foo.get(n)` over `foo[n]` and handle `Option::None`. Panicking in kernel space halts the entire operating system.
- If a kernel panic leaves QEMU frozen, close the QEMU window, or kill that QEMU process by its own PID.
