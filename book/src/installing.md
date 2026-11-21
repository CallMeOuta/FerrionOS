# Installing FerrionOS

> 📝 **Note:** FerrionOS inherits what this page describes from upstream and has not verified it yet.

FerrionOS includes upstream's installer (recipes `recipes/core/installer` and `recipes/gui/installer-gui`), which runs on FerrionOS itself or on Linux. **Please back up your system** before attempting to install.

What ends up on the disk depends on the option you choose:

- The **Desktop** and **Server** profiles of the GUI installer are compiled into the installer from its own `config/desktop.toml` and `config/server.toml`, and the installer fetches their packages, the bootloader included, from upstream's package server. These two profiles therefore install upstream's system, and not the FerrionOS image you built.
- The **Clone** profile, which the GUI installer offers only when it runs on FerrionOS itself, copies the running system. The TUI installer always does the same.

The installer is upstream's program, so its window and some of its texts carry upstream's name.

Note that the system cannot use a USB device for its root filesystem (the initfs has no USB storage driver), so it cannot be installed onto one. It can be installed from a live disk on a USB device.

## Starting the Installer from a FerrionOS Livedisk

The `desktop` config installs both installers (the packages `installer` and `installer-gui`), and `make live` builds the live disk, `redox-live.iso` ([Running FerrionOS on Real Hardware](./real-hardware.md)). After starting your FerrionOS *livedisk* system from a USB device or CD/DVD, log in as the user named `user` with an empty password, click the start icon at the left of the bar to open the app menu, then open the "System" category and click the "FerrionOS Installer" entry.

Or, if you want to launch it from the terminal, run the following command:

```sh
sudo redox_installer_gui
```

If your image was built from the `server` config, or you want to use the TUI interface, open a terminal window and type:

```sh
sudo redox_installer_tui
```

The GUI installer has more options than the TUI version, which always installs a copy of the running system onto a whole drive.

In the TUI version, if FerrionOS recognizes your device, it will prompt you to select a device to install on. Choose carefully, as it will erase all the data on that device. Note that if your device is not recognized, it may offer you the option to install on `disk/live` (the in-memory *livedisk*). Do not choose it, as it will crash the system.

The TUI version also prompts for a `redoxfs password`. This is for an encrypted filesystem. Leave the password empty and press Enter if you don't need filesystem encryption.

## Starting the Installer from Linux

The GUI installer can also run on Linux. No binary is distributed: compile and install it with `cargo`. The following command fetches the installer's source from upstream's repository and builds the `redox_installer_gui` program from it:

```sh
cargo install --git https://gitlab.redox-os.org/redox-os/installer redox_installer_gui --locked
```

A Linux build of the installer offers the Desktop and Server profiles only, so it installs upstream's system. To install the FerrionOS image that you built, boot its live disk ([Running FerrionOS on Real Hardware](./real-hardware.md)) and use the Clone profile there.

The GUI installer uses `libcosmic`; according to upstream, it only works under Wayland. After compiling it, launch it from a terminal:

```sh
redox_installer_gui
```

Depending on the target of the installation, you might want to run it with `sudo`.

### GUI Installer Options

The GUI installer is capable of many installation modes. This section explains each step option and its implications in detail:

#### 1. Where do you want to install FerrionOS?

This step asks for the general scenario of the installation:

- **Whole Drive**

  Use the whole storage device for the system. It will erase all data on that device. The next section is [#2](#2-choose-a-drive).

- **Specific Partition**

  Use a storage device partition for the system. It will erase all data in that partition, but leave every other partition intact. It is the option for a dual boot with your current Linux installation. The next section is [#3](#3-choose-a-main-partition).

- **Image File**

  Write to a new image file. This option creates a disk image file like the `harddrive.img` that a build produces. The next section is [#4](#4-configure-image-file).


#### 2. Choose a drive

This section lets you choose which storage device you want to install into. When chosen, the disk will be erased, then formatted as GPT with three partitions:

- First, a BIOS boot partition for boot using legacy BIOS.
- Second, a 1 MiB FAT filesystem for the UEFI bootloader.
- Last, a RedoxFS partition that occupies the rest of the disk.

This layout is not configurable. If you wish to do so, format your disk using `GParted` or a similar tool, then go back and choose **Specific Partition**.

*Never choose the disk that's currently active*.

#### 3. Choose a main partition

This section lets you choose which partition you want to use as the main RedoxFS partition of the system. This will erase the data for that partition but leave everything else untouched.

The installer is not capable of doing partitioning. If you want to do so, use another tool such as `GParted` and set up an empty partition for RedoxFS, then click **Reload** on this page after you're done partitioning.

Note that installing just for RedoxFS partition will not make it bootable; hence, there's a second option to choose where to install the bootloader:

- **Do not write**

  This will not write any bootloader and leave the system installed in that partition unbootable, because it needs its own bootloader and cannot be loaded by common bootloaders such as `GRUB`, `systemd-boot`, etc.

- **Dual-boot (systemd-boot)**

  If `/boot/efi` is detected to contain `systemd-boot`, this option will be visible. This option is not visible for Linux that boots using other than `systemd-boot` such as `GRUB`.

When chosen, it will write the UEFI bootloader to these paths:

- `/boot/efi/EFI/redox/*.efi` : The bootloader EFI file
- `/boot/efi/loader/entries/redox.conf` : The bootloader entry in `systemd-boot menu

In some Linux distributions, the `systemd-boot` menu is not visible, and to make it appear, the `timeout` option in `/boot/efi/loader/loader.conf` file. See [systemd-boot loader.conf man page](https://man.archlinux.org/man/loader.conf.5#OPTIONS) for more information. 

If you choose any partition, make sure it's not mounted. **Never choose the partition that's currently mounted**.

- **Any partition in /dev/\***

This option will format the partition with the FAT filesystem containing the UEFI bootloader. The bootloader does not have to be on the same disk as RedoxFS, as the bootloader will look to all drives that contain a RedoxFS partition with the help of UEFI services.

This option is meant for installation to an external storage device, or for chainloading to any bootloader that supports it, such as `GRUB`. In some machines, the bootloader can also be directly bootable from BIOS boot device menu or boot priority on firmware setup.

Note that this option only installs the UEFI bootloader. If your machine only supports booting via BIOS, then your only option is to go back and choose the **Whole Drive** option.

---

If you do not see the `Next` button, try to maximize the window.

#### 4. Configure Image File

This section contains the options for installing into an image file. It is particularly useful for running the system in a virtual machine, or for advanced installation methods later on.

- **Image Path**

This asks where to install the image file. In Linux, the `Browse` button is visible to open a file dialog where the file will be saved.

You can also specify a path to a directory, and the installer will just install the system's files in that directory, but upstream does not recommend this.

- **Image Size**

Specify the image file size, which will also be the size of the main RedoxFS partition.

- **Skip Partition?**

Checking this box will prevent the image file from being formatted as GPT device; rather, it will become a raw RedoxFS partition (written directly to storage device sectors). This is useful if you wanted only the RedoxFS format, which can then be used for `dd` into `/dev/partition` for example, or boot via the `-cdrom` option in `QEMU`.

- **Write Bootloader EFI**

This asks where to install the bootloader file. The EFI file is needed to boot the disk later on. It is a PE executable that is usually placed in `/EFI/BOOT/*.EFI` of a boot partition.


#### 5. Select System Profile

After entering options from either Section 2, 3, or 4, this is the last section before the actual install is performed.

- **Desktop**

  This profile installs upstream's desktop and server packages, which the installer downloads from upstream's package server. According to upstream, the installation usually takes around 3 minutes (200 seconds).

- **Server**

  This profile installs upstream's server packages only, downloaded the same way. According to upstream, its installation usually takes around a minute and a half (90 seconds).

- **Clone**

  This option is offered only when the installer runs on FerrionOS itself; a Linux build of the installer does not have it. It installs a copy of the running system and, on a live disk, clones the filesystem from the live disk's memory. It is the only profile that installs the FerrionOS image you built.

- **Install as Live Disk**

This option will make the bootloader install with live disk mode being chosen as the default.

- **Enable disk encryption password**

You can enter a password that encrypts the main RedoxFS partition. The whole partition is encrypted, and the bootloader asks for the password when the system boots.


#### 6. Installation Progress

The installation is then performed; according to upstream, it usually finishes within minutes. For the GUI installer, the progress is shown in the terminal where it was launched, and not in the GUI.

Once the installation completes, you can restart your computer to boot the installed system.
