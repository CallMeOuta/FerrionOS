# Running FerrionOS in a Virtual Machine

- [The Image](#the-image)
- [QEMU Instructions](#qemu-instructions)
  - [Options](#options)
  - [UEFI Firmware](#uefi-firmware)
- [Using the Emulation](#using-the-emulation)
- [VirtualBox Instructions](#virtualbox-instructions)
- [Other Virtual Machines](#other-virtual-machines)

FerrionOS has verified one setup: the x86_64 desktop image in QEMU on an x86_64 Linux host, with KVM and UEFI firmware. The other paths on this page are inherited from upstream and not verified by FerrionOS.

## The Image

FerrionOS publishes no images. Build one first, as [Building FerrionOS](./podman-build.md) describes. The image of the default build is `build/x86_64/desktop/harddrive.img`; in general it is `build/<ARCH>/<CONFIG_NAME>/harddrive.img`.

## QEMU Instructions

The bootstrap script installs QEMU ([Building FerrionOS](./podman-build.md)). From the FerrionOS directory, run:

```sh
make qemu
```

`make qemu` starts the image of the current `ARCH` and `CONFIG_NAME` in QEMU and builds it first when it does not exist. It uses KVM when the image's architecture is the host's. A window shows the screen of the guest, and the terminal that ran `make` shows its serial console.

### Options

The options of `make qemu` are make variables. `mk/qemu.mk` defines them and is the reference for their values and defaults. The ones used most:

| Option | Effect |
|---|---|
| `gpu=no` | No window. The serial console in the terminal is the only console. |
| `gpu=virtio` | A VirtIO display device instead of the standard VGA device. `gpu=virtio-gl` adds rendering on the host and needs QEMU's GTK and OpenGL modules. |
| `kvm=no` | Emulates the CPU instead of using KVM, for a host without `/dev/kvm`. |
| `uefi=no` | Boots through BIOS instead of UEFI. The boot is then not quiet. |
| `live=yes` | Boots `redox-live.iso`, which `make live` builds, instead of the hard-drive image. |
| `disk=virtio` | Attaches the image as a VirtIO disk. The default is NVMe; `ata`, `usb` and `cdrom` exist as well. |
| `net=no` | No network card. The default is an e1000 card on QEMU's user-mode network; `net=virtio` and `net=rtl8139` choose another card. |
| `net=redir` | Forwards port 8022 of the host to port 22 of the guest, and port 8080 to port 80. |
| `audio=no`, `usb=no` | No sound device, no USB controller. |
| `qemu_serial_logfile=<file>` | Also writes the serial console to a file. |
| `gdb=yes` | Waits for a debugger; `make gdb` connects to it. |
| `QEMU_SMP=<n>`, `QEMU_MEM=<MiB>` | The number of CPUs and the amount of memory. The defaults on x86_64 are 4 and 2048. |
| `share=<directory>` | Shares a directory of the host ([VirtIO-FS Shared Directories](./virtio-fs.md)). |

Options can be combined, for example `make qemu gpu=no net=redir`. To set one permanently, put it in the `.config` file ([Configuration Settings](./configuration-settings.md)).

Further QEMU flags go into the `QEMUFLAGS` environment variable. On the make command line, `QEMUFLAGS=` replaces every flag that the makefile adds.

With the image built, `make -n qemu` prints the QEMU command line that `make qemu` would run. Use it to start the image by hand or with another front end.

### UEFI Firmware

On x86_64, `make qemu` boots through UEFI and looks for the firmware in these places, in this order:

1. `/usr/share/ovmf/OVMF.fd`
2. `/usr/share/OVMF/OVMF_CODE.fd`
3. `/usr/share/edk2/x64/OVMF.4m.fd`, where Arch-based distributions install it
4. `edk2-x86_64-code.fd` in `/usr/share/qemu` or in the `share/qemu` directory beside the QEMU binary

When it finds none, QEMU boots the image through BIOS without a message. The sign is the bootloader's menu, which then appears on every boot. Install your distribution's UEFI firmware package for QEMU, which is usually named `ovmf` or `edk2-ovmf`, and check that one of these files exists.

## Using the Emulation

The screen stays black for a moment, shows a splash with a progress bar, and then the graphical login ([Boot Process](./boot-process.md#quiet-boot)). Press a key in the first second after the window opens to see the bootloader's menu and the text log instead.

Log in as `user`, with an empty password. The password of `root` is `password`. These are development accounts ([Security](./security.md)).

- **Ctrl+Alt+G** releases the mouse pointer from the QEMU window.
- **Ctrl+Alt+F** switches the window between full screen and windowed.
- **Super+F1**, **Super+F2** and **Super+F3** switch between the boot log, the text console and the graphical session.

With `gpu=no`, the serial console in the terminal shows the prompt `ferrion login:`. Ctrl-C goes to the guest there, and `Ctrl-a h` lists the keys of QEMU's console.

QEMU exits when the guest powers off:

```sh
sudo shutdown
```

To stop a guest that does not respond, end the `qemu-system-x86_64` process from another terminal.

See [Trying Out FerrionOS](./trying-out-redox.md) for things to try.

## VirtualBox Instructions

`make virtualbox` creates a VirtualBox virtual machine named `FerrionOS` from the image and starts it. FerrionOS has not verified this path.

The target is defined in `mk/virtualbox.mk`. It deletes an existing virtual machine of that name, then sets up 2048 MB of memory, 32 MB of video memory, nested paging, a PS/2 keyboard and mouse, an Intel HDA audio controller, no USB, an 82540EM network card on NAT, and a SATA controller with the image converted to `harddrive.vdi`.

## Other Virtual Machines

- [Cloud Hypervisor](./cloud-hypervisor.md): `make ch` boots the image with a serial console only.
- On aarch64, `make qemu` boots the live image by default ([Working with ARM64](./aarch64.md)).
