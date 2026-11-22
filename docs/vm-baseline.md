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

The first match on both cache-enabled runs validates shipped directory timestamps and performs no cache rewrite. The native host's fontconfig 2.18.4 writes format 12, incompatible with the guest's format 9; the successful image uses the existing container's fontconfig 2.15.0. The installer rejects incompatible formats and accepts `FC_CACHE` to select a compatible executable. Evidence: `build/worktrees/fxstart/build/fxstart-cache9/` and `build/worktrees/fxstart/build/fxstart-ready/`.

Final-image checks: directory-fsync, boot and shells pass (`fxstart-final/`); headless local/HTTPS and CJK glyph/reference and regional-family checks pass (`fxstart-accepted/`). The shared shell-prompt helper fixes page-setup races under slow Nushell startup. All directories above are under `build/worktrees/fxstart/build/`.

The complete startup performance goal is **not accepted**: screenshots intermittently stall with either cold or reused profiles (issue `.scratch/firefox/issues/05-startup-stall.md`). Cold remote-settings initialization now executes record inserts and COMMIT, but reopening the database still fails with `SQLITE_IOERR_LOCK` (3850), including in an independent read-only SQLite reader. Issue 02 remains blocked on FileLocks; persistence readback and error-free warm starts are not proven. No locking, process-isolation or SQLite-durability workaround was applied.
