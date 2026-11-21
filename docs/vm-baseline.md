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
8. In 1 of about 13 boots the text console (VT 2, with the `/etc/issue` banner and `redox login:`) took the screen instead of the graphical login. Cause, found 2026-11-20: two VT switches raced. `inputd` numbers VTs in the order their consumers open (1 for the boot log, then 2, 3, ...); init spawned orbital before `fbcond@2`, so either could take VT 2, while orbital always ran `inputd -A 3`. The desktop also ran `inputd -A 2` from `30_console_switch`: `config/desktop.toml` included `desktop-minimal.toml` before `server.toml`, and `server.toml` brought `minimal.toml`'s copy of that script back after desktop-minimal's empty override. Usually `fbcond` took VT 2 and orbital's switch to 3 came last; when orbital's switch landed first, init's `-A 2` left the text console on screen. A test config that held init for 3 s before `30_console_switch` showed the text console in 6 of 6 boots. Fixed in the configs, no component patch: `config/desktop.toml` now includes `server.toml` first, so the desktop's `30_console_switch` is empty, and every orbital init script (`desktop-minimal`, `desktop-contain`, `wayland`, `x11`, `redoxer-gui`) requires `30_console.target`, so `fbcond@2` holds VT 2 before orbital starts and orbital's VT is always 3. With the same 3 s hold plus a 2 s hold between orbital and `fbcond`, 6 of 6 boots reached the graphical login; the desktop image (`make image REPO_BINARY=1`) reached it in 30 of 30 quiet boots, each screenshot 8 s after power-on averaging (43, 95, 135), the login screen's color.

## Rust GUI apps (R0), 2026-11-20

- `recipes/wip/demos/wgpu-hello` is a new crate beside its recipe: `wgpu-hello-headless` renders a triangle through wgpu's Vulkan backend into a texture and writes a 256x256 PNG; `wgpu-hello-window` opens a winit window and shows the triangle turning, rendered by wgpu into a texture and copied to the window with softbuffer. On the Linux build machine (Intel GPU, Mesa 26.2) the headless binary wrote the expected triangle. Not yet cooked or run on FerrionOS.
- Mesa's prebuilt package has lavapipe (`/usr/lib/libvulkan_lvp.so` and `/usr/share/vulkan/icd.d/lvp_icd.x86_64.json`, Vulkan 1.1.274), but no FerrionOS package had a Vulkan loader, and lavapipe exports only the ICD entry points (`vk_icd*`), not `vkGetInstanceProcAddr`, so wgpu cannot load it directly. `recipes/wip/libs/graphics/libvulkan` (Khronos loader `vulkan-sdk-1.4.350.1`) stopped at CMake with "Unsupported Platform!"; with `ferrion-redox-platform.patch` it cooks and installs `libvulkan.so.1`. Not yet proven from a clean fetch or run.
- Expected for the window: Mesa's Vulkan has no Orbital surface extension, so `wgpu-hello-window` only tries `create_surface` and prints its result; presenting goes through softbuffer `[INFERENCE]`.
