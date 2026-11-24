# VM baseline

What upstream's daily images do in a virtual machine, measured 2026-11-20 as the baseline for FerrionOS's virtual-machine work. Upstream images are built from the same component sources this repository fetches, so they stand in for a FerrionOS image until one can be built here. Rerun these checks on every new FerrionOS image and keep the old columns for comparison.

## Setup

- Images: `redox_desktop_x86_64_2026-11-19_589_harddrive.img.zst` and `redox_desktop_aarch64_2026-11-18_589_harddrive.img.zst` from [static.redox-os.org/img](https://static.redox-os.org/img/), each checked against the `SHA256SUM` beside it, booted with `snapshot=on` so the image stays unchanged.
- Host: x86_64 Linux with KVM, QEMU 11.1.3, UEFI firmware from the distribution's edk2 packages.
- x86_64 guest: `-machine q35 -smp 4 -m 2048 -enable-kvm -cpu host`, `qemu-xhci` with USB keyboard and tablet, Intel HDA, user-mode networking, the serial port written to a file. The standard devices are those of `make qemu` on x86_64: VGA, NVMe, e1000 ([`mk/qemu.mk`](../mk/qemu.mk)).
- aarch64 guest: `-machine virt -cpu max -smp 4 -m 2048` under emulation (no KVM on an x86_64 host), the same USB, network and serial setup.
- Network: a QEMU `filter-dump` capture of the guest's traffic, read for a DHCP exchange.
- The bootloader waits at its resolution menu until Enter is pressed (finding 1), so each run sends Enter through the QEMU monitor.

## x86_64

|Devices|Reaches login|Evidence|
|---|---|---|
|Standard: VGA, NVMe, e1000|Yes|Graphical login screen 18 s after Enter; DHCP lease `10.0.2.15`|
|virtio-blk disk|Yes|`virtio-blkd` finds the disk and the root filesystem mounts|
|virtio-net|Yes|`virtio-netd` starts; DHCP lease `10.0.2.15`|
|virtio-vga|Yes|`virtio-gpud` takes over from the boot framebuffer at 1280x800|
|virtio-gpu-pci, no VGA|Yes|No boot framebuffer, so the bootloader screen is blank; `virtio-gpud` brings up the display|
|virtio-vga-gl, host rendering through EGL|Yes|Login prompt on the serial console; the guest draws in 2D only, since Mesa is built for the CPU (`swrast`)|
|FerrionOS image: VGA, NVMe, virtio-net|Yes|Built here on 2026-11-20 with `make all REPO_BINARY=1` at commit `8395b5e7`; same graphical login as upstream's image within 18 s of Enter; `virtio-netd` starts; `/usr/lib/os-release` reads `NAME="FerrionOS"`, `ID="ferrionos"`, `ID_LIKE="redox-os"`|

## aarch64

|Devices|Reaches login|Evidence|
|---|---|---|
|NVMe, e1000, ramfb (GICv2)|No|`nvmed` panics at `drivers/storage/nvmed/src/main.rs:144:50`, the root filesystem does not mount, no DHCP traffic|
|NVMe, e1000, virtio-gpu-pci (GICv2)|No|The same `nvmed` panic, and `virtio-gpud` panics at `drivers/pcid/src/driver_interface/irq_helpers.rs:313:9`|
|virtio-blk, virtio-net, ramfb (GICv2)|No|`virtio-blkd` panics at `irq_helpers.rs:313:9`; the root filesystem does not mount|
|virtio-blk, virtio-net, virtio-gpu-pci (GICv3)|No|`virtio-blkd` and `virtio-gpud` both panic at `irq_helpers.rs:313:9`|
|NVMe, e1000, virtio-gpu-pci (GICv3)|No|No panic: `nvmed` identifies the controller and the boot stops there|
|Live mode, NVMe attached, e1000, ramfb (GICv2)|Text login only|The bootloader copies the 647 MiB image into memory; `nvmed` still panics but nothing needs it; `e1000d` gets DHCP lease `10.0.2.15`; the console reaches `redox login:` but no graphical desktop appears, and a process aborts with "RELIBC PANIC: panicked at src/cxa.rs:84:11: RefCell already borrowed"|
|Live mode, NVMe attached, virtio-net, ramfb (GICv3)|No|`nvmed` identifies the controller and the boot stops there, as without live mode|

On aarch64 no PCI disk driver brought up the root filesystem: the virtio drivers fail in the shared interrupt setup of `pcid`, with either interrupt controller, and `nvmed` panics or stalls. `make qemu` sidesteps this by booting the live disk, which the bootloader loads whole into memory, with a `ramfb` framebuffer ([`mk/qemu.mk:55-62`](../mk/qemu.mk#L55-L62)); that path reaches a text login with working e1000 networking, not a desktop. The `irq_helpers.rs:313` panic is the first ARM item: it blocks the virtio disk, network and display drivers at once.

## Findings

1. Upstream's bootloader waits at its resolution menu with no timeout, so an unattended VM never boots. Fixed for FerrionOS on 2026-11-20 by `recipes/core/bootloader/ferrion-quiet-boot.patch`: on UEFI the bootloader shows nothing and boots the best mode unless a key arrives in the first second, and the image built with it reaches the login screen with no key pressed. It replaced a first fix that booted the highlighted mode after 3 seconds.
2. Upstream's bootloader panics when the serial port is a socket instead of a file: "Failed to set max mode" and then `src/os/uefi/mod.rs:362:87` unwrapping a "device error" (bootloader 1.0.0). The same patch ignores that error, since highlighting is cosmetic.
3. `make qemu` looked for UEFI firmware in `/usr/share/ovmf`, `/usr/share/OVMF`, `/usr/share/AAVMF` and QEMU's own share folder, not in `/usr/share/edk2/x64/` and `/usr/share/edk2/aarch64/`, where Arch-based distributions install it, so an x86_64 guest fell back to legacy BIOS boot without a message. `mk/qemu.mk` now looks there too.
4. Upstream branding showed in the running system: the login background is upstream's logo from the `orbdata` package, the bootloader printed upstream's name, the kernel logged "<name> starting...", the console greeted with an upstream banner from `/etc/issue`, login printed a fixed `redox login:` prompt and an upstream `/etc/motd`, `/etc/hosts` and three launcher entries named upstream. Since 2026-11-20 the bootloader patch renames its banners, `recipes/core/kernel/ferrion-boot-name.patch` logs "FerrionOS starting..." (`uname -s` still answers Redox), `recipes/core/userutils/ferrion-login-name.patch` prompts with `/etc/hostname` (`ferrion login:`) and ships a FerrionOS motd and issue, and the configs write `/etc/hostname`, `/etc/hosts`, `/etc/issue` and the launcher entries. The FerrionOS mark replaced upstream's logo the same day (`branding/README.md`): `recipes/gui/orbdata` lays the login background, the launcher's start button and a plain wallpaper over the package, and the login box takes FerrionOS's colors from `recipes/gui/orbutils/ferrion-neutral-login.patch`. A scan of the desktop image's `/etc`, `/home` and `/usr/share` finds the name only in identifiers, fetch URLs and licence notices. What remains: help and author texts inside prebuilt programs (`ion`, `cur`, `info`, `less`, `man`, `mdless`, `mtxt`, `rem`, `ifconfig`, the installers), each a patch to its own component.
5. The login screen has `user` filled in and accepts an empty password: the dev accounts, since 2026-11-20 in `config/dev-accounts.toml` (they were in `config/base.toml`).
6. A QEMU without its optional display modules has no virtio-gpu devices and no window: on split-package distributions `make qemu gpu=virtio-gl` needs the `virtio-vga-gl`, GTK and OpenGL modules installed.
7. Quiet boot, 2026-11-20: with the bootloader, kernel (`recipes/core/kernel/ferrion-quiet-boot.patch`) and base (`recipes/core/base/ferrion-boot-splash.patch`) patches, screenshots every 0.25 s from power-on show a black screen, a progress bar that moves on real boot events, then the graphical login at about 3 s, with no text. A key in the first second brings back the bootloader menu and the text log; a key during the splash shows the log. The kernel's messages stay on the serial port. Since the FerrionOS mark went in, the splash draws the mark where the login screen shows it and the bar where the login screen sets the name, on FerrionOS's ground `#0f0e0c`, which the bootloader, the splash and Orbital all fill; screenshots every 0.25 s show the firmware's black screen with its TianoCore logo at about 0.6 s, black through the bootloader's one-second wait for a key, the splash from about 1.9 s and the login at about 2.4 s. The black before the splash is the firmware's screen, which the bootloader only fills once it has chosen a mode.
8. In 1 of about 13 boots the text console (VT 2, with the `/etc/issue` banner and `redox login:`) took the screen instead of the graphical login. Cause, found 2026-11-20: two VT switches raced. `inputd` numbers VTs in the order their consumers open (1 for the boot log, then 2, 3, ...); init spawned orbital before `fbcond@2`, so either could take VT 2, while orbital always ran `inputd -A 3`. The desktop also ran `inputd -A 2` from `30_console_switch`: `config/desktop.toml` included `desktop-minimal.toml` before `server.toml`, and `server.toml` brought `minimal.toml`'s copy of that script back after desktop-minimal's empty override. Usually `fbcond` took VT 2 and orbital's switch to 3 came last; when orbital's switch landed first, init's `-A 2` left the text console on screen. A test config that held init for 3 s before `30_console_switch` showed the text console in 6 of 6 boots. Fixed in the configs, no component patch: `config/desktop.toml` now includes `server.toml` first, so the desktop's `30_console_switch` is empty, and every orbital init script (`desktop-minimal`, `desktop-contain`, `wayland`, `redoxer-gui`) requires `30_console.target`, so `fbcond@2` holds VT 2 before orbital starts and orbital's VT is always 3. With the same 3 s hold plus a 2 s hold between orbital and `fbcond`, 6 of 6 boots reached the graphical login; the desktop image (`make image REPO_BINARY=1`) reached it in 30 of 30 quiet boots, each screenshot 8 s after power-on averaging (43, 95, 135), the login screen's color.
9. Disk encryption, 2026-11-21: on an image with `encrypt_disk`, the passphrase typed at the bootloader does not stay in guest memory once the disk is unlocked (`recipes/core/bootloader/ferrion-passphrase-wipe.patch`, `recipes/core/redoxfs/ferrion-passphrase-wipe.patch`): a 2 GiB guest memory dump taken at the login holds no copy of it, a wrong passphrase is refused, and the right one reaches the login in about 9 s. Verified boot and disk encryption do not combine yet: signing mounts the image through FUSE, which would ask an encrypted image for its passphrase `[unverified]`.

## Rust GUI apps (R0), 2026-11-20

- `recipes/wip/demos/wgpu-hello` is a new crate beside its recipe: `wgpu-hello-headless` renders a triangle through wgpu's Vulkan backend into a texture and writes a 256x256 PNG; `wgpu-hello-window` opens a winit window and shows the triangle turning, rendered by wgpu into a texture and copied to the window with softbuffer. On the Linux build machine (Intel GPU, Mesa 26.2) the headless binary wrote the expected triangle. Not yet cooked or run on FerrionOS.
- Mesa's prebuilt package has lavapipe (`/usr/lib/libvulkan_lvp.so` and `/usr/share/vulkan/icd.d/lvp_icd.x86_64.json`, Vulkan 1.1.274), but no FerrionOS package had a Vulkan loader, and lavapipe exports only the ICD entry points (`vk_icd*`), not `vkGetInstanceProcAddr`, so wgpu cannot load it directly. `recipes/wip/libs/graphics/libvulkan` (Khronos loader `vulkan-sdk-1.4.350.1`) stopped at CMake with "Unsupported Platform!"; with `ferrion-redox-platform.patch` it cooks and installs `libvulkan.so.1`. Not yet proven from a clean fetch or run.
- Expected for the window: Mesa's Vulkan has no Orbital surface extension, so `wgpu-hello-window` only tries `create_surface` and prints its result; presenting goes through softbuffer `[INFERENCE]`.

## Sound (AU0), 2026-11-21

What the x86_64 desktop image plays, recorded by QEMU's `wav` backend and compared with its source frame by frame. `scripts/vm-test.py audio-baseline` boots four times with `audio=no` and its own `-audiodev wav,id=snd0,path=<file> -device ich9-intel-hda -device hda-output,audiodev=snd0`, logs in as root on the serial console, makes 6 s of 16-bit stereo at 44.1 kHz with `seq 100000 251201 | head -c 1058400` and writes it to `/scheme/audio` with `dd bs=4096`. The host builds the same bytes and runs the checker, which `scripts/vm-test.py --check-wav` also runs on its own: it passes E1, a recording of a Linux guest playing a 6-second counter signal through virtio-sound on the same QEMU, kept on the build host in `build/audio-rig/e1.wav` (264,600 of 264,600 frames exact in one piece) and fails it with one bit of frame 100,000 changed (264,599 exact, one foreign frame, a break at 2.268 s).

- Host: QEMU 11.1.3 with KVM, while other builds ran; the load average over one minute is given per run. Guest: `make qemu`'s 4 CPUs and 2,048 MiB.
- Volume 100 is audiod's gain of 1; the default volume is 50, which audiod plays as each sample times 0.125, truncated, and the checker expects exactly that.
- Load is two `bash` busy loops per guest CPU, started half a second before the write.
- "Exact" counts source frames that some piece of the recording holds unchanged; "pieces" counts runs that continue the source, so silence inserted into the stream splits a piece without losing a frame; "repeated" counts frames played twice. No run had more than 5 foreign frames.

|Run|Image|Host load|Volume 100|Default volume|Volume 100, load|Default volume, load|
|---|---|---|---|---|---|---|
|1|The day's image|15|264,600 exact, 1 piece|264,600 exact, 1 piece|264,600 exact, 192 pieces, 9,989 repeated|264,600 exact, 11 pieces|
|2|With `ferrion-ihdad-ring.patch`|23|264,600 exact, 1 piece|264,600 exact, 1 piece|264,142 exact (458 lost), 23 pieces|257,803 exact (6,797 lost), 79 pieces|
|3|The same|47|249,833 exact (14,767 lost), 39 pieces|264,600 exact, 1 piece|264,526 exact (74 lost), 12 pieces|264,579 exact (21 lost), 10 pieces|
|4|The same|41|264,600 exact, 1 piece|264,600 exact, 1 piece|264,600 exact, 14 pieces|263,502 exact (1,098 lost), 37 pieces|
|5|The same|30|264,600 exact, 1 piece|262,552 exact (2,048 lost), 2 pieces|264,539 exact (61 lost), 10 pieces|264,098 exact (502 lost), 19 pieces|

Without load the stream is exact in one piece in 8 of 10 recordings; the other two lost 2,048 frames in one gap, or 14,767 in 39 pieces on the most loaded host. A first trial run on the day's image also lost 2,048 frames in one gap at 3.8 s. With two busy loops per CPU no recording was in one piece: silence is inserted, frames repeat, and up to 6,797 are lost. The patch touches only the ring path, which these runs do not use. For comparison, a Linux guest playing through ALSA on the same QEMU stayed in one exact piece with two busy loops per CPU at buffers of 512 to 2,048 frames.

## Sound (AU2), 2026-11-21

With `ferrion-audiod` and AU1's rebuilt base package, `audio-baseline` kept all 264,600 source frames in one piece at both volume 100 and the default 50 without guest load. With two busy loops per guest CPU, volume 100 kept 264,343 frames in 18 pieces (257 lost, 232 repeated), and volume 50 kept 264,351 in 15 pieces (249 lost, 217 repeated). All four had zero foreign frames. The host's one-minute load averages were 22.03, 19.98, 18.01 and 16.85, respectively. These results are within AU0's observed range; they do not claim glitch-free playback under load. A separate legacy playback boot still showed AU0's known 2,048-frame gap.

`audio-ring` passed: 20 rounds, 21 driver descriptors before and after, and all 264,600 frames exact in one piece. Boot, shells, lockdown and driver-namespaces passed on the same image; the namespace check now names `ferrion-audiod` and still requires only `memory` and `rand`. Evidence: `build/worktrees/au2/build/audio-verified/` and its sibling `audio-verified.log`.

## Firefox startup

`scripts/vm-test.py firefox-start --config firefox --timeout 120 --make QEMU_MEM=4096` measures the first `fc-match sans-serif` in a fresh boot, then three screenshots of a fixed local page, using a new profile for the first and reusing it thereafter. Screenshot times stop at the saved-PNG message, not process exit. Since font matching comes first, cold means a new profile rather than an unscanned font set. Serial polling resolves roughly 0.1 s. Launch waits honor `--timeout`.

The initial copied image produced 0.501 s for font matching, 5.707 s for the cold screenshot and 1.502 s for the second. The third saved no PNG within 600.095 s. This is an incomplete baseline: that image predates the CJK collection and directory-mtime fix, and every launch logged remote-settings IndexedDB errors. Evidence: `build/worktrees/fxstart/build/fxstart-red/`.

The first storage error was `SQLITE_IOERR_DIR_FSYNC` (1290), errno 9: after SQLite unlinks its journal, RedoxFS refused fsync on the parent directory. The scoped filesystem patch fixes that call without changing SQLite's durability settings. The `directory-fsync` regression probe is green for read-only directories with and without O_DIRECTORY, including synchronization after creation and unlink, and still rejects an invalid descriptor.

A freshly rebuilt baseline with the CJK package and directory-mtime patch, but without the cache/policy/fsync changes, measured 1.202 s for font matching, 11.330 s cold and 1.603 s warm. The third launch again timed out without a PNG. Evidence: `build/worktrees/fxstart/build/fxstart-baseline/`.

| Measurement | Rebuilt baseline | Compatible-cache run | Final image |
|---|---:|---:|---:|
| First font match | 1.202 s | 0.200 s | 0.503 s |
| Cold screenshot | 11.330 s | Timed out at 600 s | Timed out at 120 s |
| Second screenshot | 1.603 s | Not reached | Not reached |
| Third screenshot | Timed out at 600 s | Not reached | Not reached |

The first match on both cache-enabled runs validates shipped directory timestamps and performs no cache rewrite. The native host's fontconfig 2.18.4 writes format 12, incompatible with the guest's format 9; the successful image uses the existing container's fontconfig 2.15.0. The installer rejects incompatible formats; `mk/config.mk` defaults `FC_CACHE` to `scripts/fc-cache-container.sh` when `PODMAN_BUILD=1` (and `fc-cache` inside a container or on native builds) so the build container's `fc-cache` runs against the image root, and honors a user-supplied `FC_CACHE` override. Evidence: `build/worktrees/fxstart/build/fxstart-cache9/` and `build/worktrees/fxstart/build/fxstart-ready/`.

Final-image checks: directory-fsync, boot and shells pass (`fxstart-final/`); headless local/HTTPS and CJK glyph/reference and regional-family checks pass (`fxstart-accepted/`). The shared shell-prompt helper fixes page-setup races under slow Nushell startup. All directories above are under `build/worktrees/fxstart/build/`.

The complete startup performance goal is **not accepted**: screenshots intermittently stall with either cold or reused profiles (issue `.scratch/firefox/issues/05-startup-stall.md`). Cold remote-settings initialization now executes record inserts and COMMIT, but reopening the database still fails with `SQLITE_IOERR_LOCK` (3850), including in an independent read-only SQLite reader. Issue 02 remains blocked on FileLocks; persistence readback and error-free warm starts are not proven. No locking, process-isolation or SQLite-durability workaround was applied.

## Networking (NW0), 2026-11-22

What the `x86_64` `net-test` image (`config/x86_64/net-test.toml`, `desktop.toml` plus `net-probe` and `iperf3` 3.20) does in the unprivileged network lab (`scripts/net_lab.py`, [ADR 0044](adr/0044-network-lab-and-baseline.md)) on both virtual network cards (`e1000` with `e1000d`, and `virtio-net` with `virtio-netd`), measured across two separate topologies: Topology A (`lab_tap`, guest-to-host over `tap0` with `dnsmasq` and `tc netem`) and Topology B (`qemu_user`, `-netdev user` inside the same isolated network namespace with no route out).

- Conditions: QEMU 11.1.3 with KVM (`-machine q35 -smp 4 -m 2048 -enable-kvm -cpu host`), `vhost=off`, no CPU pinning, host `iperf3` 3.22.1, guest `iperf3` 3.20, component commits `kernel` `e7f1d4231d0f`, `base` `20f7573b4dc0`, `relibc` `74b5e7e31504`.
- All four passes below ran inside quiet windows with `max_load1 <= 5.83` (`e1000` Pass 1: `load1` start `1.67`, end `2.17`, max `5.24`; `e1000` Pass 2: start `3.60`, end `1.91`, max `5.01`; `virtio-net` Pass 1: start `4.46`, end `3.86`, max `5.79`; `virtio-net` Pass 2: start `5.42`, end `2.93`, max `5.83`). They predated the live QEMU `nice`/policy recorder and `/proc` foreign-QEMU detector, so their `nice` column reads `not recorded` and they were compared with `scripts/vm-test.py --legacy-unrecorded-nice --compare-net-perf <pass1> <pass2>` (which prints `nice not recorded` in its verdict; the default comparison answers `BLOCKED` when `nice` is unrecorded).
- Guest CPU columns (`ns/drv/tot`) report median `/scheme/sys/context` `ACTTIME` deltas in seconds per run for `netstack`, the card driver (`e1000d` or `virtio-netd`), and `tot` (sum over processes alive before and after the run). Guest `iperf3` CPU: not measured (its own `cpu_utilization_percent.host_total` fields read `0.0`).
- Repeatability status: NW0's "twice within noise" rule (`|med1 - med2| <= max(0.10 * max(|med1|, |med2|), spread1, spread2)`) is **met for `e1000`** (`PASS compare-net-perf: 17 measures within noise (nice not recorded)`; `lab_tap.tcp_1b_echo` completed 2 of 3 runs in Pass 2 and is excluded from the checked count) and **not met for `virtio-net`** (`FAIL compare-net-perf: 2/18 outside noise (nice not recorded)`). The two `virtio-net` misses are `lab_tap.tcp_rx_rtt50ms` (`0.1833` vs `0.1179 MB/s`: across six `virtio-net` passes its runs fall in discrete steps of about `0.013 MB/s`, `0.118` to `0.197 MB/s`; NW1 addresses the cause) and `qemu_user.tcp_tx_4` (missed its bound by `0.01 MB/s`: `13.64` against `15.67 MB/s`, bound `2.02`).
- Notes on red and retried measures: `lab_tap.tcp_loopback` is a red finding on both cards (`unable to receive parameters from client: Operation already in progress`); `lab_tap.ping_100` receives `0/100` replies on both cards; 4-stream runs lost 1 of 3 runs on the control connection and were retried up to `MAX_IPERF_ATTEMPTS` (5) to collect 3 valid samples (cause: a separate open issue in the stack).
- `net-basics` pre-NW1 status (with `net-hardening` passing on the same image and card): `e1000` passes 4 of 9 items (green: `2_idle_http_rtt`, `4_isn_and_ports`, `5_dns_reply_match`, `9_arp_cache_bound`; red: `1_full_size_frames`, `3_delayed_download_50ms`, `6_ping_100`, `7_tcp_nodelay_1b_echo`, `8_so_error_closed_port`), and `virtio-net` passes 3 of 9 items (same red items plus `2_idle_http_rtt`).

### Topology A (`lab_tap`)

| Measure | Card | Pass 1 median (spread, runs) | Pass 2 median (spread, runs) | Pass 1 / Pass 2 `max_load1` | Pass 1 / Pass 2 `nice` | Pass 1 CPU `ns/drv/tot` (s) | Within noise |
|---|---|---:|---:|---:|---|---:|---|
| `tcp_tx_1` (MB/s) | `e1000` | 51.21 (1.41, 3/3) | 49.35 (1.56, 3/3) | 3.26 / 4.37 | not recorded | 3.84 / 8.05 / 13.03 | Yes |
| `tcp_tx_1` (MB/s) | `virtio-net` | 17.63 (0.64, 3/3) | 17.44 (2.49, 3/3) | 4.72 / 5.01 | not recorded | 1.67 / 8.86 / 13.28 | Yes |
| `tcp_rx_1` (MB/s) | `e1000` | 70.61 (1.88, 3/3) | 69.63 (3.66, 3/3) | 3.98 / 5.01 | not recorded | 3.99 / 7.87 / 13.18 | Yes |
| `tcp_rx_1` (MB/s) | `virtio-net` | 58.04 (4.80, 3/3) | 59.43 (0.92, 3/3) | 4.98 / 4.97 | not recorded | 3.22 / 8.14 / 14.54 | Yes |
| `tcp_tx_4` (MB/s) | `e1000` | 57.10 (2.68, 3/3) | 58.31 (2.12, 3/3) | 4.04 / 5.01 | not recorded | 3.77 / 9.21 / 14.27 | Yes |
| `tcp_tx_4` (MB/s) | `virtio-net` | 13.57 (3.39, 3/3) | 15.83 (5.27, 3/3) | 5.79 / 5.28 | not recorded | 1.31 / 6.41 / 10.68 | Yes |
| `tcp_rx_4` (MB/s) | `e1000` | 76.27 (3.76, 3/3) | 75.52 (3.83, 3/3) | 5.24 / 4.67 | not recorded | 3.71 / 9.12 / 14.24 | Yes |
| `tcp_rx_4` (MB/s) | `virtio-net` | 69.39 (1.23, 3/3) | 72.36 (2.24, 3/3) | 4.76 / 5.20 | not recorded | 2.68 / 8.79 / 14.94 | Yes |
| `udp_1200` (MB/s, 10M target) | `e1000` | 1.2491 (0.0011, 3/3, 0% loss, 0.052 ms jit) | 1.2487 (0.0003, 3/3, 0% loss, 0.047 ms jit) | 5.24 / 4.65 | not recorded | 0.30 / 0.45 / 1.58 | Yes |
| `udp_1200` (MB/s, 10M target) | `virtio-net` | 1.2493 (0.0013, 3/3, 0% loss, 0.147 ms jit) | 1.2492 (0.0009, 3/3, 0% loss, 0.132 ms jit) | 4.59 / 5.20 | not recorded | 0.17 / 0.67 / 1.93 | Yes |
| `tcp_rx_rtt50ms` (MB/s) | `e1000` | 1.2183 (0.0137, 3/3) | 1.2186 (0.0133, 3/3) | 3.52 / 3.32 | not recorded | 0.18 / 0.39 / 1.42 | Yes |
| `tcp_rx_rtt50ms` (MB/s) | `virtio-net` | 0.1833 (0.0263, 3/3) | 0.1179 (0.0131, 3/3) | 3.28 / 3.88 | not recorded | 0.02 / 0.07 / 0.94 | No (discrete steps) |
| `tcp_rx_rtt50ms_loss1pct` (MB/s) | `e1000` | 0.5108 (0.2094, 3/3) | 0.4190 (0.1049, 3/3) | 3.01 / 2.36 | not recorded | 0.07 / 0.16 / 1.04 | Yes |
| `tcp_rx_rtt50ms_loss1pct` (MB/s) | `virtio-net` | 0.0786 (0.0393, 3/3) | 0.0786 (0.0132, 3/3) | 3.01 / 3.33 | not recorded | 0.01 / 0.07 / 0.94 | Yes |
| `tcp_loopback` | `e1000` | red (`Operation already in progress`) | red (`Operation already in progress`) | 2.17 / 1.91 | not recorded | — | Red |
| `tcp_loopback` | `virtio-net` | red (`Operation already in progress`) | red (`Operation already in progress`) | 2.44 / 3.33 | not recorded | — | Red |
| `idle_http_rtt` (ms, p50) | `e1000` | 1.474 (0.164, 3/3, max 8.464) | 1.457 (0.107, 3/3, max 4.234) | 2.20 / 1.86 | not recorded | 0.01 / 0.01 / 0.78 | Yes |
| `idle_http_rtt` (ms, p50) | `virtio-net` | 16.904 (413.019, 3/3, max 504.736) | 16.624 (328.345, 3/3, max 505.520) | 2.44 / 4.27 | not recorded | 0.01 / 0.06 / 0.86 | Yes |
| `tcp_1b_echo` (ms, p50) | `e1000` | 0.141 (0.006, 3/3, p99 0.409) | 0.111 (0.065, 2/3, p99 1.224) | 2.04 / 1.77 | not recorded | 0.01 / 0.02 / 0.39 | Excluded (2/3 runs in Pass 2) |
| `tcp_1b_echo` (ms, p50) | `virtio-net` | 0.119 (0.006, 3/3, p99 503.541) | 0.137 (0.129, 3/3, p99 503.535) | 2.86 / 3.21 | not recorded | 0.01 / 0.03 / 0.56 | Yes |
| `udp_1b_echo` (ms, p50) | `e1000` | 0.121 (0.068, 3/3, p99 0.442) | 0.112 (0.027, 3/3, p99 0.273) | 2.04 / 1.87 | not recorded | 0.01 / 0.01 / 0.37 | Yes |
| `udp_1b_echo` (ms, p50) | `virtio-net` | 0.128 (0.031, 3/3, p99 503.490) | 0.107 (0.019, 3/3, p99 503.396) | 3.12 / 3.19 | not recorded | 0.01 / 0.03 / 0.57 | Yes |
| `ping_100` | `e1000` | no_reply (0/100) | no_reply (0/100) | — | not recorded | — | Red |
| `ping_100` | `virtio-net` | no_reply (0/100) | no_reply (0/100) | — | not recorded | — | Red |

### Topology B (`qemu_user`)

| Measure | Card | Pass 1 median (spread, runs) | Pass 2 median (spread, runs) | Pass 1 / Pass 2 `max_load1` | Pass 1 / Pass 2 `nice` | Pass 1 CPU `ns/drv/tot` (s) | Within noise |
|---|---|---:|---:|---:|---|---:|---|
| `tcp_tx_1` (MB/s) | `e1000` | 46.63 (1.13, 3/3) | 46.03 (1.83, 3/3) | 3.04 / 3.24 | not recorded | 4.09 / 7.80 / 13.03 | Yes |
| `tcp_tx_1` (MB/s) | `virtio-net` | 13.29 (3.03, 3/3) | 12.93 (1.64, 3/3) | 4.18 / 4.71 | not recorded | 1.40 / 6.35 / 10.17 | Yes |
| `tcp_rx_1` (MB/s) | `e1000` | 75.91 (2.86, 3/3) | 76.56 (4.86, 3/3) | 3.89 / 3.93 | not recorded | 4.36 / 7.51 / 13.01 | Yes |
| `tcp_rx_1` (MB/s) | `virtio-net` | 60.47 (1.00, 3/3) | 55.49 (2.56, 3/3) | 4.18 / 5.33 | not recorded | 3.17 / 7.54 / 13.01 | Yes |
| `tcp_tx_4` (MB/s) | `e1000` | 62.18 (3.01, 3/3) | 62.29 (3.27, 3/3) | 3.89 / 4.17 | not recorded | 3.97 / 8.76 / 13.86 | Yes |
| `tcp_tx_4` (MB/s) | `virtio-net` | 13.64 (2.02, 3/3) | 15.67 (0.85, 3/3) | 4.73 / 5.52 | not recorded | 1.56 / 7.86 / 12.52 | No (diff 2.03 > 2.02) |
| `tcp_rx_4` (MB/s) | `e1000` | 73.30 (3.67, 3/3) | 70.93 (16.03, 3/3) | 3.96 / 4.16 | not recorded | 4.62 / 7.36 / 13.39 | Yes |
| `tcp_rx_4` (MB/s) | `virtio-net` | 66.95 (1.69, 3/3) | 67.10 (5.07, 3/3) | 5.37 / 5.83 | not recorded | 3.01 / 9.08 / 15.62 | Yes |
| `udp_1200` (MB/s, 10M target) | `e1000` | 1.2487 (0.0005, 3/3, 0% loss, 0.047 ms jit) | 1.2489 (0.0007, 3/3, 0% loss, 0.062 ms jit) | 3.96 / 3.90 | not recorded | 0.32 / 0.46 / 1.59 | Yes |
| `udp_1200` (MB/s, 10M target) | `virtio-net` | 1.2488 (0.0004, 3/3, 0% loss, 0.090 ms jit) | 1.2494 (0.0009, 3/3, 0% loss, 0.071 ms jit) | 5.37 / 5.83 | not recorded | 0.19 / 0.73 / 2.07 | Yes |
| `idle_http_rtt` (ms, p50) | `e1000` | 1.686 (0.350, 3/3, max 5.476) | 1.708 (0.177, 3/3, max 4.564) | 3.03 / 2.71 | not recorded | 0.06 / 0.02 / 0.84 | Yes |
| `idle_http_rtt` (ms, p50) | `virtio-net` | 8.270 (3.944, 3/3, max 483.609) | 9.922 (480.170, 3/3, max 1003.593) | 4.37 / 4.78 | not recorded | 0.06 / 0.08 / 0.95 | Yes |
| `tcp_1b_echo` (ms, p50) | `e1000` | 0.153 (0.063, 3/3, p99 0.482) | 0.145 (0.070, 3/3, p99 0.375) | 2.28 / 1.91 | not recorded | 0.01 / 0.02 / 0.40 | Yes |
| `tcp_1b_echo` (ms, p50) | `virtio-net` | 0.173 (0.091, 3/3, p99 12.310) | 0.121 (0.054, 3/3, p99 12.298) | 4.11 / 3.52 | not recorded | 0.01 / 0.03 / 0.48 | Yes |
| `udp_1b_echo` (ms, p50) | `e1000` | 0.134 (0.002, 3/3, p99 0.415) | 0.137 (0.005, 3/3, p99 0.470) | 2.28 / 1.91 | not recorded | 0.01 / 0.01 / 0.37 | Yes |
| `udp_1b_echo` (ms, p50) | `virtio-net` | 0.124 (0.029, 3/3, p99 503.687) | 0.105 (0.016, 3/3, p99 503.274) | 4.39 / 3.48 | not recorded | 0.01 / 0.03 / 0.62 | Yes |

## Monorepo cutover (ADRs 0027 and 0047), 2026-11-24

Landed on `master` at `5958f13f6..77c259efc` (106 commits on `191befd3e`) and `ferrion-hardware` `main` at `2802feb34..c222cfd8` (26 commits on `20da44c`). The four core trees (`kernel`, `relibc`, `base` without its drivers, `redoxfs`) live under `components/`; base's drivers live under `drivers/` in `ferrion-hardware` (GPL-2.0-only beside upstream's MIT notice), pinned in `recipes/core/drivers/recipe.toml` at `c222cfd8`.

| Measure | Before (`01b232978`) | Target | After (`81bc51915` / `77c259efc`) | Status |
|---|---|---|---|---|
| Cold cook CPU (`relibc`, `kernel`, `redoxfs`, `base`, `bootloader`, plus `drivers`, `initfs` after; paired alternating runs on a second, idle build host at `load1` under 2, `SCCACHE_BUILD=0`, `nice -4` uniform) | 1,054.21 CPU-s (wall 171.30 s) | <= 1.05x before | 1,130.63 CPU-s (+7.25%, wall 171.71 s, +0.24%); unchanged 4 matched +0.01% (354.94 -> 354.97 CPU-s); split `base` 699.25 -> `base` 223.48 + `drivers` 526.92 + `initfs` 25.22 CPU-s | Missed, accepted by the maintainer (shared crates compile twice; follow-up filed) |
| Fresh clone to desktop image (Step 0 on primary host vs Step 12 on a second build host, `SCCACHE_BUILD=0`) | 50m30s real (`074ac6d76`, resumed after restart) | informational (different hosts) | 47m25s real (2,845.65 s, 21,873.88 CPU-s, 105 cooks, 0 failed); `PASS boot 1/1` (3.1 s), `PASS shells 1/1` (5/5); 4/4 `source_identifier` == `HEAD:components/<name>` (no `-dirty`); booted `/scheme/sys/uname` carries `9043c1b7cd8828205a4f4f0999841e6eaf9e9cde` | Pass (timing informational only) |
| Pack size of a single-branch clone of `master` (`git count-objects -v`, fresh bare fetches from GitHub on a second build host) | 9,924 KiB at `191befd3e` (plan baseline: 8,365 KiB at `168362176`) | <= 12,288 KiB (cutover delta <= +3,923 KiB) | 13,191 KiB at `77c259efc` (target <= 12,288; cutover delta `191befd3e` -> `77c259efc`: +3,267 KiB) | Missed and accepted by the maintainer |
| Pack size of a single-branch clone of `ferrion-hardware` `main` (`git count-objects -v`, fresh bare fetches from GitHub on a second build host) | 5,101 KiB at `20da44c` (plan baseline: 2,921 KiB at `fd715e4`) | <= 4,096 KiB (cutover delta <= +1,175 KiB) | 5,769 KiB at `c222cfd8` (target <= 4,096; cutover delta `20da44c` -> `c222cfd8`: +668 KiB; 5,770 KiB at `b9a243138`) | Missed and accepted by the maintainer |
| Local clone time, median of 5 alternating pairs in the same minute (`git clone --no-local --single-branch`, on a second build host at `load1` 14.56 -> 14.66) | 0.579 s at `191befd3e` (0.584, 0.313, 0.579, 0.354, 0.776 s; plan: 0.359 s on 2026-11-21) | <= 2x before | 0.647 s at `77c259efc` (0.619, 0.647, 0.702, 0.607, 0.984 s; 1.117x) | Met (information only) |
| Clone from GitHub, median of 3 (`git clone --single-branch`, on a second build host at `load1` 14.66 -> 13.01) | 3.67 s on 2026-11-21 (all branches) | <= 8 s | 3.340 s at `77c259efc` (3.340, 2.894, 3.818 s) | Met (information only) |
| Base split byte-for-byte completeness (`verify-split.sh`) | 656 files in `base` | every file in one repo, dropped named | 656 = 301 in `ferrion-hardware/drivers/` + 348 in `components/base` + 7 `ixgbed` dropped; 39 patch parts reversed, 0 missing | Pass |
| Stage diff (`base` vs `base` + `drivers` + `initfs`) | `before-stage-base.txt` | exact 5 lines | 5 lines (`-ixgbed`, `-ixgbed.toml`, +3 license files under `/usr/share/licenses/drivers/`); initfs 0-line diff (49 names); 0 FerrionOS fetches in `drivers.log` | Pass |
| No MIT code links a driver crate (`scripts/components-check.py`) | Red on 2 lines of `ferrion-wm-backend` | Green | Green (`Every tree under components/ matches its record.`) | Pass |
| Acceptance VM checks (Step 11, 14 checks) | Step 2 baselines (`8/8` original + `lxhost` green + `hw-live` red on `cpu-24-usb`) | No regressions | `boot`, `shells`, `lockdown`, `graphics-bounds`, `gpu-heads`, `boot gpu=virtio`, `net-hardening`, `net-calls`, `disk-keys` (`VERIFIED_BOOT=1`), `nvme-queue`, `fork-wake`, `firefox-headless` all PASS; `lxhost-link.py` GREEN (`f0c4286 -> c222cfd8`, 311/12/258/41/0/0); paired idle `net-basics` (`e1000`) 4/9 before and 4/9 after (same items green as above); paired idle `audio-ring` 3/3 before and 3/3 after (primary-host high-load runs had 2 failures at load > 80 and 1 pass at load 5.57); `hw-live` 45/45 initfs entries (0-line diff) and 2/2 runs match Step 2 (`safety` 5/5 refused, `cpu-1-ps2` + `cpu-24-ps2` PASS, `cpu-24-usb` pre-existing `Lost event TRB`) | Pass (0 regressions) |
| Upstream carry cost (`scripts/component-upstream.sh check`) | `kernel`: 4 commits, 0 conflicts (plan); `base`: at pin | reported | `kernel`: 5 upstream commits, 2 conflicts; `base`: 0 commits, merges cleanly | Recorded |

## Media decode (M1a), 2026-11-24

Quiet-window 4K AV1 software decode on the `x86_64` `desktop` image (`scripts/vm-test.py media-decode`, `ffmpeg 9.1` with `libdav1d 1.5.5`, `4` vCPUs, `QEMU 11.1.3` with KVM, on a second build host): `114.16 fps` (`150` frames in `1.314 s`, host `/proc/loadavg` `2.74 4.69 7.01` before and `2.84 4.31 6.70` after). Input is synthetic `testsrc2` (`3840x2160`, `30 fps`, encoded with SVT-AV1 preset 10), so this is a synthetic-content figure, not a real-video figure. The same boot passed all functional checks (`PASS media-decode 1/1`: `7` video `framemd5` exact, `1` exact audio, `4` lossy audio SNR >= 60 dB, `45` allowed decoders, `4` allowed encoders). Evidence: `build/second-host/m1a-quiet-4k/`.
