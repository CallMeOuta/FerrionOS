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

- FerrionOS targets only `x86_64`, `aarch64` and `riscv64gc`, with no 32-bit userland or compatibility layer. x86_64 is the only broadly exercised target. Upstream's prebuilt package repository (`REMOTE_PKG_SOURCE` in `src/lib.rs`, read by `REPO_BINARY=1`) held 489 packages for x86_64, 115 for aarch64 and 83 for riscv64gc. Prove a change on x86_64 first; a recipe that fails elsewhere has probably never built there.
- aarch64 reaches no graphical login in QEMU: the virtio drivers panic in `pcid`'s interrupt setup and `nvmed` panics or stalls, and only live mode reaches a text login ([`vm-baseline.md`](vm-baseline.md)). The book says "(ARM64 has limited support)" ([`book/src/aarch64.md`](../book/src/aarch64.md)).

### Hardware

- FerrionOS's compatibility record is [`vm-baseline.md`](vm-baseline.md), and it covers virtual machines only. Verify in QEMU.
- The drivers in `drivers/` of `base` at `20f7573b`: five network drivers (`e1000d`, `ixgbed`, `rtl8139d`, `rtl8168d`, `virtio-netd`), one USB host-controller driver (`xhcid`), and for display `ihdgd` (Intel), `vesad` (the firmware framebuffer) and `virtio-gpud`. There is no Wi-Fi, Bluetooth or I2C driver. Emulate devices from that list, as the `net=` choices in `mk/qemu.mk` do.
- Graphics are drawn by the CPU: `recipes/libs/mesa/recipe.toml` builds Mesa with the `swrast` Gallium and Vulkan drivers only, and a `virtio-vga-gl` guest draws in 2D only ([`vm-baseline.md`](vm-baseline.md)). The book says the same ([`book/src/graphics-windowing.md`](../book/src/graphics-windowing.md)).

### Display

- The window system is Orbital (`recipes/gui/orbital`): the desktop images start it from `/usr/lib/init.d/20_orbital` and it draws in software. Programs reach it through `liborbital` (`recipes/libs/liborbital`), SDL's Orbital backend (`-DSDL_ORBITAL=1` in `recipes/libs/sdl2/recipe.toml`) or winit's Orbital backend, which the COSMIC apps and iced use.
- Wayland is experimental: `config/wayland.toml` starts `cosmic-comp` (or `smallvil`) as a compositor inside an Orbital window, with `libwayland` from `recipes/libs/libwayland`. Recipes for Wayland compositors and clients are in `recipes/wip/wayland/` and `recipes/wip/cosmic/`. `WAYLAND_DEBUG` prints nothing on FerrionOS (`recipes/libs/libwayland/redox.patch` turns it off), and Firefox's `MOZ_LOG=KeyboardHandler:5,Focus:5` traces keys and focus instead.
- Firefox (`recipes/wip/web/firefox`) is a Wayland-only build with its JavaScript JIT on, for x86_64 only. It renders headless (`scripts/vm-test.py firefox-headless`) and opens a window inside cosmic-comp's nested session (`config/x86_64/firefox-wayland.toml`, `scripts/vm-test.py firefox-window`). Earlier windowed launches hit an idle hang in 3 of 34 runs (symptoms only, no watcher dump) and 2 of 20 during diagnosis; both hangs caught with dumps were `nvmed`'s NVMe submission queue overflowing on >= 64-command bursts when `head == 0`, fixed by `ferrion-nvme-queue-full.patch`. A second, rarer stall found afterward in `nvmed`'s completion-entry read paired a new phase bit with an old command id, fixed by `ferrion-nvme-completion-read.patch` (both patches in `recipes/core/base`). Neither NVMe bug has stalled since (`scripts/vm-test.py nvme-queue` 0 stalls in 20 boots); `ferrion-ring-wake-nonblock.patch` in `recipes/core/base` also ensures the block-driver queue worker never blocks the driver's executor on its wake pipe (`scripts/vm-test.py ring-wake` passed 5 of 6 boots with 12,100 rounds in the 5 passing boots and 1 stall on a separate open issue that can still make the check fail; `nvme-queue` 5 of 5; `firefox-window` 0 disk stalls in 10 launches: 9 passed and 1 ended the HTTPS wait at 9.5 s while the page was still loading). Earlier address-bar typing failures (2 of 20 `firefox-window` boots) came from the harness typing after a blind 1 s sleep before `Ctrl+L` finished focusing the address bar across Firefox's content-process round-trip, fixed in `scripts/vm-test.py` by waiting with a bound for the address bar's selection highlight before typing (0 lost keystrokes in 28 navigations across 15 boots). With Firefox open, the guest has about 3.2 GB more memory in use.
- There is no X11: no X server, no Xwayland, no X11 client library or protocol headers. `config/x11.toml`, `recipes/wip/x11/` and the X11-only recipes were removed on 2026-11-20, ports build with X11 disabled, and `scripts/no-x11-check.py` fails when a recipe depends on an X11 library or a config installs one (`AGENTS.md`, Display). A program that can only draw through X11 has no path here.
- Upstream's prebuilt packages of cairo, firefox, gstreamer, gtk3, libxkbcommon, mesa-demos, qemu, webkitgtk3 and zenity depend on X11 libraries (their `depends` in upstream's package repository, read 2026-11-20). The configs that install them set them to `"source"`, so `REPO_BINARY=1` builds them from these recipes, with X11 disabled, instead of fetching upstream's builds.

### Networking

- IPv4 only: `netstack/Cargo.toml` of `base` at `20f7573b` builds smoltcp with `proto-ipv4` and no IPv6 feature. Turn IPv6 off in a port, as `recipes/net/curl` and `recipes/dev/git` do.
- NW1 transport, wakeups, socket options, DNS, and interface enumeration ([ADR 0048](adr/0048-nw1-network-stack.md)): `netstack` answers ICMPv4 echo requests on `eth0` (`auto-icmp-echo-reply`), defaults TCP socket buffers to `1 MiB` RX / `256 KiB` TX (`SO_RCVBUF`/`SO_SNDBUF` clamped to `16 KiB`–`4 MiB`, post-connect sets return `0` while `getsockopt` reports actual capacity), closes TCP sockets with `FIN` and a bounded 64-entry lingering table (`15 s`), returns `ENETUNREACH` on unconfigured TCP connects, enforces a `30 s` TCP connect timeout, supports `SOL_SOCKET` (`SO_REUSEADDR`, `SO_KEEPALIVE`, `SO_RCVBUF`, `SO_SNDBUF`, `SO_RCVTIMEO`, `SO_SNDTIMEO`, clear-on-read `SO_ERROR`) and `IPPROTO_TCP` (`TCP_NODELAY`, `TCP_KEEPIDLE`), sizes `listen(backlog)` pools to `1..=64` within an `8 MiB` per-listener buffer-capacity budget (`6` slots at default `1 MiB` RX / `256 KiB` TX; unsupported levels/options return `ENOPROTOOPT`), and waits for the three-way handshake to complete before returning a socket from `accept()`. `virtio-netd` handles RX interrupts directly on its event loop with a 64-slot `4 KiB` TX DMA pool. `relibc` retries dropped UDP DNS queries once with a fresh transaction ID and source port within its 7-second budget, falls back to TCP port 53 when a matched reply sets `TC`, and implements IPv4-only `getifaddrs` plus `if_nameindex`/`if_nametoindex`/`if_indextoname` (`lo` index `1`, `eth0` index `2`; until NW2's `netd` status scheme lands, callers without `/scheme/netcfg` such as `user` see `lo` only, `ferrion-run --no-network` sees `lo` without `IFF_UP`, and `linux-run`'s netlink interface list stays independent).
- os-test scores the udp suite 52% (70/134) ([os-test](https://sortix.org/os-test/)).

### Terminals, POSIX and relibc

- os-test's weak suites: pty 34% (10/29), fnmatch 18% (7/37) ([os-test](https://sortix.org/os-test/)).
- Tests hang in `waitpid` and `ppoll`; `recipes/tests/os-test-bins/recipe.toml` skips them.
- There is no Linux binary compatibility; programs are ported from source by design ([`book/src/developer-faq.md`](../book/src/developer-faq.md)).
- The kernel's system-call interface is unstable ([`book/src/how-redox-compares.md`](../book/src/how-redox-compares.md)), and Rust's targets are only expected to work with upstream's latest release because the ABI is not yet stable ([rustc book](https://doc.rust-lang.org/nightly/rustc/platform-support/redox.html)). The book plans a stable ABI for `libredox` only, keeps relibc unstable while it is under heavy development, and keeps `redox_syscall` unstable ([`book/src/libraries-apis.md`](../book/src/libraries-apis.md)). Write a port against relibc or `libredox`, not against raw system calls.
- Ported programs known to misbehave: Servo "Often Crashes", Neovim "Crashes", Zsh "Hangs", LuaJit "Often Crashing" ([`book/src/important-programs.md`](../book/src/important-programs.md)).
- relibc `74b5e7e3` answers `fcntl(fd, F_DUPFD, n)` with the lowest free descriptor and ignores `n` (`redox-rt/src/sys.rs`, `fcntl`), where POSIX asks for the lowest at or above `n`. Move a descriptor to a chosen number with `dup2`, or pass its number to the program that inherits it.
- A descriptor received over a pipe with `call_ro(pipe, ..., CallFlags::FD, ...)` was unusable in `ferrion-scoped` (`dup` answered `EBADF`, seen 2026-11-21); received with `CallFlags::FD | CallFlags::FD_UPPER`, as `init` takes a scheme daemon's capability (`base` `init/src/service.rs`), it works.
- A scheme serves requests in the address space of the process that created its socket: the kernel maps each request's buffers into that process (`kernel` `e7f1d423`, `src/scheme/user.rs`, `UserInner::context`). A socket created by one process and served by another faults on the first path it reads, so the serving process creates its own socket and hands out a descriptor for it.
- Native eventfd and timerfd descriptors are provided by `fdobjd` and the relibc patch `ferrion-fd-primitives.patch`. The x86_64 guest check `scripts/vm-test.py fd-primitives --config fd-primitives` passed on 2026-11-21; removing the daemon made the probe fail at eventfd creation. It covers counter/semaphore I/O, timer deadlines and intervals, poll/epoll wakeups and dup/fork/exec sharing. Timer cancellation-on-clock-change is refused with `EINVAL` until a clock-change notification exists; signalfd remains unimplemented. The native event queue reports changes, not Linux level-triggered snapshots.

### Filesystem

- RedoxFS has no fsck or repair tool: `Cargo.toml` of `redoxfs` at `5ba45a68` builds five binaries, `redoxfs` (mount), `redoxfs-ar`, `redoxfs-clone`, `redoxfs-mkfs` and `redoxfs-resize`. A damaged image is rebuilt, not repaired.
- When the installer reports "No space left on device", raise `filesystem_size` in the config first.
- Advisory file locks are real on RedoxFS through the relibc and RedoxFS patches `ferrion-file-locks.patch`: fcntl `F_SETLK`, `F_SETLKW` (blocking, with deadlock detection) and `F_GETLK`, open-file-description locks and `flock`, released when the owner closes the file or exits. A conflict answers `EAGAIN`, and `flock` answers `EWOULDBLOCK`. `scripts/vm-test.py file-locks` runs `lock-probe` (25 checks) and two SQLite writers in WAL and rollback-journal mode; it passed on 2026-11-21.
- Inside a `ferrion-run` session, locks on files in `--files` and `--clone` folders are answered by the session's proxy: a conflicting lock fails at once instead of waiting, and a lock does not exclude programs outside the session (`.scratch/agent-scope/issues/01-waiting-locks.md`).
- Shared file mappings on RedoxFS agree with `read` and `write` through the RedoxFS patch `ferrion-map-coherence.patch`: a new mapping shows ordinary writes, `read` returns bytes written through a live shared mapping, unmapping leaves the file's size alone, and a file's pages are freed when its last handle and mapping are gone. `scripts/vm-test.py shm-reuse` and `shm-read-back` passed on 2026-11-21.

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
- `ferrion-run` sessions are scoped, not contained: their file system holds only the folders they are given, and their limits are listed in [`book/src/security.md`](../book/src/security.md#scoped-sessions).
- The images' login accounts are dev credentials ([`AGENTS.md`](../AGENTS.md), Security).

Describe FerrionOS's security as it is in any text you write. A test or design that leans on user separation or `contain` as a boundary rests on an open gap.
