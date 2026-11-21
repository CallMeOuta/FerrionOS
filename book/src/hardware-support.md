# Hardware Support

FerrionOS has been run only in virtual machines so far. Nothing on this page has been verified on physical hardware by FerrionOS.

## What FerrionOS Has Verified

The repository's [VM baseline](https://github.com/CallMeOuta/FerrionOS/blob/master/docs/vm-baseline.md) records every measurement with its setup and evidence. In short:

- **x86_64 in QEMU.** The desktop image built from the repository boots with KVM and UEFI firmware to the graphical login, on QEMU's `q35` machine with a VGA display, an NVMe disk and an e1000 or virtio-net network card. `scripts/vm-test.py` repeats this check ([Testing](./testing-practices.md)).
- **aarch64 in QEMU.** Measured on upstream's image, which is built from the same component sources: no device configuration reached a graphical login. Live mode reached a text login.
- **riscv64gc.** The build system has configurations for it. FerrionOS has not built or booted it.

A report of a boot on a real machine, or in another virtual machine, is welcome: see [Participating](./contributing.md).

## CPU Requirements

FerrionOS targets `x86_64`, `aarch64` and `riscv64gc` only, with no 32-bit userland or compatibility layer. The kernel requires the following features on x86_64. CPUs of other architectures need their equivalents.

- [MMU](https://en.wikipedia.org/wiki/Memory_management_unit)
- [FPU](https://en.wikipedia.org/wiki/Floating-point_unit)
- The FXSAVE extension
- The [Page Size Extension](https://en.wikipedia.org/wiki/Page_Size_Extension)
- The paging global extension

## Low-end Computers

Memory capacity limits what an older 64-bit computer can run, and some programs require CPU extensions beyond the architecture's baseline. How the system itself behaves on a given old computer has not been verified by FerrionOS.

## Drivers in the Source

The drivers live in the `drivers/` directory of the `base` source (recipe `recipes/core/base`). The table lists the drivers that exist there at the commit the recipe pins. A driver in the source is not a statement that a given device works.

| **Category** | **Drivers** |
|--------------|-------------|
| Hardware interfaces | `acpid` (ACPI), `pcid` (PCI) |
| Storage | `ided` (IDE), `ahcid` (SATA), `nvmed` (NVMe), `usbscsid` (USB mass storage), `virtio-blkd` |
| Video | `vesad` (the framebuffer that BIOS VESA or UEFI GOP provides), `ihdgd` (Intel), `virtio-gpud` |
| Sound | `ac97d`, `ihdad` (Intel HD Audio), `sb16d` |
| Input | `ps2d` (PS/2 keyboard, mouse and touchpad), `usbhidd` (USB keyboard and mouse) |
| USB | `xhcid` (xHCI host controllers), `usbhubd` |
| Network | `e1000d` and `ixgbed` (Intel), `rtl8139d` and `rtl8168d` (Realtek), `virtio-netd` |

There is no driver for Wi-Fi, Bluetooth or I2C devices. Graphics are drawn by the CPU: no driver provides hardware-accelerated 2D or 3D.
