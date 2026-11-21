# Native Build

This page explains how to build FerrionOS with the tools of your own system, without Podman. A native build can be useful where Podman is not available or poorly supported, or in an isolated environment such as a CI job or Nix.

> ⚠️ **Warning:** FerrionOS has not verified the native build: it builds and checks its images with the [Podman build](./podman-build.md). A build outside Podman depends on the versions of the tools on your system and needs experience with toolchains. Use the Podman build unless you have a reason not to.

If you meet a problem that is hard to explain, try the [Podman build](./podman-build.md) to see whether the problem occurs there as well.

## Preparing the Build

### Get the Source and Install the Tools

Clone the repository as [Building FerrionOS](./podman-build.md#get-the-source) describes, then run the bootstrap script for the native build from the FerrionOS directory:

```sh
./native_bootstrap.sh -d
```

The script installs Rust through rustup and the build tools that the recipes need, with the package manager of your system. It has package lists for Debian and Ubuntu, Fedora, Arch Linux, openSUSE, Gentoo, Solus, FreeBSD and macOS, inherited from upstream. `-y` answers its questions with the default.

> ⚠️ **Warning:** Always pass `-d`. Without it, the script clones the upstream build system into a new directory and deletes itself.

If the script does not know your system, [Advanced Native Build](./advanced-build.md) lists the tools to install by hand.

Then load Rust into the current shell:

```sh
source ~/.cargo/env
```

### Set the Configuration Values

The build system reads its settings from the `.config` file in the FerrionOS directory. [Configuration Settings](./configuration-settings.md) describes them. For a native build, set:

- `PODMAN_BUILD?=0` to turn the Podman build off
- `ARCH?=x86_64` and `CONFIG_NAME?=desktop`, or the architecture and configuration you want
- `PREFIX_BINARY?=0` if the prebuilt cross toolchain does not run on your system ([Prefix](./advanced-build.md#prefix)); the toolchain is then built from source, which adds hours to the first build
- `PREFIX_USE_UPSTREAM_RUST_COMPILER?=1` together with `PREFIX_BINARY?=0`, to download the Rust compiler instead of building it ([Prefix: Rust](./advanced-build.md#prefix-rust))

## Compiling FerrionOS

From the FerrionOS directory:

```sh
CI=1 COOKBOOK_LOGS=true make all
```

This builds `build/x86_64/desktop/harddrive.img`, which `make qemu` starts in a virtual machine. [Building FerrionOS](./podman-build.md#build-the-image) explains the command, and the [`make all` section of Build Phases](./build-phases.md) what it does.

> 💡 **Tip:** The image is assembled through [FUSE](https://github.com/libfuse/libfuse). If the build fails while it makes the image, check that FUSE is installed and that your user may use it.
