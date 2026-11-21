# Hardware Support

FerrionOS has been run only in virtual machines so far. Nothing on this page has been verified on physical hardware by FerrionOS.

## What FerrionOS Has Verified

The repository's [VM baseline](https://github.com/CallMeOuta/FerrionOS/blob/master/docs/vm-baseline.md) records every measurement with its setup and evidence. In short:

- **x86_64 in QEMU.** The desktop image built from the repository boots with KVM and UEFI firmware to the graphical login, on QEMU's `q35` machine with a VGA display, an NVMe disk and an e1000 or virtio-net network card. `scripts/vm-test.py` repeats this check ([Testing](./testing-practices.md)).
- **aarch64 in QEMU.** Measured on upstream's image, which is built from the same component sources: no device configuration reached a graphical login. Live mode reached a text login.
- **i586 and riscv64gc.** The build system has configurations for them. FerrionOS has not built or booted them.

A report of a boot on a real machine, or in another virtual machine, is welcome: see [Participating](./contributing.md).

## CPU Requirements

The kernel requires the following features on x86. CPUs of other architectures need their equivalents.

- [MMU](https://en.wikipedia.org/wiki/Memory_management_unit)
- [FPU](https://en.wikipedia.org/wiki/Floating-point_unit)
- The FXSAVE extension
- The [Page Size Extension](https://en.wikipedia.org/wiki/Page_Size_Extension)
- The paging global extension

## Low-end Computers

The main limits of an old computer are the amount of memory and the lack of the SSE and AVX extensions, which programs use to speed up their algorithms. Some modern programs therefore do not work on such a computer, or need a lot of memory for complex tasks. How the system itself behaves on a given old computer has not been verified by FerrionOS.

## Why i586 is the Oldest x86 CPU Targeted

- i686 ([Pentium Pro](https://en.wikipedia.org/wiki/Pentium_Pro)) and its successors introduced [extensions](https://en.wikipedia.org/wiki/P6_(microarchitecture)) such as MMX, SSE and SSE2. The kernel and the other critical system components do not use them.
- i586 ([the original Pentium](https://en.wikipedia.org/wiki/Pentium_(original))) is the oldest x86 CPU that [Rust](https://doc.rust-lang.org/beta/rustc/platform-support.html) supports, and probably the oldest that most Rust packages support.
- [i486](https://en.wikipedia.org/wiki/I486) introduced the FPU and the atomic operations that the kernel and the other critical system components use. Going back to i486 would be possible, with far fewer programs.
- [i386](https://en.wikipedia.org/wiki/I386) has no atomic operations and no floating-point instructions, so it cannot be a target for the kernel and the other critical system components.

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
