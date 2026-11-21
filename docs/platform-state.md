# Platform state as of 2026-11-20

FerrionOS builds its system components from upstream's sources ([`upstream.md`](upstream.md)), so what those components can and cannot do is what a FerrionOS image can and cannot do until a component changes here. This page keeps the facts that change how you work: beliefs that are out of date, limits that explain a failing port, test or boot, and places where the book in `book/` is out of date. Every fact is as of 2026-11-20 and carries its source: a file in this repository, a component's source at a commit, the book, or FerrionOS's own measurements in [`vm-baseline.md`](vm-baseline.md). Confirm a fact at its source before a decision rests on it.

Component sources cited here are the checkouts a fetch puts in `recipes/core/<name>/source/`: `base` at commit `20f7573b`, `kernel` at `e7f1d423`, `redoxfs` at `5ba45a68`.

## Out of date: what you may remember

| Remembered | State now | Source |
|---|---|---|
| "No dynamic linking" | A recipe that calls `DYNAMIC_INIT` links dynamically on every target except `riscv64gc-unknown-redox` and `wasm32-wasip1` | `DYNAMIC_INIT` in `src/cook/script.rs` |
| "Single-core by default" | `multi_core` is a default feature of the kernel | `Cargo.toml` of `kernel` at `e7f1d423` |
| "No Unix domain sockets" | `base` has a daemon for them, `udsd` | `udsd/` of `base` at `20f7573b` |
| "Build system is shell scripts" | The cookbook is Rust | `src/cook/` |
| "POSIX compliance is poor" | os-test scores the system these components make up 84% (3875/4594). Weak suites remain, see below. | [os-test](https://sortix.org/os-test/) |

## Limits that explain a failure

### Architectures

- x86_64 is the only broadly exercised target. Upstream's prebuilt package repository (`REMOTE_PKG_SOURCE` in `src/lib.rs`, read by `REPO_BINARY=1`) held 489 packages for x86_64, 115 for aarch64, 99 for i586 and 83 for riscv64gc. Prove a change on x86_64 first; a recipe that fails elsewhere has probably never built there.
- aarch64 reaches no graphical login in QEMU: the virtio drivers panic in `pcid`'s interrupt setup and `nvmed` panics or stalls, and only live mode reaches a text login ([`vm-baseline.md`](vm-baseline.md)). The book says "(ARM64 has limited support)" ([`book/src/aarch64.md`](../book/src/aarch64.md)).

### Hardware

- FerrionOS's compatibility record is [`vm-baseline.md`](vm-baseline.md), and it covers virtual machines only. Verify in QEMU.
- The drivers in `drivers/` of `base` at `20f7573b`: five network drivers (`e1000d`, `ixgbed`, `rtl8139d`, `rtl8168d`, `virtio-netd`), one USB host-controller driver (`xhcid`), and for display `ihdgd` (Intel), `vesad` (the firmware framebuffer) and `virtio-gpud`. There is no Wi-Fi, Bluetooth or I2C driver. Emulate devices from that list, as the `net=` choices in `mk/qemu.mk` do.
- Graphics are drawn by the CPU: `recipes/libs/mesa/recipe.toml` builds Mesa with the `swrast` Gallium and Vulkan drivers only, and a `virtio-vga-gl` guest draws in 2D only ([`vm-baseline.md`](vm-baseline.md)). The book says the same ([`book/src/graphics-windowing.md`](../book/src/graphics-windowing.md)).

### Networking

- IPv4 only: `netstack/Cargo.toml` of `base` at `20f7573b` builds smoltcp with `proto-ipv4` and no IPv6 feature. Turn IPv6 off in a port, as `recipes/net/curl` and `recipes/dev/git` do.
- os-test scores the udp suite 52% (70/134) ([os-test](https://sortix.org/os-test/)).

### Terminals, POSIX and relibc

- os-test's weak suites: pty 34% (10/29), fnmatch 18% (7/37) ([os-test](https://sortix.org/os-test/)).
- Tests hang in `waitpid` and `ppoll`; `recipes/tests/os-test-bins/recipe.toml` skips them.
- There is no Linux binary compatibility; programs are ported from source by design ([`book/src/developer-faq.md`](../book/src/developer-faq.md)).
- The kernel's system-call interface is unstable ([`book/src/how-redox-compares.md`](../book/src/how-redox-compares.md)), and Rust's targets are only expected to work with upstream's latest release because the ABI is not yet stable ([rustc book](https://doc.rust-lang.org/nightly/rustc/platform-support/redox.html)). The book plans a stable ABI for `libredox` only, keeps relibc unstable while it is under heavy development, and keeps `redox_syscall` unstable ([`book/src/libraries-apis.md`](../book/src/libraries-apis.md)). Write a port against relibc or `libredox`, not against raw system calls.
- Ported programs known to misbehave: Servo "Often Crashes", Neovim "Crashes", Zsh "Hangs", LuaJit "Often Crashing" ([`book/src/important-programs.md`](../book/src/important-programs.md)).

### Filesystem

- RedoxFS has no fsck or repair tool: `Cargo.toml` of `redoxfs` at `5ba45a68` builds five binaries, `redoxfs` (mount), `redoxfs-ar`, `redoxfs-clone`, `redoxfs-mkfs` and `redoxfs-resize`. A damaged image is rebuilt, not repaired.
- When the installer reports "No space left on device", raise `filesystem_size` in the config first.

### Debugging and virtual machines

- Debug from the host: `make qemu gdb=yes` with `make gdb`, or `scripts/backtrace.sh`.
- `make qemu` enables KVM when `ARCH` matches the host ([`mk/qemu.mk`](../mk/qemu.mk)).
- Boot failures FerrionOS has seen, with their panic sites, are in [`vm-baseline.md`](vm-baseline.md).

## Upstream's own checks are red

Of upstream's last 100 default-branch pipelines, relibc had 78 failed and 14 passed, kernel 51 failed and 46 passed, redoxfs 87 failed and 11 passed, and relibc's `test:x86_64` job failed in 22 of its 25 most recent failed pipelines (GitLab API query of 2026-11-20, `/pipelines?ref=<default>&per_page=100`). Upstream's branch head is therefore not a known-good state, and a recipe that follows it can break from one fetch to the next.

## Where the book is out of date

- [`book/src/developer-faq.md`](../book/src/developer-faq.md) sends driver authors to the README of the old `drivers` repository; drivers now live in `drivers/` of `base`.
- [`book/src/features.md`](../book/src/features.md) says NUMA "No (planned)"; `numa` is a default feature of the kernel at `e7f1d423`.
- [`book/src/real-hardware.md`](../book/src/real-hardware.md) says "USB devices are not yet supported during run-time" and, a paragraph later, "Only USB input devices (HID) work".
- [`book/src/porting-applications.md`](../book/src/porting-applications.md) gives `make ur.recipe-name` for a fresh build, and `mk/repo.mk` has no `ur.%` target (`ucr.%` unfetches, cleans and cooks).

## Security: unhardened

- "Exploit Mitigations: No (planned)" ([`book/src/features.md`](../book/src/features.md)).
- The sandbox tool `contain` is "a partial implementation of sandboxing" ([`book/src/developer-faq.md`](../book/src/developer-faq.md)).
- The images' login accounts are dev credentials ([`AGENTS.md`](../AGENTS.md), Security).

Describe FerrionOS's security as it is in any text you write. A test or design that leans on user separation or `contain` as a boundary rests on an open gap.
