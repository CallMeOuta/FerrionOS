# VirtIO-FS Shared Directories

> 📝 **Note:** FerrionOS inherits what this page describes from upstream and has not verified it yet.

VirtIO-FS lets a FerrionOS virtual machine access directories of the host system, without mounting the FerrionOS image and without a network connection. The build system can set it up with QEMU and with [Cloud Hypervisor](./cloud-hypervisor.md).

- [Overview](#overview)
- [Requirements](#requirements)
- [Sharing Directories With The Build System](#sharing-directories-with-the-build-system)
  - [Share Several Directories](#share-several-directories)
  - [Read-only Shares](#read-only-shares)
  - [Make The Share Permanent](#make-the-share-permanent)
- [Using The Shared Directories In FerrionOS](#using-the-shared-directories-in-ferrionos)
- [Sharing Directories Without The Build System](#sharing-directories-without-the-build-system)
- [Driver And Schemes](#driver-and-schemes)
- [Test Suite](#test-suite)
- [Troubleshooting](#troubleshooting)

## Overview

VirtIO-FS has two sides:

- On the host, one [virtiofsd](https://gitlab.com/virtio-fs/virtiofsd) daemon serves each shared directory and talks to the virtual machine monitor (QEMU or Cloud Hypervisor) through a Unix socket (vhost-user).
- In FerrionOS, the `virtio-fsd` driver exposes each shared directory as a [scheme](./schemes.md) named `virtio-fs.<tag>`, where the `<tag>` is the name that you gave to the share. For example, `/scheme/virtio-fs.host`.

The `make qemu` and `make ch` commands can do all the host-side work for you, you only need to say which directories you want to share.

This is useful to edit files on your host with your usual tools and use them in FerrionOS, or to move many or large files in and out of FerrionOS, as an alternative to the [Insert Files in the Image](./coding-and-building.md#insert-files-in-the-image) method.

> **Warning:** FerrionOS can read, change and delete everything inside of a shared directory. Only share directories that you are fine with losing, or share them as [read-only](#read-only-shares) to protect them from changes. A read-only share can still be read, so don't share directories that contain sensitive data.

## Requirements

- A Linux host, because the host side uses vhost-user and shared memory.
- For QEMU, a QEMU with the `vhost-user-fs-pci` device. Most distributions include it, you can verify with the following command:

```sh
qemu-system-x86_64 -device help | grep vhost-user-fs
```

- For Cloud Hypervisor, nothing else is needed, the `--fs` device is always available (see [Cloud Hypervisor](./cloud-hypervisor.md)).

- The Rust implementation of `virtiofsd`. The bootstrap scripts (see [Building FerrionOS](./podman-build.md)) do not install it.

    Install the package of your distribution:

    ```sh
    sudo apt-get install virtiofsd
    ```
    
    ```sh
    sudo dnf install virtiofsd
    ```

    ```sh
    sudo pacman -S virtiofsd
    ```

    The build system uses the first `virtiofsd` that it finds: the `VIRTIOFSD` variable, then `virtiofsd` in your `PATH`, then `/usr/libexec/virtiofsd` and `/usr/lib/qemu/virtiofsd`. To use another binary, set `VIRTIOFSD=/path/to/virtiofsd`.

    When it finds none, `make qemu` and `make ch` first download a static `virtiofsd` into the `build/virtiofsd/<host arch>/` directory, and `make virtiofsd-fetch` only does that download. This happens on x86_64 Linux hosts, on other hosts `make` stops with an error. The download is the CI build of the `main` branch of virtiofsd: it is not pinned to a release and not checked against a checksum, as the `mk/virtiofs.mk` file notes. Prefer the package of your distribution.

- A FerrionOS image that includes the `virtio-fsd` driver. It's part of the `base` recipe. If FerrionOS doesn't show your share, rebuild the image with the `make rebuild` command.

## Sharing Directories With The Build System

Pass the directory to share in the `share` option of `make qemu` (or `make ch` for Cloud Hypervisor, which accepts the same option):

```sh
make qemu share=$HOME/shared
```

The directory must already exist. The share has the tag `host`, so it's available in FerrionOS at `/scheme/virtio-fs.host`.

> **Note:** Use `$HOME` instead of `~`, some shells don't expand `~` after `share=`.

Like the other QEMU options, it can be combined with them:

```sh
make qemu gpu=no share=$HOME/shared
```

When `share` is set, `make qemu` and `make ch` do the following:

- They start one `virtiofsd` for each shared directory. The sockets and logs are in the `build/$ARCH/$CONFIG_NAME` directory (`virtiofsd-1.sock`, `virtiofsd-1.log`, and so on).
- They start the virtual machine with one VirtIO-FS device for each shared directory, and with guest memory that can be shared with `virtiofsd`. QEMU uses `vhost-user-fs-pci` devices and takes the amount of memory from `QEMU_MEM`, Cloud Hypervisor uses `--fs` devices and takes it from `CH_MEM`.
- When the virtual machine exits, or you interrupt `make`, they stop all the `virtiofsd` processes and remove the sockets.

The `virtiofsd` daemons run as your user, without needing `root`. The `root` user of FerrionOS is mapped to your user on the host, thus the files that you create in FerrionOS are owned by you on the host.

### Share Several Directories

To share more than one directory, give each one a tag, separated by spaces, using the `tag=directory` format:

```sh
make qemu share="docs=$HOME/docs src=$HOME/code"
```

In FerrionOS, they are available at `/scheme/virtio-fs.docs` and `/scheme/virtio-fs.src`.

The word `redox` shares the FerrionOS build system, the directory you run `make` in, with the tag `redox`:

```sh
make qemu share=redox
```

It is available at `/scheme/virtio-fs.redox`. Writing `redox=/some/path` does not do this: that is an ordinary share of `/some/path` with the tag `redox`.

> **Warning:** `share=redox` gives FerrionOS read and write access to the whole build system, including the `build/$ARCH/$CONFIG_NAME/harddrive.img` file that FerrionOS is running from, the `.git` directory and the `.config` file. Changing these files from FerrionOS can corrupt the running system or your checkout, and running `make` on the host at the same time as FerrionOS writes to the tree can cause the same problem. Share a subdirectory (for example `recipes=$PWD/recipes`) if you only need part of the tree, or share it read-only with `share=redox:ro` (see [Read-only Shares](#read-only-shares)).

The rules are:

- A directory without a tag is only accepted when it's the only share, and gets the tag `host` (or the value of `share_tag`). The word `redox` is the exception above, including when it is one entry in a list.
- The tags can have letters, digits, `.`, `_` and `-`, up to 36 characters, and can't be repeated.
- Directories with spaces in the name aren't supported, because the list is split on spaces. Create a symbolic link with a name without spaces and share it.

These mistakes are reported before anything is started. Nothing is left running after the error.

### Read-only Shares

Add `:ro` to the end of a share to stop FerrionOS from changing anything in it:

```sh
make qemu share=$HOME/shared:ro
```

```sh
make qemu share="docs=$HOME/docs:ro src=$HOME/code"
```

```sh
make qemu share=redox:ro
```

FerrionOS can still read the files, but every attempt to create, change or delete something fails. In the second example only `docs` is read-only, `src` can be changed as usual.

### Make The Share Permanent

To always share a directory, add the options to your `.config` file (see [Configuration Settings](./configuration-settings.md)):

```
share?=$(HOME)/shared
```

The options, defined in `mk/virtiofs.mk`, are:

| **Option**       | **Default**  | **Description** |
|:-----------------|:-------------|:----------------|
| `share`          | (none)       | A directory, a list of `tag=directory` entries separated by spaces, or the word `redox` to share the build system. Add `:ro` to an entry to make it read-only. Without it, no directory is shared |
| `share_tag`      | `host`       | The tag of a single directory given without a tag |
| `share_sockdir`  | `$(BUILD)`   | The directory where the sockets and logs of the `virtiofsd` daemons are created |

## Using The Shared Directories In FerrionOS

After the login, list the content of the share:

```sh
ls /scheme/virtio-fs.host
```

You can use it like any other directory, with the same commands:

```sh
cp /scheme/virtio-fs.host/notes.txt ~/notes.txt
```

```sh
cp ~/result.log /scheme/virtio-fs.host/
```

```sh
cd /scheme/virtio-fs.host
```

The driver supports the usual file operations: create, read, write, truncate, rename, remove, directories, hard links, symbolic links, permissions, owners, timestamps, file locks (`fcntl`) and filesystem statistics.

Some limitations:

- Mapping the content of a file into memory (`mmap`) is not supported yet, programs that need it can't work from a share. Copy the file to the FerrionOS filesystem first.
- The `cd` command doesn't follow symbolic links yet. If you create a symbolic link to a share in the FerrionOS filesystem, `cd` on it fails with the "Cross-device link" error. Use the scheme path (`/scheme/virtio-fs.<tag>`) directly.

## Sharing Directories Without The Build System

If you run QEMU or Cloud Hypervisor by yourself (see [Running FerrionOS in a Virtual Machine](./running-vm.md)), start one `virtiofsd` for each directory:

```sh
/usr/libexec/virtiofsd \
  --socket-path /tmp/ferrion-share.sock \
  --shared-dir $HOME/shared \
  --sandbox none \
  --translate-uid map:0:$(id -u):1 \
  --translate-gid map:0:$(id -g):1
```

Add `--readonly` to make the share read-only.

Then add the following options to your QEMU command. The size of the memory backend must be the same as the `-m` option of the VM, and a second share needs its own `-chardev` and `-device` pair with another `id` and `tag`:

```sh
-object memory-backend-memfd,id=mem,size=2048M,share=on \
-numa node,memdev=mem \
-chardev socket,id=vfs1,path=/tmp/ferrion-share.sock \
-device vhost-user-fs-pci,queue-size=1024,chardev=vfs1,tag=host
```

> **Tip:** if your system doesn't have `memory-backend-memfd`, use `-object memory-backend-file,id=mem,size=2048M,mem-path=/dev/shm,share=on` instead.

For Cloud Hypervisor, the guest memory must be shared and each directory needs its own `--fs` device:

```sh
--memory size=2048M,shared=on \
--fs tag=host,socket=/tmp/ferrion-share.sock
```

The image must include the `virtio-fsd` driver (see [Requirements](#requirements)).

## Driver And Schemes

The driver is the `virtio-fsd` daemon, in the `drivers/storage/virtio-fsd` directory of the `base` source (recipe `recipes/core/base`).

It's started by the `pcid-spawner` system service, which reads the file `/usr/lib/pcid.d/virtio-fsd.toml` when the PCI device of the VirtIO file system is detected. QEMU and Cloud Hypervisor present the device with different PCI classes, so there is one entry for each:

```toml
# QEMU presents virtio-fs as mass storage, class 0x01 (subclass 0x80).
[[drivers]]
name = "virtio-fs"
class = 0x01
vendor = 0x1AF4
device = 0x105A
command = ["virtio-fsd"]

# Cloud Hypervisor's non-transitional virtio-fs is class 0xFF. The device id is the same.
[[drivers]]
name = "virtio-fs"
class = 0xFF
vendor = 0x1AF4
device = 0x105A
command = ["virtio-fsd"]
```

Each shared directory is a different PCI function, so each one gets its own `virtio-fsd` process and its own scheme: `virtio-fs.<tag>`, where the `<tag>` is the one given to the virtual machine monitor. The driver also serves some debug information, such as the number of evicted inodes (`lru_evicted`), in a second scheme named `virtio-fs-debug.<tag>`. The driver talks to `virtiofsd` using the [FUSE](https://en.wikipedia.org/wiki/Filesystem_in_Userspace) protocol over VirtIO queues.

## Test Suite

The driver has a test suite, the `virtio-fsd-selftest` program, in the `selftests/virtio-fsd` directory of the `base` source.

To install and run it in your own FerrionOS image, build and push or add the recipe to your filesystem configuration (see [Configuration Settings](./configuration-settings.md#filesystem-customization)):

- Build and push

```sh
make rp.virtio-fsd-selftest
```

Or permanently install the recipe to your FerrionOS image:

```toml
[packages]
virtio-fsd-selftest = {}
```

Create the files that the tests use on the host, in the directory that you will share. It's the `fixtures` directory, and it's deleted and recreated each time you run the script. The script is in the `base` source, which `make f.base` fetches, run it from the FerrionOS directory:

```sh
./recipes/core/base/source/selftests/virtio-fsd/scripts/gen-fixtures.sh $HOME/shared
```

Then start FerrionOS with `make qemu share=$HOME/shared` (or `make ch share=$HOME/shared`) and run the program in FerrionOS with the tag of the share:

```sh
virtio-fsd-selftest host --skip-perf
```

Without the `fixtures` directory, the tests that need it are skipped. To run the tests without FerrionOS running in a window, the Makefile of the `base` source has the `test-virtio-fs` target (`make test-virtio-fs` in the `recipes/core/base/source` directory), that uses `redoxer`.

## Troubleshooting

- `virtiofs: virtiofsd not found` or `virtiofsd: no prebuilt virtiofsd for this ... host`: install `virtiofsd` (see [Requirements](#requirements)) or set `VIRTIOFSD=/path/to/virtiofsd`.
- `virtiofs: tag ...` or `virtiofs: share ...` errors: the options of `share` have a mistake, see the rules in [Share Several Directories](#share-several-directories). The `virtiofs: share '<tag>': <directory> is not a directory` error means that the directory doesn't exist, create it first.
- `virtiofs: ... is in use by another virtiofsd`: another virtual machine is using the same socket. Stop it first, or use another `share_sockdir`.
- QEMU or Cloud Hypervisor says that it failed to connect to the socket, or the share disappears while FerrionOS is running: `virtiofsd` stopped. Read its log in `build/$ARCH/$CONFIG_NAME/virtiofsd-<number>.log`.
- `/scheme/virtio-fs.<tag>` doesn't exist in FerrionOS: check that your image has the driver (see [Requirements](#requirements)), and that the tag is the one that you used in the `share` option.
- `virtiofsd` processes after an interrupted `make qemu` or `make ch`: they are normally stopped by the build system, if some are left, stop them with `pkill virtiofsd`.
