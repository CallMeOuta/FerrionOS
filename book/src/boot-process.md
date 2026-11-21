# Boot Process

This page follows a boot from the firmware to the login. The file names refer to the sources of the `bootloader`, `kernel` and `base` recipes, which lie in `recipes/core/<name>/source/` after `make f.<name>`.

## Quiet Boot

On UEFI, FerrionOS boots without printing text on the screen:

1. The screen stays black for one second. A key pressed in this second opens the bootloader's menu and brings back the text log of the boot.
2. Without a key, the bootloader takes the best display mode, fills the screen with the background color and starts the kernel.
3. A splash appears: the background color and a progress bar that advances on real boot events.
4. The graphical login appears.

A key pressed while the splash is shown replaces it with the boot log. The kernel's messages go to the serial port in every case. Errors, panics and the password prompt of an encrypted filesystem still appear on the screen.

In QEMU, the desktop image goes from power-on to the graphical login in about 3 seconds this way, and `scripts/vm-test.py boot` checks that no text appears before the login ([Testing](./testing-practices.md)).

A boot through BIOS is not quiet: the bootloader shows its menu and waits, as described below.

Three patches implement the quiet boot. The bootloader patch (`recipes/core/bootloader/ferrion-quiet-boot.patch`) waits for the key and passes `QUIET=1` and `BOOT_BACKGROUND` to the kernel. The kernel patch (`recipes/core/kernel/ferrion-quiet-boot.patch`) keeps the kernel log off the screen when `QUIET=1` is set. The `base` patch (`recipes/core/base/ferrion-boot-splash.patch`) makes `fbbootlogd` draw the splash instead of the boot log.

## Verified Boot

A hard-drive image built with the default settings is a verified-boot image: its UEFI bootloader starts only the kernel and the `initfs` that the image was built with. The build variable is `VERIFIED_BOOT`, which is 1 by default (`mk/config.mk`) and 0 for i586, which boots through BIOS only.

The build writes a manifest with the hashes of both files, and its signature, into the image, and puts the public key into the image's bootloader (`scripts/verified-boot-sign.sh`, which needs FUSE, OpenSSL 3 and Python 3 on the host).

- The first build creates a development key pair in `build/verified-boot/`. Keep it private, and never sign an image that you give away with it.
- If the kernel or the `initfs` on the disk changes, the boot stops with the message "FerrionOS verified boot refused to start" and the reason. The message appears in a quiet boot as well.
- The bootloader's menu, which a key in the first second opens, does not offer the environment editor in such an image.
- After `make p.<recipe>` or `make push` has changed the kernel or the `initfs`, the image must be signed again. Both targets do that when they unmounted the image themselves. When you had mounted it, run `make verified-boot-sign` after `make unmount`.
- `make image VERIFIED_BOOT=0` builds an image whose bootloader verifies nothing and offers the environment editor.
- Verified boot covers the kernel and the `initfs` only. It does not cover the rest of the filesystem, a live image or the BIOS bootloader, and it does not prevent a return to an older signed version. Whoever can write to the disk can replace the bootloader and its key, which only UEFI Secure Boot prevents.
- With UEFI Secure Boot on and the bootloader signed with your own db key, the firmware checks the bootloader, and the bootloader checks the kernel and the `initfs`. `scripts/secure-boot-test.sh <image>` shows both in QEMU ([Testing](./testing-practices.md#secure-boot)).

Not covered yet: live images, the BIOS bootloader, files beyond the `initfs`, and protection against older signed versions.

## Boot Loader

### BIOS Boot

BIOS boot dates back to the [IBM PC](https://dosdays.co.uk/topics/pc_bios.php). Because of its long history, BIOS starts in 16-bit mode (real mode), and the boot loader has to load in several stages to reach the wider modes. The firmware executes the boot sector in the first sector of the main storage device, which is known as the stage 1 bootloader ([OSDev Wiki](https://wiki.osdev.org/Boot_Sequence#Master_Boot_Record)).

The stage 1 bootloader is written in assembly and can be found in `asm/x86-unknown-none/stage1.asm`. Its main task is to read the disk so that it can load stage 2 from another sector of the storage device. The stage 2 bootloader is also written in assembly. It makes the [BIOS functions](https://wiki.osdev.org/BIOS#BIOS_functions) available from protected mode, switches to protected mode (32-bit) or long mode (64-bit) and loads the boot loader written in Rust, called stage 3.

These three stages are combined in one executable that is written to the first megabyte of the storage device. The first code that runs in Rust is `start` in `src/os/bios/mod.rs`. From this point the bootloader follows the common boot process described below.

### UEFI Boot

The bootloader supports UEFI on x86-64, ARM64 and 64-bit RISC-V machines. UEFI starts in 64-bit mode, so the boot process does not need several stages. The firmware finds the EFI System Partition (ESP) on the storage device, then loads and executes a PE32+ UEFI program, typically located at `/EFI/BOOT/BOOTX64.efi` ([OSDev Wiki](https://wiki.osdev.org/UEFI#Bootable_UEFI_applications)).

The first code that runs is `main` in `src/os/uefi/mod.rs`. From this point the bootloader follows the common boot process described below.

### Common Boot Process

The bootloader initializes the memory map and the display mode. Both rely on firmware mechanisms that are not accessible after control passes to the kernel. Unless the boot is quiet, the bootloader shows a menu of display modes and waits for a choice.

The bootloader then finds the RedoxFS partition on the disk and loads the kernel and the `initfs` from it into memory. For a live disk, it loads the whole partition into memory first.

After the kernel and the `initfs` are loaded, the bootloader sets up paging for the kernel and the environment variables that it passes on, including the location of the RedoxFS partition. Then it maps the kernel to its expected virtual address and jumps to its entry function.

## Kernel

The kernel is a single ELF program. It performs architecture-specific initialization, starting in the `kstart` function of its architecture's `start.rs` under `src/arch/`, and then enters the `kmain` function in `src/startup/mod.rs`. The user-space bootstrap, a specially prepared executable that limits the parsing the kernel has to do, then sets up the `/scheme/initfs` scheme, and loads and executes the `init` program.

The kernel provides a small number of schemes of its own, such as `memory`, `pipe`, `event`, `irq`, `debug`, `sys` and `time` ([Schemes](./schemes.md#kernel-schemes)). Which schemes a process can open by name depends on its namespace ([Scheme Operation](./scheme-operation.md#namespaces)).

## Init

The init process has two stages, so that the storage drivers can be loaded in a modular and configurable way. The first stage runs from the initial RAM disk (`initfs`), a file that holds the bootstrap code and packed files and that the kernel makes available as `/scheme/initfs`. Both stages are described by unit files: a `.service` file starts a program, and a `.target` file groups units.

### Initfs Stage

The initfs stage loads the drivers and daemons that are needed to reach the root filesystem. Its units are in `init.initfs.d` of the `base` source:

1. Services that relibc needs (`00_runtime.target`)
    - `rtcd` sets the time from the real-time clock
    - `zerod` provides `/scheme/null` and `/scheme/zero`, and `randd` provides `/scheme/rand`
    - `logd` provides `/scheme/log`
    - `ipcd` and `udsd` provide inter-process communication and Unix domain sockets
    - `ramfs` provides an in-memory filesystem for logging
2. Graphics (`20_graphics.target`)
    - `inputd` multiplexes input and the virtual terminals, and provides `/scheme/input`
    - `vesad` handles the framebuffer that the bootloader set up
    - `fbbootlogd` shows the boot log, or the splash in a quiet boot
3. The live disk
    - `lived` provides the disk of a live image that the bootloader loaded into memory
4. Drivers (`40_drivers.target`)
    - `ps2d` handles PS/2 input
    - `hwd` manages the hardware description (ACPI or a device tree)
    - `pcid-spawner` starts the drivers for the PCI devices it finds, as `drivers/initfs.toml` lists them: `ahcid`, `ided`, `nvmed`, `virtio-blkd` and others
5. The root filesystem (`50_rootfs.service`)
    - `redoxfs` mounts the partition that the bootloader chose, named by `REDOXFS_UUID`, as `/scheme/file`

### Root Filesystem Stage

Init then continues with the files on the root filesystem, in `/usr/lib/init.d` and `/etc/init.d`. They start everything that is not needed to reach the storage device: the remaining PCI drivers, `ptyd`, the `sudo` daemon, the network stack (`netstack`) with `dhcpd`, the audio daemon and the consoles. The units of the `base` package are in `init.d` of its source, and a filesystem configuration adds its own files: `config/desktop-minimal.toml`, for example, writes `/usr/lib/init.d/20_orbital`, which starts Orbital.

## Login

After the init units have started the drivers and daemons, the system offers a login:

- On an image with a desktop, Orbital starts the graphical login, `orblogin`.
- A text console and the serial console show the prompt `ferrion login:`, where `ferrion` is the host name from `/etc/hostname`.

The login program prints `/etc/motd` and starts the user's login shell. In the x86_64 desktop image the login shell of `user` is Nushell, and `root` uses Ion ([Shells](./shell.md)). The accounts of an image built from the repository's configurations are development accounts ([Security](./security.md)).

The system has several virtual terminals. Hold the **Super** key and press a function key to switch: **Super+F1** shows the boot log, **Super+F2** the text console, and **Super+F3** the graphical session.

## Boot Process Documentation

- [Boot process documentation](https://wiki.osdev.org/Boot_Sequence)
