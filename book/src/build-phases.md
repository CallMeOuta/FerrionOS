# Build Phases

This page explains what each build system command does in detail.

(Read the [Build System](./build-system-reference.md) to know the context of each command)

- [Bootstrap Scripts](#bootstrap-scripts)
- [Toolchain](#toolchain)
- [Build System](#build-system)
- [Recipes](#recipes)
- [QEMU](#qemu)

## Bootstrap Scripts

The bootstrap scripts are run from inside the FerrionOS directory, and only with the `-d` option. Without `-d` they clone upstream's build system into a new `redox` directory and delete themselves.

### podman_bootstrap.sh

With `./podman_bootstrap.sh -d`:

- Install Podman, GNU Make, Git, Rust, FUSE and QEMU if they are not installed in the host system.
- Stop there, without downloading the build system sources or writing a `.config` file.

### native_bootstrap.sh

With `./native_bootstrap.sh -d`:

- Install the Rust toolchain (using [rustup.rs](https://rustup.rs/)).
- Install the recipe build tools from your Linux or Unix-like distribution.
- Install `just` and `cbindgen` with Cargo.
- Stop there, without downloading the build system sources or writing a `.config` file.

## Toolchain

### make prefix

- With `PREFIX_BINARY=1`, the default: download upstream's prebuilt toolchain (the GCC, Rust and Clang archives) from upstream's server, if it is not present or if you executed `rm -rf prefix` to fix issues. The address is in `mk/prefix.mk`.
- With `PREFIX_BINARY=0`: build the toolchain from source instead.
- Cook the `relibc` recipe and assemble the toolchain with relibc in `prefix/$TARGET/sysroot` (for example `prefix/x86_64-unknown-redox/sysroot`).
- Do nothing if that `sysroot` directory is up to date. To build relibc again, run `make c.relibc` first: it removes relibc's build and the `sysroot` directory.

## Build System

### make pull

- Run `git pull` in the FerrionOS directory.
- Remove `build/fstools.tag`, so that the next build rebuilds the Cookbook.

### make all (first execution)

- Build the Cookbook from the `src` directory.
- Download upstream's prebuilt toolchain and cook relibc (if `make prefix` was not executed before).
- Build the installer and the RedoxFS tools for the host.
- Download the sources of the recipes specified on your filesystem configuration.
- Cross-compile the recipes for the `redox` target.
- Package recipe binaries as `pkgar` files.
- Install the packages in the image `harddrive.img`, a virtual disk formatted with RedoxFS.

### make all (second execution and next)

If the `build/$ARCH/$CONFIG_NAME/repo.tag` file is up to date, it will not do anything. If the `repo.tag` file is missing it will work like the `make rebuild` command.

### make all (Podman environment, first execution)

- Build the container image `redox-base` from `podman/redox-base-containerfile`, which installs the recipe build tools. The layer cache of the image is pulled from upstream's registry (`PODMAN_CACHE_PATH` in `mk/podman.mk`).
- Install Rust, `sccache`, `just` and `cbindgen` in the container's home directory, `build/podman`, with `podman/rustinstall.sh`.
- Build the Cookbook, download upstream's prebuilt toolchain and cook relibc (inside the container).
- Download the sources of the recipes specified on your filesystem configuration.
- Cross-compile the recipes for the `redox` target (inside the container).
- Package recipe binaries as `pkgar` files.
- Build the installer and the RedoxFS tools, and install the recipe packages in the image `harddrive.img`, a virtual disk formatted with RedoxFS (on the host, unless `FSTOOLS_IN_PODMAN=1`).

### make rebuild

- Verify source changes on recipes (if available, download them) or if a new recipe was added to the filesystem configuration.
- Cross-compile the recipes with changes for the `redox` target.
- Package recipe binaries with changes as `pkgar` files.
- Create a new image and install the recipe packages of the filesystem configuration in it.

### make image

- Remove the `harddrive.img` and `redox-live.iso` files of the active configuration.
- Run `make all`: the recipes are cooked only if `repo.tag` is missing or out of date, and a new FerrionOS image is created from the packages.

## Recipes

### make r.recipe

- Search the recipe location.
- Fetch the source with the method specified in the `[source]` section of the `recipe.toml`, or update a source that is already present (this step is skipped if the recipe has no `[source]` section or if its rule is `local`).
- Build the recipe library dependencies as shared or static objects.
- Start the compilation based on the template of the `recipe.toml`
- If the recipe is using Cargo, it will download the crates, build them, link them/relibc to the program binary.
- If the recipe is using GNU Autotools, CMake or Meson, they will check the build environment and dependencies presence/versions for compatibility, build the libraries or the program and link them/relibc to the final binary.
- Package the recipe binaries.

## QEMU

### make qemu

- If the image does not exist, it is built first, as `make all` does. If the filesystem configuration file or the toolchain in `prefix` is newer than `repo.tag`, the recipes are cooked and the image is created again. Changes in recipe sources are not detected: run `make rebuild` for them.
- If the image is mounted with `make mount`, it is unmounted.
- QEMU is started with the options that `mk/qemu.mk` builds from the make variables.
- If the `share` option is used, one `virtiofsd` process is started for each shared directory before QEMU, and they are stopped when QEMU exits (see [VirtIO-FS Shared Directories](./virtio-fs.md)). The `make ch` command does the same for Cloud Hypervisor.
- The QEMU window is shown. On UEFI the boot is quiet: the screen stays black and then shows a splash with a progress bar, and the bootloader's menu to choose the resolution appears only if a key is pressed in the first second (see [Quiet boot](./boot-process.md#quiet-boot)). A BIOS boot shows the menu.
- The bootloader does a bootstrap of the kernel, the kernel starts the init, the init starts the user-space daemons and Orbital.
- The Orbital login screen appears.
