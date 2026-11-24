# Base

Essential userspace system daemons and libraries for FerrionOS (`components/base`). Derived from the upstream project at the revision recorded in [`components/upstream.toml`](../upstream.toml), under the license in [`LICENSE`](LICENSE).

Hardware drivers live in `ferrion-hardware` (pinned by `recipes/core/drivers/recipe.toml`, see [`docs/adr/0047-drivers-in-ferrion-hardware.md`](../../docs/adr/0047-drivers-in-ferrion-hardware.md)).

## Components

- `audiod` - Sound daemon (FerrionOS images run `ferrion-audiod` instead, configured in `config/base.toml`)
- `bootstrap` - First userspace binary executed by the kernel, responsible for launching `init` and core namespace services
- `config` - Configuration override library
- `daemon` - FerrionOS daemon library
- `dhcpd` - DHCP client daemon
- `getty` - TTY initialization and login launcher
- `graphics-ipc` - Shared graphics IPC definitions
- `init` - System service launcher
- `initfs` - Bootstrap filesystem library and archive tools used before mounting the root filesystem
- `inputd` - Input multiplexer daemon
- `ipcd` - Inter-process communication daemon
- `logd` - System logging daemon
- `netstack` - Userspace networking stack daemon
- `ptyd` - Pseudo-terminal daemon
- `ramfs` - In-memory filesystem daemon
- `randd` - Random number generator daemon
- `scheme-utils` - Shared helpers for scheme daemons
- `selftests` - Guest selftests (including `virtio-fsd`)
- `udsd` - Unix domain socket daemon
- `zerod` - Zero and null device daemon

## Building and checking in FerrionOS

Read [`agents/skills/component-sources.md`](../../agents/skills/component-sources.md) before modifying this tree.

Cook `base` from the repository root:

```sh
CI=1 COOKBOOK_LOGS=true make r.base
```

When boot filesystem contents change, also cook `initfs` (`CI=1 COOKBOOK_LOGS=true make r.base,initfs`). Push into an existing image with `make rp.base,initfs` (with QEMU stopped), or rebuild the image with `make image`.

Verify changes with the VM checks in [`scripts/vm-test.py`](../../scripts/vm-test.py) (see [`docs/testing.md`](../../docs/testing.md)):

- `boot`, `shells`, and `logout`
- `nsmgr-bounds`, `scheme-create`, `driver-namespaces`, `login-namespace`, and `lockdown`
- `rand-seed`, `resolver`, `net-basics`, `net-hardening`, and `net-calls`

For contribution policy, see [`CONTRIBUTING.md`](../../CONTRIBUTING.md) and [`docs/code-policy.md`](../../docs/code-policy.md).

## Standalone Makefile targets

Outside the cookbook, `Makefile` and `./check.sh` provide direct build and selftest targets using `redoxer`:

- `./check.sh` - Run checks for `TARGET` (defaults to `x86_64-unknown-redox`)
- `./check.sh --test` - Run `cargo test` instead of `cargo check`
- `./check.sh --clippy` - Run `cargo clippy` instead of `cargo check`
- `./check.sh --all-target` - Run the command across all supported target architectures
- `./check.sh --target=<target>` - Override the target triple (for example, `x86_64-unknown-redox`)
- `./check.sh --arch=<arch>` - Override the target architecture (`x86_64`, `aarch64`, `riscv64gc`)
- `./check.sh --help` - Print `check.sh` usage
- `make test-selftests` - Run all selftests under `selftests/`
- `make test-virtio-fs` - Run the `virtio-fsd` selftest
- `make install` - Stage output into `./sysroot`
