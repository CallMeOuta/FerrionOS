# Quick Workflow

This page contains the most quick testing/development workflow for people that want an direct list to do things.

**You need to fully understand the build system to use this workflow, as it doesn't give a detailed explanation of each command to save time and space**

- [Install Rust Nightly](#install-rust-nightly)
- [Update Rust](#update-rust)
- [Download a new build system copy without the bootstrap script](#download-a-new-build-system-copy-without-the-bootstrap-script)
- [Install the required dependencies for the build system](#install-the-required-dependencies-for-the-build-system)
- [Build the system](#build-the-system)
- [Update the build system](#update-the-build-system)
- [Update the toolchain and relibc](#update-the-toolchain-and-relibc)
- [Update recipes and FerrionOS image](#update-recipes-and-ferrionos-image)
- [Only update system recipes and install in FerrionOS image](#only-update-system-recipes-and-install-in-ferrionos-image)
- [Update everything](#update-everything)
- [Wipe statically linked recipe binaries](#wipe-statically-linked-recipe-binaries)
- [Wipe and build all recipe binaries](#wipe-and-build-all-recipe-binaries)
- [Wipe the toolchain and download again](#wipe-the-toolchain-and-download-again)
- [Wipe the toolchain/recipe binaries and download/build them again](#wipe-the-toolchainrecipe-binaries-and-downloadbuild-them-again)
- [Wipe toolchain/recipe binaries and Podman container, update build system source and rebuild the system](#wipe-toolchainrecipe-binaries-and-podman-container-update-build-system-source-and-rebuild-the-system)
- [Wipe all recipe sources/binaries and download/build them again](#wipe-all-recipe-sourcesbinaries-and-downloadbuild-them-again)
- [Use the "myfiles" recipe to insert your files on FerrionOS image](#use-the-myfiles-recipe-to-insert-your-files-on-ferrionos-image)
- [Disable a recipe on the filesystem configuration](#disable-a-recipe-on-the-filesystem-configuration)
- [Create logs](#create-logs)
- [Temporarily build the toolchain from source](#temporarily-build-the-toolchain-from-source)
- [Build and install a meta-package](#build-and-install-a-meta-package)
- [Build some filesystem configuration for some CPU architecture](#build-some-filesystem-configuration-for-some-cpu-architecture)
- [Build some filesystem configuration for some CPU architecture (using pre-built packages from upstream's package server)](#build-some-filesystem-configuration-for-some-cpu-architecture-using-pre-built-packages-from-upstreams-package-server)
- [Boot FerrionOS on QEMU from a NVMe device](#boot-ferrionos-on-qemu-from-a-nvme-device)
- [Boot FerrionOS on QEMU from a NVMe device with a custom number of CPU threads](#boot-ferrionos-on-qemu-from-a-nvme-device-with-a-custom-number-of-cpu-threads)
- [Boot FerrionOS on QEMU from a NVMe device, a custom number of CPU threads and memory](#boot-ferrionos-on-qemu-from-a-nvme-device-a-custom-number-of-cpu-threads-and-memory)

#### Install Rust Nightly

```sh
curl https://sh.rustup.rs -sSf | sh -s -- --default-toolchain nightly
```

Use Case: Configure the host system without the build system bootstrap scripts.

#### Update Rust

```sh
rustup update
```

Use Case: Try to fix Rust problems.

#### Download a new build system copy without the bootstrap script

```sh
git clone https://github.com/CallMeOuta/FerrionOS.git
```

Use Case: Commonly used when a big build system breakage was not prevented before an update or to wipe leftovers.

### Install the required dependencies for the build system

Run the bootstrap script from inside the FerrionOS directory, and always with the `-d` option: without it, the script clones upstream's build system into a new `redox` directory and deletes itself.

```sh
cd FerrionOS
```

```sh
./podman_bootstrap.sh -d
```

Use Case: Install new build tools or update the existing ones.

#### Build the system

```sh
cd FerrionOS
```

```sh
make all
```

Use Case: Build the system from a clean build system copy.

A first build needs 50 GB of storage or more. For a long or unattended build, run `CI=1 COOKBOOK_LOGS=true make all` instead: `CI` turns off the Cookbook's terminal interface, which waits at a prompt when a recipe fails, and `COOKBOOK_LOGS=true` keeps each recipe's output in `build/logs/<target>/<recipe>.log`.

#### Update the build system

```sh
make pull
```

Use Case: Keep the build system up-to-date.

#### Update the toolchain and relibc

```sh
make c.relibc prefix
```

Use Case: Keep relibc in the toolchain up-to-date. `make prefix` alone does nothing while the `sysroot` directory of the toolchain exists, and the prebuilt compilers are downloaded again only after the `prefix` directory is wiped.

#### Update recipes and FerrionOS image

```sh
make rebuild
```

Use Case: Keep the FerrionOS image up-to-date.

#### Only update system recipes and install in FerrionOS image

```sh
make rp.sys,sys-gui,--with-package-deps
```

Or if incremental compilation can't be used:

```sh
make crp.sys,sys-gui,--with-package-deps
```

#### Update everything

Install the `topgrade` tool to update your system packages (you can install it with `cargo install topgrade`)

```sh
topgrade
```

```sh
make pull rebuild
```

Use Case: Try to fix most problems caused by outdated recipes, toolchain and build system configuration.

#### Wipe statically linked recipe binaries

```sh
make static_clean rebuild
```

#### Wipe and build all recipe binaries

```sh
make repo_clean rebuild
```

#### Wipe the toolchain and download again

```sh
rm -rf prefix
```

```sh
make prefix
```

Use Case: Commonly used to fix problems.

#### Wipe the toolchain/recipe binaries and download/build them again

```sh
make clean all
```

Use Case: Commonly used to fix breaking changes on recipes.

#### Wipe toolchain/recipe binaries and Podman container, update build system source and rebuild the system

```sh
make clean container_clean pull all
```

Use Case: Full build system binary cleanup and update to avoid most configuration breaking changes

#### Wipe all recipe sources/binaries and download/build them again

```sh
make distclean all
```

Use Case: Fix source/binary breaking changes on recipes or save space.

#### Use the "myfiles" recipe to insert your files on FerrionOS image

```sh
mkdir recipes/other/myfiles/source
```

```sh
nano config/my_config.toml
```

```toml
[packages]
myfiles = {}
```

```sh
make rp.myfiles
```

Use Case: Quickly insert files on the FerrionOS image or keep files between rebuilds.

#### Disable a recipe on the filesystem configuration

```sh
nano config/my_config.toml
```

```toml
[packages]
recipe-name = "ignore"
```

Use Case: Mostly used if some default recipe is broken.

#### Create logs

```sh
CI=1 COOKBOOK_LOGS=true make some-command 2>&1 | tee file-name.log
```

Use Case: Report errors (see [Participating](./contributing.md)). `CI=1` turns off the Cookbook's terminal interface, which does not fit in a log file, and `COOKBOOK_LOGS=true` also keeps each recipe's output in `build/logs/<target>/<recipe>.log`.

#### Temporarily build the toolchain from source

```sh
make prefix PREFIX_BINARY=0
```

Use Case: Test toolchain fixes.

#### Build and install a meta-package

```sh
make rp.meta-package,--with-package-deps
```

#### Build some filesystem configuration for some CPU architecture

```sh
make all CONFIG_NAME=your-config ARCH=$ARCH
```

Use Case: Quickly build FerrionOS variants without manual intervention on configuration files.

#### Build some filesystem configuration for some CPU architecture (using pre-built packages from upstream's package server)

(Much faster than the option above)

```sh
make all REPO_BINARY=1 CONFIG_NAME=your-config ARCH=$ARCH
```

Use Case: Quickly build FerrionOS variants with little compilation and without manual intervention on configuration files. `base`, `bootloader`, `kernel` and `userutils` are still built from source (`config/base.toml`), as is every package that a configuration sets to `"source"` or that upstream's package server lacks. Every other package is upstream's build of upstream's source.

#### Boot FerrionOS on QEMU from a NVMe device

```sh
make qemu disk=nvme
```

#### Boot FerrionOS on QEMU from a NVMe device with a custom number of CPU threads

```sh
make qemu disk=nvme QEMU_SMP=number
```

#### Boot FerrionOS on QEMU from a NVMe device, a custom number of CPU threads and memory

```sh
make qemu disk=nvme QEMU_SMP=number QEMU_MEM=number-in-mb
```
