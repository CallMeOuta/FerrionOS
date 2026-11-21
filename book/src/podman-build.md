# Building FerrionOS

This page builds a FerrionOS image from the repository and starts it in a virtual machine. It describes the build that FerrionOS uses and has verified: an x86_64 Linux host, the Podman build, and the x86_64 `desktop` image. The build system also targets [ARM64](./aarch64.md) (`aarch64`) and RISC-V (`riscv64gc`), and it can build without Podman ([Native Build](./building-redox.md)); FerrionOS has not verified those paths.

- [What You Need](#what-you-need)
- [Get the Source](#get-the-source)
- [Install the Host Tools](#install-the-host-tools)
- [Configure the Build](#configure-the-build)
- [Build the Image](#build-the-image)
- [Run in a Virtual Machine](#run-in-a-virtual-machine)
- [After the First Build](#after-the-first-build)
- [Update](#update)

## What You Need

- A Linux host with `git`.
- 50 GB of storage or more. A first build makes the build container, the cross toolchain and every package of the image.
- A network connection. The build fetches the sources of the system's components, most of them from upstream's repositories.
- A terminal. A Podman build needs one: start `make` from a terminal, not from a job without one.

The build runs in a [Podman](https://docs.podman.io/en/latest/Introduction.html) container, so that it does not depend on the tools of your distribution. The container is a Debian image with Rust and the other build tools. The host still needs a few tools of its own: it assembles the disk image through FUSE, signs it for [verified boot](./boot-process.md#verified-boot) with OpenSSL 3 and Python 3, and runs the emulator.

## Get the Source

```sh
git clone https://github.com/CallMeOuta/FerrionOS.git
```

```sh
cd FerrionOS
```

The branch is `master`. This book calls the directory of the clone the FerrionOS directory, and every `make` command runs there.

## Install the Host Tools

```sh
./podman_bootstrap.sh -d
```

The script offers to install Rust through rustup, and installs Podman, FUSE, make and QEMU with the package manager of your distribution. It asks for your password through `sudo`. It knows the package managers of Debian and Ubuntu, Fedora, Arch Linux, openSUSE, Gentoo, Solus, FreeBSD and macOS; [Advanced Podman Build](./advanced-podman-build.md) lists the packages for a host it does not know.

> ⚠️ **Warning:** Always pass `-d`. Without it, the script clones the upstream build system into a new directory and deletes itself.

If the script installed Rust, load it into the current shell:

```sh
source ~/.cargo/env
```

## Configure the Build

A build needs no configuration: the defaults are the architecture of the host and the `desktop` configuration.

Settings go into a file named `.config` in the FerrionOS directory, in make syntax, or on the `make` command line. [Configuration Settings](./configuration-settings.md) lists them. The ones a first build may need:

| Setting | Effect |
|---|---|
| `CONFIG_NAME?=minimal` | Builds a small image without a graphical interface instead of `desktop`. `server` is a larger one without a graphical interface. |
| `REPO_BINARY?=1` | Downloads prebuilt packages instead of building every package from source. This makes a first image much faster. |
| `ARCH?=x86_64` | The architecture to build for. |

With `REPO_BINARY=1`, the packages come from upstream's package server: they are upstream's builds of upstream's sources. Four components are built from source in every case, because FerrionOS carries patches for them: `base`, `bootloader`, `kernel` and `userutils`. Such an image is good for trying FerrionOS and for testing a configuration; it does not contain a change you make to another recipe.

## Build the Image

```sh
CI=1 COOKBOOK_LOGS=true make all
```

- `CI=1` turns off the cookbook's terminal interface, which waits at a prompt when a recipe fails.
- `COOKBOOK_LOGS=true` keeps the output of each recipe in `build/logs/<target>/<recipe>.log`, where `<target>` is the Rust target, such as `x86_64-unknown-redox`.

The first run builds the container, compiles the cookbook (the build tool in `src/`), downloads the prebuilt cross toolchain from upstream's server, fetches and cooks every package of the configuration, and assembles the image. It takes a long time. To keep the whole output, add `2>&1 | tee build.log`.

The result is `build/x86_64/desktop/harddrive.img`, or in general `build/<ARCH>/<CONFIG_NAME>/harddrive.img`.

When a recipe fails, read its log in `build/logs/`, and see [Troubleshooting the Build](./troubleshooting.md).

## Run in a Virtual Machine

```sh
make qemu
```

QEMU opens a window. The screen stays black for a moment, shows a splash, and then the graphical login. Log in as `user`, with an empty password. [Using the Emulation](./running-vm.md#using-the-emulation) describes the keys and the accounts, and [Trying Out FerrionOS](./trying-out-redox.md) has things to try.

To run without a window, with the serial console in the terminal:

```sh
make qemu gpu=no
```

If the virtual machine does not start, try `make qemu kvm=no`, which emulates the CPU instead of using KVM. [Running FerrionOS in a Virtual Machine](./running-vm.md) describes the other options and the UEFI firmware that `make qemu` looks for.

## After the First Build

| Goal | Command |
|---|---|
| Cook again what changed, then make a new image | `make rebuild` |
| Make a new image from the packages that are already cooked | `make image` |
| Cook one recipe and push it into the existing image | `make rp.<recipe>` |
| Build another configuration beside the first | `make all CONFIG_NAME=<name>` |

`make all` and `make qemu` do not notice most changes after the first build: use `make rebuild`. An image takes one writer at a time, so stop QEMU before a command that changes the image.

## Update

```sh
make pull
```

```sh
make rebuild
```

`make pull` updates the checkout with `git pull`, and `make rebuild` cooks what changed and makes a new image.

## Next Steps

- [Build System Reference](./build-system-reference.md): how the build system is organized, and its commands.
- [Configuration Settings](./configuration-settings.md): every setting, and how to make your own filesystem configuration.
- [Advanced Podman Build](./advanced-podman-build.md): how the Podman build works.
- [Testing](./testing-practices.md): how to check an image.
- [VirtIO-FS Shared Directories](./virtio-fs.md) and [Cloud Hypervisor](./cloud-hypervisor.md): further ways to run an image, which FerrionOS has not verified.
- [Participating](./contributing.md): how to report a build that fails.
