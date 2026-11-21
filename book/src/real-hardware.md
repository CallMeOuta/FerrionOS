# Running FerrionOS on Real Hardware

> ⚠️ **Warning:** FerrionOS has not been run on physical hardware yet. This page describes the path that the build system inherits from upstream. Back up the data of a machine before you try FerrionOS on it.

## Before You Start

- **Accounts.** The repository's configurations build images with development accounts: `user` with an empty password, and `root` with the password `password`. An image that leaves the build machine needs its own accounts ([Security](./security.md)).
- **Hardware.** [Hardware Support](./hardware-support.md) lists the drivers that the source contains. There is no driver for Wi-Fi or Bluetooth, and there is one USB host-controller driver, for xHCI. A PS/2 driver exists for the keyboards and touchpads that use that interface.
- **Memory.** A live image is loaded into memory as a whole, so the machine needs more memory than the image is large.

## Build a Live Image

From the FerrionOS directory, after a build ([Building FerrionOS](./podman-build.md)):

```sh
make live
```

This writes `build/<ARCH>/<CONFIG_NAME>/redox-live.iso`, for example `build/x86_64/desktop/redox-live.iso`. The bootloader of a live image loads the filesystem into memory and presents it as a disk, so the system runs without changing the disks of the machine.

## Write the Image to a USB Drive

Writing the image erases the USB drive.

- On Linux, [Popsicle](https://github.com/pop-os/popsicle) writes an image to a USB drive. With its `popsicle-gtk` program installed, `make popsicle` builds the live image if needed and opens it in Popsicle.
- `dd` works as well: [Testing on Real Hardware](./coding-and-building.md#testing-on-real-hardware) shows the command and how to find the right device.
- On Windows, [balenaEtcher](https://etcher.balena.io/) writes an image to a USB drive.

## Boot the Machine

Some computers do not boot from USB until it is allowed in the UEFI or BIOS setup. The key that opens the setup or the boot menu differs between machines; it is often Esc or one of the function keys.

On UEFI the boot is quiet: a black screen, a splash, then the graphical login ([Boot Process](./boot-process.md#quiet-boot)). Press a key in the first second to see the bootloader's menu and the text log, which is the first thing to try when the boot stops. A BIOS boot shows the menu on every boot.

Log in with the accounts of your image. To switch between the graphical session and the consoles, hold the **Super** key:

- **Super+F1**: the boot log
- **Super+F2**: a text console
- **Super+F3**: the graphical session

See [Trying Out FerrionOS](./trying-out-redox.md) for things to try, and [Installing FerrionOS](./installing.md) for the installer.

## Report What You See

Testing on hardware is one of the ways to help FerrionOS. A report that names the machine, the image and what happened, with a photo of the screen when the boot stops, is welcome: see [Participating](./contributing.md).
