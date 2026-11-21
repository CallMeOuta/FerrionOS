# Advanced Native Build

In this section, we provide the gory details that may be handy to know if you are working on or developing for **FerrionOS**.

Before reading through this section, make sure you have already read:
- [Podman Build](./podman-build.md)
- [Native Build](./building-redox.md)
- [Build System Reference](./build-system-reference.md)

## Setup Your Environment

Advanced users may accomplish the same as the [**native_bootstrap.sh**](https://github.com/CallMeOuta/FerrionOS/blob/master/native_bootstrap.sh) script with the following steps:

- [Clone The Repository](#clone-the-repository)
- [Install The Necessary Packages](#install-the-necessary-packages-and-emulator)
- [Install Rust](#install-rust-stable-and-nightly)
- [Adjust Your Configuration Settings](./configuration-settings.md): a native build needs `PODMAN_BUILD?=0` in the `.config` file, because a Podman build is the default
- Build the system

## Clone The Repository

- Clone the repository into your home directory and go to its directory

```sh
cd ~
```

```sh
git clone https://github.com/CallMeOuta/FerrionOS.git
```

```sh
cd FerrionOS
```

The examples on this page call this checkout `~/FerrionOS`. The **native_bootstrap.sh** and **podman_bootstrap.sh** scripts are run from inside it, and only with the `-d` option, which installs the various packages needed for building FerrionOS and stops. Without `-d` they clone upstream's build system into a new `redox` directory and delete themselves.

## Install The Necessary Packages and Emulator

The **native_bootstrap.sh** script installs the packages and the emulator when you run it in the FerrionOS directory with the `-d` option:

```sh
cd ~/FerrionOS
```

```sh
./native_bootstrap.sh -d
```

If you can't use the `native_bootstrap.sh` script, you need to install at least:

- Essential compilers: GCC, Rust and Nasm
- GNU search, text, and build tools: `find`, `grep`, `make`, `patch`, `pkg-config`, and `sed`
- Other build tools: `autotools`, `cmake`, `meson`, `perl` and `python3`
- Other file tooling: `curl`, `git`, `rsync`, `tar` and `wget`
- Rust tooling: `cbindgen` and `just`
- FUSE (to build an image) and QEMU (to run the image)

Additional programs or libraries might be needed to build more packages. You can attempt to install the necessary packages below.

> ⚠️ **Warning:** The following commands are inherited from upstream. FerrionOS has not verified them, and they may be outdated.

> 📝 **Note:** Always use the latest stable version of the Linux or Unix-like distribution of your choice, as any outdated tools might result in unexpected build errors. The environment that the build system itself defines is the Podman container, which is Debian 13 (Trixie) (`podman/redox-base-containerfile`).

### Pop!_OS/Ubuntu/Debian Users

Install the build system dependencies:

```sh
sudo apt-get install ant autoconf automake autopoint bison \
build-essential clang cmake curl dos2unix doxygen file flex \
fuse3 g++ genisoimage git gperf intltool libexpat-dev libfuse-dev \
libgmp-dev libhtml-parser-perl libjpeg-dev libmpfr-dev libpng-dev \
libsdl1.2-dev libsdl2-ttf-dev libtool llvm lua5.4 m4 make meson nasm \
ninja-build patch perl pkg-config po4a protobuf-compiler python3 \
python3-mako rsync scons texinfo unzip wget xdg-utils xxd zip zstd
```

- If you want to use QEMU, run:

```sh
sudo apt-get install qemu-system-x86 qemu-kvm qemu-system-arm qemu-system-riscv
```

- If you want to use VirtualBox, run:

```sh
sudo apt-get install virtualbox
```

### Fedora Users

Install the build system dependencies:

```sh
sudo dnf install autoconf vim bison flex genisoimage gperf \
expat expat-devel fuse-devel fuse3-devel gmp-devel \
libpng-devel perl perl-HTML-Parser libtool libjpeg-turbo-devel
SDL2_ttf-devel sdl12-compat-devel m4 nasm po4a syslinux \
texinfo ninja-build meson waf python3-mako make gcc gcc-c++ \
openssl patch automake perl-Pod-Html perl-FindBin gperf curl \
gettext-devel perl-Pod-Xhtml pkgconf-pkg-config cmake llvm zip \
unzip lua luajit make clang doxygen ant protobuf-compiler zstd
```

- If you want to use QEMU, run:

```sh
sudo dnf install qemu-system-x86 qemu-kvm qemu-system-arm qemu-system-riscv
```

- If you want to use VirtualBox, install from the VirtualBox [Linux Downloads](https://www.virtualbox.org/wiki/Linux_Downloads) page.

### Arch Linux Users

Install the build system dependencies:

```sh
pacman -S --needed cmake fuse git gperf perl-html-parser nasm \
wget texinfo bison flex po4a autoconf curl file patch automake \
scons waf expat gmp libtool libpng libjpeg-turbo sdl12-compat \
m4 pkgconf po4a syslinux meson python python-mako make xdg-utils \
zip unzip llvm clang perl doxygen lua ant protobuf
```

- If you want to use QEMU, run:

```sh
sudo pacman -S qemu-system-x86 qemu-system-arm qemu-system-riscv
```

- If you want to use VirtualBox, run:

```sh
sudo pacman -S virtualbox
```

### OpenSUSE Users

Install the build system dependencies:

```sh
sudo zypper install gcc gcc-c++ nasm make fuse-devel \
cmake openssl automake gettext-tools libtool po4a patch flex gperf autoconf \
bison curl wget file libexpat-devel gmp-devel libpng16-devel libjpeg8-devel \
perl perl-HTML-Parser m4 patch scons pkgconf syslinux-utils ninja meson python-Mako \
xdg-utils zip unzip llvm clang doxygen lua54 ant protobuf
```

- If you want to use QEMU, run:

```sh
sudo zypper install qemu-x86 qemu-kvm
```

### Gentoo Users

Install the build system dependencies:

```sh
sudo emerge dev-lang/nasm dev-vcs/git sys-fs/fuse
```

- If you want to use QEMU, run:

```sh
sudo emerge app-emulation/qemu
```

- If you want to use VirtualBox, install from the VirtualBox [Linux Downloads](https://www.virtualbox.org/wiki/Linux_Downloads) page.

### GNU Guix Users

Rust nightly isn't packaged in Guix currently, so you need a
FHS-enabled (Filesystem Hierarchy Standard) container to use rustup:

```sh
guix shell --pure --container --emulate-fhs --network --share=$HOME \
  coreutils bash curl grep gcc-toolchain@14.3.0 nss-certs \
  -- bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly'

guix shell --pure --container --emulate-fhs --network --share=$HOME \
  coreutils bash gcc-toolchain@14.3.0 nss-certs zlib glibc \
  -- bash -c 'export LD_LIBRARY_PATH=$(dirname $(gcc -print-file-name=libgcc_s.so.1)):$LD_LIBRARY_PATH && source ~/.cargo/env && cargo install cbindgen'
```

Then you will be able to run the actual build except for the part that uses FUSE to build the root filesystem. Modify `FerrionOS` in the command below to match where your sources are:

```sh
guix shell --pure --container --emulate-fhs --network --share=$HOME \
  coreutils bash curl wget gcc-toolchain@14.3.0 pkg-config fuse nss-certs zlib \
  grep make which findutils sed gawk diffutils tar gzip perl git git-lfs \
  binutils nasm just m4 patch autoconf automake help2man texinfo xz \
  bzip2 mpfr gmp file ncurses readline flex bison python ninja cmake \
  -- bash -c '
export LD_LIBRARY_PATH="/lib64:/lib:$LD_LIBRARY_PATH"
export CI=1
source ~/.cargo/env
cd ~/FerrionOS
make all PODMAN_BUILD=0 REPO_BINARY=1
'
```

The FUSE portion needs to run outside of a Guix shell container. To
do that, we will patch the Rust executables so they can find
`libgcc_s.so.1` under `/gnu/store` instead of `/lib` :

```sh
LIBGCC_DIR=$(guix shell --container --emulate-fhs gcc-toolchain bash coreutils \
  -- bash -c 'dirname $(readlink -f /lib64/libgcc_s.so.1)')
```

```sh
guix shell patchelf -- patchelf --set-rpath "$LIBGCC_DIR" build/fstools/bin/redox_installer
```

```sh
guix shell patchelf -- patchelf --set-rpath "$LIBGCC_DIR" build/fstools/bin/redoxfs
```

```sh
guix shell patchelf -- patchelf --set-rpath "$LIBGCC_DIR" build/fstools/bin/redoxfs-mkfs
```

Finally, you can run the image building part outside of a container
so that FUSE works and launch qemu:

```sh
guix shell make just nasm qemu -- make qemu PODMAN_BUILD=0
```

### FreeBSD Users

Install the build system dependencies:

```sh
sudo pkg install coreutils findutils gcc nasm pkgconf fusefs-libs3 \
cmake gmake wget openssl texinfo python automake gettext bison gperf \
autoconf curl file flex expat2 gmp png libjpeg-turbo sdl12 sdl2_ttf \
perl5.36 p5-HTML-Parser libtool m4 po4a syslinux ninja meson xdg-utils \
zip unzip llvm doxygen patch automake scons lua54 py-protobuf-compiler
```

- If you want to use QEMU, run:

```sh
sudo pkg install qemu qemu-system-x86_64
```

- If you want to use VirtualBox, run:

```sh
sudo pkg install virtualbox
```

### MacOS Users

Read the MacOS section of [Advanced Podman Build](./advanced-podman-build.md#macos) first. The `native_bootstrap.sh` script itself warns against a native build on MacOS and points to the Podman Build.

#### MacPorts

Install the build system dependencies:

```sh
sudo port install coreutils findutils gcc49 gcc-4.9 nasm pkgconfig \
osxfuse x86_64-elf-gcc cmake ninja po4a findutils texinfo autoconf \
openssl3 openssl11 bison curl wget file flex gperf expat gmp libpng \
jpeg libsdl12 libsdl2_ttf libtool m4 ninja meson python311 py37-mako \
xdg-utils zip unzip llvm-16 clang-16 perl5.24 p5-html-parser doxygen \
gpatch automake scons gmake lua protobuf-c
```

- If you want to use QEMU, run:

```sh
sudo port install qemu qemu-system-x86_64
```

- If you want to use VirtualBox, run:

```sh
sudo port install virtualbox
```

If you have some problem, try to install this Perl module:

```sh
cpan install HTML::Entities
```

#### Homebrew

Install the build system dependencies:

```sh
brew install automake bison gettext libtool make nasm gcc@7 \
gcc-7 pkg-config cmake ninja po4a macfuse findutils texinfo \
openssl@1.1 openssl@3.0 autoconf curl wget flex gperf expat \
gmp libpng jpeg sdl12-compat sdl2_ttf perl libtool m4 ninja \
meson python@3.11 zip unzip llvm doxygen gpatch automake scons \
lua ant protobuf redox-os/gcc_cross_compilers/x86_64-elf-gcc x86_64-elf-gcc
```

- If you want to use QEMU, run:

```sh
brew install qemu qemu-system-x86_64
```

- If you want to use VirtualBox, run:

```sh
brew install virtualbox
```

If you have some problem, try to install this Perl module:

```sh
cpan install HTML::Entities
```

## Install Rust Stable And Nightly

Install Rust, make the nightly version your default toolchain, list the installed toolchains, then install more Rust tooling:

```sh
curl https://sh.rustup.rs -sSf | sh
```

then

```sh
source ~/.cargo/env
```

```sh
rustup default nightly
```

```sh
rustup toolchain list
```

```sh
cargo install cbindgen just
```

The `. "$HOME/.cargo/env` command (equivalent to `source ~/.cargo/env`) have been added to your shell start-up file, `~/.bashrc`, but you may wish to add it elsewhere or modify it according to your own environment.

The `native_bootstrap.sh` script does this differently: it installs rustup with `stable` as the default toolchain, and the pinned versions `just` 1.50.0 and `cbindgen` 0.29.0. The nightly toolchain that builds the Cookbook is pinned in the `rust-toolchain.toml` file of the FerrionOS directory, and rustup installs it the first time `cargo` runs there.

## Customizing C compiler

The FerrionOS build requires a GCC-compatible compiler for the host operating system to build additional host tools. The host tools are searched automatically in the `PATH` environment variable under their plain names (`gcc`, `g++`, `gcc-ar`, `as`, `ld` and so on, see `generate_gnu_targets` in `src/env.rs` of the `redoxer` source).

If you have to use a different compiler because GCC is not installed or is incompatible, you can export the `REDOXER_HOST_*` environment variables that `redoxer` (recipe `recipes/dev/redoxer`) reads in the `.config` file:

```sh
export REDOXER_HOST_AR=ar
export REDOXER_HOST_AS=as
export REDOXER_HOST_CC=cc
export REDOXER_HOST_CXX=c++
export REDOXER_HOST_LD=ld
export REDOXER_HOST_NM=nm
export REDOXER_HOST_OBJCOPY=objcopy
export REDOXER_HOST_OBJDUMP=objdump
export REDOXER_HOST_PKG_CONFIG=pkg-config
export REDOXER_HOST_RANLIB=ranlib
export REDOXER_HOST_READELF=readelf
export REDOXER_HOST_STRIP=strip
```

> 📝 **Note:** The default compiler of FreeBSD and MacOS is Clang, and the Podman container has GCC 14.x (the GCC version in Debian 13). FerrionOS has verified neither another compiler nor another GCC version. Try to set these environment variables if you find any issues in any recipe compilation.

## Prefix

In addition to build tools from the system, building FerrionOS requires additional compilers and tools bootstrapped from your host compilers:

- GCC
- GNU Binutils
- Libtool
- LLVM
- Rust
- Clang
- Relibc

The tools that build FerrionOS are specific to each CPU architecture. These tools are located in the directory `prefix`, in a subdirectory named for the architecture, e.g. `prefix/x86_64-unknown-redox`. If you have problems with these tools, you can remove the subdirectory or even the whole `prefix` directory, which will cause the tools to be re-downloaded or rebuilt. The variable `PREFIX_BINARY` in `mk/config.mk` controls whether they are downloaded or built.

### Prebuilt Prefix

With `PREFIX_BINARY=1`, the default, the build downloads upstream's prebuilt prefix toolchain from upstream's server (the address is in `mk/prefix.mk`) to make building fast. FerrionOS publishes no toolchain of its own. The prebuilt prefix is made for the Podman container, so a native build needs glibc version 2.41 or newer (the container is based on Debian 13), and on ARM-based Linux hosts it exists only for the `aarch64` and `x86_64` targets. On a host that is not Linux, `make` sets `PREFIX_BINARY=0` by itself (`mk/config.mk`).

If your Linux distribution is not using glibc or its version is older than 2.41, you need to set `PREFIX_BINARY=0` and build your own prefix toolchain.

### Prefix: GCC

With `PREFIX_BINARY=0`, the build system compiles its own GCC, GNU Binutils and Libtool to cross-compile for the `redox` target. The whole build takes about a half hour or less if your hardware is relatively powerful. When it is completed it generates the `gcc-install` directory containing these cross-compilers.

### Prefix: Rust

Rust lists the `redox` targets as both Tier 2 and Tier 3 [platform support](https://doc.rust-lang.org/nightly/rustc/platform-support/redox.html). `x86_64-unknown-redox` is a Tier 2 target, which means that Rust's libstd is available for this target.

With `PREFIX_BINARY=0`, the build system compiles its own Rust compiler from the `rust` recipe (`recipes/dev/rust`) to be able to build Tier 3 libstd. Setting `PREFIX_USE_UPSTREAM_RUST_COMPILER=1` (see `mk/config.mk`) downloads the Rust project's official nightly binaries instead of building the Rust compiler.

Building Rust takes about 2 hours or more (if your hardware is relatively powerful) and it is also needed to compile LLVM. If you are building for the x86_64 target, downloading the official Rust binaries might be preferable. When it is completed it generates the `rust-install` directory containing the Rust compiler and its tools.

Note that upstream's fork of Rust, which the `rust` recipe fetches, may carry patches that the Rust project has not merged, so your experience with the official Rust binaries might be different than building it.

### Prefix: Clang

The toolchain includes Clang as an additional C/C++ compiler. Upstream describes the use of Clang as experimental. It is already used for additional compilation stages, such as compiling to WASI, and by the recipes that select it with `compiler` in their `[build]` section.

The Clang contained in this toolchain also comes with LLD and the LLVM runtime libraries of the `llvm-rt21` recipe (`libcxx`, `libcxxabi` and `libunwind`), which are meant to make the compilation independent of GCC. FerrionOS has not verified a build with Clang as the default compiler, which requires the `export REDOXER_USE_CLANG=true` command in `.config`.

When completed, it generates the `clang-install` directory containing Clang and its components.

### Prefix: Relibc

`relibc` (recipe `recipes/core/relibc`) is the [C standard library](https://en.wikipedia.org/wiki/C_standard_library) of FerrionOS. Relibc is needed for both GCC and Rust to compile programs.

The `relibc` recipe has no fixed revision and follows the head of upstream's repository, so even with `PREFIX_BINARY=1` relibc is compiled anyway, to make sure that the toolchain has the current libc. Compiling it is quick. When it is completed it generates the `sysroot` directory, which contains the GCC, Rust and Clang toolchains bundled with the freshly built relibc.

## Cookbook

The **Cookbook** system is an essential part of the FerrionOS build system. Each component package is built and managed by the Cookbook toolset. The variable `REPO_BINARY` in `mk/config.mk` controls if the recipes are compiled from sources or use upstream's prebuilt packages from upstream's package server, read the section [REPO_BINARY](./configuration-settings.md#repo_binary) for more details. See the [Including Programs in FerrionOS](./including-programs.md) page for examples of using the Cookbook toolset, if you will be developing recipes to include in FerrionOS.

## sccache

[sccache](https://github.com/mozilla/sccache/) is an important object compilation cache optimization to make rebuilds fast. It is automatically installed and enabled in Podman Build, but it's not the case with Native Build, as it has some caveats.

To enable it, download the prebuilt binaries in their [releases page](https://github.com/mozilla/sccache/releases) to a directory read by the `PATH` environment variable (typically `~/.cargo/bin`) and add this to `.config`:

```
SCCACHE_BUILD=1
```

`sccache` will run automatically in the background after the first time it runs. See [their configuration file](https://github.com/mozilla/sccache/blob/main/docs/Configuration.md) to customize many aspects of it. Do note that when running Podman build while native `sccache` exists and is started, you have to turn it off because it conflicts with `sccache` inside Podman which causes port conflicts:

```
sccache --stop-server
```

There is a very rare occasion where `sccache` will not pick up build cache, such as when the platform ABI changed. In this case, you have to remove `sccache` cache (`~/.cache/sccache`) manually. In a Podman build the `sccache` cache is in the container's home directory, at `build/podman/.cache/sccache`.

## Creating a Build Environment Shell

If you are working on specific components of the system, and will be using some of the tools in the FerrionOS directory and bypassing `make`, you may wish to create a build environment shell. This shell includes the `prefix` tools in your `PATH`. You can do this with:

```sh
make env
```

This command also works with a Podman Build, creating a shell in Podman and setting `PATH` to include the necessary build tools.

## Updating The Sources

If you want to update the build system or if some of the recipes have changed, you can update those parts of the system with `make pull`, which runs `git pull` in the FerrionOS directory and makes the next build rebuild the Cookbook. However, this will not update the sources of the recipes.

```sh
cd ~/FerrionOS
```

```sh
make pull
```

If you want to update the source for the recipes, use `make rebuild`, which fetches each source before it cooks the recipe, or `make fetch`, which only fetches.

## Changing the Filesystem Size and Contents

You can modify the size and contents of the filesystem for emulation and *livedisk* as described in the [Configuration Settings](./configuration-settings.md) page.

## Next Steps

Once this is all set up, the system can be built. See the [Native Build](./building-redox.md) page.
