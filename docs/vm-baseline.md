# VM baseline

What upstream's daily images do in a virtual machine, measured 2026-11-20 for step V0 and A0 of [`plans/product-roadmap.md`](../plans/product-roadmap.md). Upstream images are built from the same component sources this repository fetches, so they stand in for a FerrionOS image until one can be built here. Rerun these checks on every new FerrionOS image and keep the old columns for comparison.

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

1. Upstream's bootloader waits at its resolution menu with no timeout, so an unattended VM never boots. Fixed for FerrionOS on 2026-11-20 by `recipes/core/bootloader/ferrion-boot-timeout.patch`: on UEFI the highlighted mode boots after 3 seconds without a key, and the image built with it reached the login screen with no key pressed.
2. Upstream's bootloader panics when the serial port is a socket instead of a file: "Failed to set max mode" and then `src/os/uefi/mod.rs:362:87` unwrapping a "device error" (bootloader 1.0.0). The same patch ignores that error, since highlighting is cosmetic.
3. `make qemu` looked for UEFI firmware in `/usr/share/ovmf`, `/usr/share/OVMF`, `/usr/share/AAVMF` and QEMU's own share folder, not in `/usr/share/edk2/x64/` and `/usr/share/edk2/aarch64/`, where Arch-based distributions install it, so an x86_64 guest fell back to legacy BIOS boot without a message. `mk/qemu.mk` now looks there too.
4. Upstream branding shows in the running system: the login background is the Redox logo from the `orbdata` package, the bootloader printed "Redox OS Bootloader", the kernel logs "Redox OS starting...", and the console greeted with a "Redox OS" banner from `/etc/issue`. The bootloader patch renames its banners, and `config/base.toml` now writes `/etc/issue`; the logo and the kernel line remain.
5. The login screen has `user` filled in and accepts an empty password: the dev accounts of `config/base.toml`.
6. A QEMU without its optional display modules has no virtio-gpu devices and no window: on split-package distributions `make qemu gpu=virtio-gl` needs the `virtio-vga-gl`, GTK and OpenGL modules installed.
