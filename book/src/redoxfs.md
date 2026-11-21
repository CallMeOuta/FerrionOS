# RedoxFS

This is the default filesystem of FerrionOS, inspired by ZFS and adapted to a microkernel architecture.

Current features:

- Compatible with FerrionOS and Linux (FUSE)
- Copy-on-write
- Data/metadata checksums
- Transparent encryption
- Standard Unix file attributes
- File/directory size limit up to 193TiB (212TB)
- File/directory quantity limit up to 4 billion per 193TiB (2^32 - 1 = 4294967295)
- Disk encryption handled by the bootloader, which asks for the password and loads the kernel from the encrypted partition.
- MIT licensed

FerrionOS has not tested the encryption and does not claim it as a protection: see [Security](./security.md).

Being MIT licensed, RedoxFS can be bundled on GPL-licensed operating systems (Linux, for example).

## Tooling

RedoxFS tooling can be used to create, mount and edit contents of an `.img` file containing RedoxFS. It can be installed with:

```sh
cargo install redoxfs
```

If you found errors while installing it make sure to install the `libfuse` 3.x package for your Unix-like distribution.

A FerrionOS build also compiles these tools for the host, from the source of the `redoxfs` recipe (`recipes/core/redoxfs`), and puts them in `build/fstools/bin/`.

### Create a disk

You can create an empty, non-bootable RedoxFS by allocating an empty file with `fallocate` then run `redoxfs-mkfs` to initialize the whole image as `RedoxFS`.

```sh
fallocate -l 1G ferrion.img
```

```sh
redoxfs-mkfs ferrion.img
```

To create an encrypted disk use the `--encrypt` option, it will ask the password through a masked prompt:

```sh
redoxfs-mkfs --encrypt ferrion.img
```

To create a disk with contents from an existing directory use `redoxfs-ar` (currently the `redoxfs-ar` tool overwrites existing content and have no  `--encrypt` option).

```sh
redoxfs-ar ferrion.img ./sysroot
```

### Create a bootable disk

The second option of `redoxfs-mkfs` and third option of `redoxfs-ar` tools accepts a file that contains a raw image to be written as reserved disk space. This is meant to be a bootloader. FerrionOS is booted by the bootloader that the `bootloader` recipe builds (`recipes/core/bootloader`), which carries FerrionOS's patches.

Build the bootloader with the cookbook, from the FerrionOS directory:

```sh
make r.bootloader
```

For an x86_64 build, the BIOS bootloader is then at `recipes/core/bootloader/target/x86_64-unknown-redox/stage/usr/lib/boot/bootloader.bios`. With it, you can create a new bootable disk using either `redoxfs-mkfs` or `redoxfs-ar`:

```sh
redoxfs-mkfs ferrion.img recipes/core/bootloader/target/x86_64-unknown-redox/stage/usr/lib/boot/bootloader.bios
```

```sh
redoxfs-ar ferrion.img ./sysroot recipes/core/bootloader/target/x86_64-unknown-redox/stage/usr/lib/boot/bootloader.bios
```

You can also convert a non-bootable disk into bootable using `redoxfs-clone`:

```sh
redoxfs-clone ferrion.img ferrion-bootable.img recipes/core/bootloader/target/x86_64-unknown-redox/stage/usr/lib/boot/bootloader.bios
```

It is not possible to create a UEFI-bootable disk this way: that takes a GPT-partitioned disk with an EFI partition, which only the installer creates (`redox_installer`, recipe `recipes/core/installer`). A dual boot option in the same disk is also impossible if booting from BIOS firmware.

Note that the image needs the files `usr/lib/boot/kernel` and `usr/lib/boot/initfs` to be bootable, because the bootloader loads these two. The [build system](./podman-build.md) builds them with the `kernel` and `base` recipes (`recipes/core/kernel` and `recipes/core/base`): after `make r.kernel,base`, each file is in its recipe's `target/x86_64-unknown-redox/stage/usr/lib/boot/` directory.

### Mount a disk

To mount the disk run the `redoxfs [image] [directory]` command, for example:

```sh
mkdir ./ferrion-img
```

```sh
redoxfs ferrion.img ./ferrion-img
```

It will mount the disk using FUSE underneath.

The difference is that `redoxfs-ar` overwrites existing disk content. Mounting a disk through FUSE is the only way to update existing content in the RedoxFS-formatted image.

### Unmount the disk

Unmount the disk using FUSE unmounting tool:

```sh
fusermount3 -u ./ferrion-img
```

### Extend the disk

> ⚠️ **Warning:** Experimental, please backup first.

To extend an existing disk, you need to extend the file before:

```sh
truncate -s 2GB ferrion.img
```

Then you can run the `redoxfs-resize [disk] [size]` command. The `[size]` option can be `max` (the default), `min`, or a fixed size defined in the [parse-size](https://crates.io/crates/parse-size) library.

```sh
redoxfs-resize ./ferrion.img
```

```
minimum size: 438.75 MB (418.42 MiB)
maximum size: 2.00 GB (1.86 GiB)
new size: 2.00 GB (1.86 GiB)
growing by 1559135232
redoxfs-resize: resized filesystem on ferrion.img
	uuid: febb081e-06ff-4786-a878-f1bb031cdf97
	size: 2.00 GB (1.86 GiB)
	used: 438.65 MB (418.33 MiB)
	free: 1.56 GB (1.45 GiB)
```

### Shrink the disk

> ⚠️ **Warning:** Experimental, please backup first.

To shrink an existing disk you can use the `redoxfs-resize` tool:

```sh
redoxfs-resize ./ferrion.img min
```

```
minimum size: 438.77 MB (418.44 MiB)
maximum size: 2.00 GB (1.86 GiB)
new size: 438.77 MB (418.44 MiB)
shrinking by 1559135232
redoxfs-resize: resized filesystem on ferrion.img
	uuid: febb081e-06ff-4786-a878-f1bb031cdf97
	size: 438.77 MB (418.44 MiB)
	used: 438.64 MB (418.32 MiB)
	free: 122.88 kB (120 KiB)
```

You can use the value from "shrinking by ..." to accurately tell how many bytes can be shrinked:

```sh
truncate -s -1559135232 ferrion.img
```

### Working with Linux Partition

> ⚠️ **Warning:** Experimental and dangerous; always double-check your command before executing it.

On Linux, RedoxFS supports formatting and mounting directly from `/dev` partition. The [GUI installer](./installing.md) can also format and install into a whole disk or a specific partition. This guide only covers the manual process using `redoxfs` CLI tools.

### Creating an empty partition for RedoxFS

First thing you need to do is that you need to create an empty space for RedoxFS partition to fit in. Use [GParted](https://gparted.org/), [GNOME Disks](https://apps.gnome.org/DiskUtility/) or [KDE Partition Manager](https://apps.kde.org/partitionmanager/) to create an empty partition, then identify the partition using `lsblk`

```sh
lsblk
```

```
NAME          MAJ:MIN RM  SIZE RO TYPE  MOUNTPOINTS
zram0         251:0    0  7.7G  0 disk  [SWAP]
vda           253:0    0  2.7G  0 disk
vdb           253:16   0  200G  0 disk
├─vdb1        253:17   0 1022M  0 part  /boot/efi
├─vdb2        253:18   0  3.9G  0 part
└─vdb3        253:19   0  191G  0 part  /
```

For example, we want to format a partition with RedoxFS in `/dev/vdb2`. Format it using redoxfs-mkfs:

```sh
sudo $(which redoxfs-mkfs) /dev/vdb2
```

After the format is complete, the partition managers should detect the new partition type as `Unknown`.

### Mounting RedoxFS partition

Before mounting, you can create an empty directory:

```sh
mkdir myfs
```

There are two ways to mount the RedoxFS partition to the directory based on your preference:

#### Rootless mount

A rootless mount grants full access to a partition to a specific user. It can be configured using `setfacl`:

```sh
sudo setfacl -m u:$USER:rw /dev/vdb2
```

Then mount the disk without sudo:

```sh
redoxfs /dev/vdb2 ./myfs
```

This way you can gain full access to the files partition, ignoring standard file permissions (i.e., allow writing files as a root user), but the mounted device is not shared with other users in your operating system.

To unmount from the partition, use `fusermount3`:

```sh
fusermount3 -u ./myfs
```

#### Rootful mount

A rootful mount allows sharing the mounted device with other users, and standard file permissions apply. It can be mounted using `sudo`:

```sh
sudo REDOXFS_FUSE_SHARE=1 $(which redoxfs) /dev/vdb2 ./myfs
```

The environment variable `REDOXFS_FUSE_SHARE=1` is used to share a FUSE mount from root with other non-root users. The FUSE options applied are `default_permission` and `allow_other`. See [FUSE man page](https://man.archlinux.org/man/mount.fuse.8.en#default_permissions) for details.

To unmount from the shared partition, use a similar command with sudo.

```sh
sudo fusermount3 -u ./myfs
```
