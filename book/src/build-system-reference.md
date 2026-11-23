# Build System Reference

The build system downloads and creates several files that you may want to know about. There are also several `make` commands mentioned below, and a few extras that you may find useful. Here is a quick summary. All file paths are relative to the FerrionOS directory.

The `make` targets are documented in the comments of `Makefile` and `mk/*.mk`, and the build variables in `mk/config.mk`. Where this page disagrees with those files, the files are right.

- [Build System Organization](#build-system-organization)
  - [Root directory](#root-directory)
  - [GNU Make Configuration](#gnu-make-configuration)
  - [Podman Configuration](#podman-configuration)
  - [Filesystem Configuration](#filesystem-configuration)
  - [Cookbook](#cookbook)
  - [Build System Files](#build-system-files)
- [GNU Make Commands](#gnu-make-commands)
  - [Build System](#build-system)
  - [Podman](#podman)
  - [Recipe](#recipe)
  - [QEMU/VirtualBox](#qemuvirtualbox)
- [Environment Variables](#environment-variables)
- [Git Auto-checkout](#git-auto-checkout)
- [Update The Build System](#update-the-build-system)
  - [Update FerrionOS](#update-ferrionos)
- [Fix Breaking Changes](#fix-breaking-changes)
  - [All Recipes](#all-recipes)
  - [One Recipe](#one-recipe)
- [Configuration](#configuration)
  - [Format](#format)
  - [Filesystem Customization](#filesystem-customization)
- [Cross-Compilation](#cross-compilation)
- [Build Phases](#build-phases)
- [Scripts](#scripts)

## Context

- `$ARCH` is the environment variable for a CPU architecture directory
- `$BOARD` is the environment variable for a board model directory
- `$TARGET` is the environment variable for CPU-specific recipe binaries

## Build System Organization

### Root directory

- `podman_bootstrap.sh` - The script that installs the host tools for the Podman build. Run it as `./podman_bootstrap.sh -d`.
- `native_bootstrap.sh` - The script that installs the host tools for the Native build. Run it as `./native_bootstrap.sh -d`.
- `Makefile` - The main Makefile of the build system, it loads all the other Makefiles.
- `.config` - Where you override your build system settings. It is loaded by the Makefile (it is ignored by `git`).

### GNU Make Configuration

- `mk/config.mk` - The build system settings are here. You can override these settings in your `.config`, don't change them here to avoid conflicts in the `make pull` command.
- `mk/*.mk` - The rest of the Makefiles. You should not need to change them.

### Podman Configuration

- `podman/redox-base-containerfile` - The file used to create the image used by the Podman build. The installation of packages needed for the build is done here. See the [Adding Packages to the Build](./advanced-podman-build.md#adding-packages-to-the-build) section if you need to add additional packages.

### Filesystem Configuration

- `config` - This directory contains all filesystem configurations.
- `config/*.toml` - Filesystem configurations shared by all CPU architectures (a configuration can include other configurations to reduce duplication)
- `config/$ARCH/*.toml` - Filesystem configurations of one CPU architecture. When `config/$ARCH/$CONFIG_NAME.toml` exists, it is used instead of `config/$CONFIG_NAME.toml`.
- `config/base.toml` - Essential configuration, meant to be included by the others. It has no login accounts and does not make a bootable system by itself.
- `config/dev-accounts.toml` - The dev accounts: `user` with an empty password, in group `sudo`, and `root` with the password `password`. They are development credentials for test machines. `config/minimal.toml` and the configurations that include `config/base.toml` directly include this file.
- `config/minimal.toml` - The variant without server tools and Orbital.
- `config/server.toml` - The variant with some important tools without Orbital. It cooks far fewer packages than `desktop`.
- `config/x86_64/server-demo.toml` - `server` variant with a demonstration of a server software stack
- `config/desktop-minimal.toml` - The minimal `desktop` variant: `minimal` with Orbital.
- `config/desktop.toml` - The variant with some important CLI and GUI programs (this is the default configuration of the build system). It currently installs the COSMIC terminal, file manager, text editor and Orbital. Which desktop apps FerrionOS ships is not settled and may change.
- `config/x86_64/desktop.toml` - The `desktop` configuration of x86_64: the shared one with the fish, zsh and Nushell shells added, and Nushell as the login shell of `user`.
- `config/riscv64gc/desktop.toml` - The `desktop` configuration of RISC-V, including the shared desktop configuration.
- `config/desktop-contain.toml` - `desktop` variant that starts the graphical login through `contain_orblogin`, a part of the `contain` sandbox tool, which aims at limiting the schemes and files that a session can reach. FerrionOS has not verified it, see [Security](./security.md).
- `config/dev.toml` - The variant with development tools included (`config/aarch64/dev.toml` adapts it to that CPU architecture).
- `config/$ARCH/demo.toml` - The variant with `desktop` and optional programs and games.
- `config/x86_64/rustconf2025.toml` - `demo` variant with two more packages, a demonstration configuration inherited from upstream
- `config/wayland.toml` - Configuration for Wayland sessions and applications
- `config/acid.toml` - Configuration with the `acid` test suite.
- `config/os-test.toml` - Configuration with the `os-test` test suite
- `config/auto-test.toml` - Configuration with automated test suites execution
- `config/sys-build.toml` - Configuration with automated system compilation testing
- `config/tests.toml` - Configuration for tests
- `config/redoxer.toml` - Configuration used to create the Redoxer image
- `config/redoxer-gui.toml` - Configuration used to create the Redoxer GUI image
- `config/aarch64/raspi3bp/minimal.toml` - The `minimal` configuration of the Raspberry Pi 3 Model B+ board. A board model specific configuration lies in `config/$ARCH/$BOARD` and is selected with the `BOARD` variable; this is the only one today.
- `config/$ARCH/redoxer.toml` - CPU-specific Redoxer configuration
- `config/x86_64/full.toml` - Configuration with all the packages of `config/x86_64/ci.toml`
- `config/$ARCH/ci.toml` - The list of packages that `make ci-pkg` cooks (`mk/ci.mk`). Upstream's build server publishes its packages from this list; FerrionOS publishes no packages.
- `config/$ARCH/jeremy.toml` - A test configuration inherited from upstream.

### Build System Files

- `build` - The directory where the build system will place the final image. Usually `build/$(ARCH)/$(CONFIG_NAME)`, e.g. `build/x86_64/desktop`
- `build/$ARCH/$CONFIG_NAME/harddrive.img` - The FerrionOS image file, to be used by QEMU or VirtualBox for virtual machine execution on a Unix-like host.
- `build/$ARCH/$CONFIG_NAME/redox-live.iso` - The FerrionOS bootable image file, created by `make live`, to be used on real hardware for testing and possible installation (FerrionOS has not verified it on real hardware).
- `build/$ARCH/$CONFIG_NAME/repo.tag` - An empty file that, if present, tells the build system that all recipes required for the FerrionOS image have been successfully built. **The build system will not check for changes to your code when this file is present.** Use `make rebuild` to force the build system to check for changes.
- `build/podman` - The directory that Podman Build mounts as `/root`, the home directory in the container (`PODMAN_HOME` in `mk/podman.mk`). It includes the container's Rust installation, made by `podman/rustinstall.sh`. Use `make container_clean` to remove it. In some situations, you may need to remove this directory manually, possibly with root privileges.
- `build/container.tag` - An empty file, created during the first Podman build, so a Podman build knows when a reusable Podman image is available. Use `make container_clean` to force a rebuild of the Podman image on your next `make rebuild` run.
- `build/logs/$TARGET/recipe.log` - Where recipe build logs are stored.

### Cookbook

- `prefix/*` - Tools used by the Cookbook system. They are normally downloaded during the first system build (if you are having a problem with the build system, you can remove the `prefix` directory and it will be recreated during the next build).
- `repo` - Contains all packaged recipes.
- `web` - Contains optional generated web content for `repo`.
- `recipes/recipe` - A recipe (software port) directory (represented as `recipe`; the real path has a category directory in between, such as `recipes/core/kernel`), this directory holds the `recipe.toml` file.
- `recipes/recipe/recipe.toml` - The recipe configuration file, this configuration contains instructions for downloading Git repositories or tarballs, then creating executables or other files to include in the FerrionOS filesystem. Note that a recipe can contain dependencies that cause other recipes to be built, even if the dependencies are not otherwise part of your FerrionOS build.

(To learn more about the recipe system read the [Porting Applications using Recipes](./porting-applications.md) page)

- `recipes/recipe/source.tar` - The tarball of the recipe (renamed).
- `recipes/recipe/source` - The directory where the recipe source is extracted or downloaded.
- `recipes/recipe/target` - The directory where the recipe binaries are stored.
- `recipes/recipe/target/${TARGET}` - The directory for the recipe's binaries of the CPU architecture.
- `recipes/recipe/target/${TARGET}/build` - The directory where the recipe build system runs its commands.
- `recipes/recipe/target/${TARGET}/stage` - The directory where recipe binaries go before the packaging, after `make all`, `make rebuild` and `make image` the `installer` (recipe `recipes/core/installer`) will extract the recipe package on the image, generally at `/usr/bin` or `/usr/lib` in a FerrionOS filesystem hierarchy.
- `recipes/recipe/target/${TARGET}/sysroot` - The directory where recipe build dependencies (libraries) are shared, for example: `*.so`, `*.a` and `cmake` files.
- `recipes/recipe/target/${TARGET}/toolchain` - The directory where recipe tool dependencies (toolchain) are shared, for example: binaries.
- `recipes/recipe/target/${TARGET}/stage.pkgar` - The package file of the recipe, in the `pkgar` format.
- `recipes/recipe/target/${TARGET}/stage.toml` - Contains the metadata of the package, including its runtime dependencies.

## GNU Make Commands

You can combine `make` commands, but order is significant. For example, `make r.redox-games image` will build the `redox-games` recipe and create a new FerrionOS image, but `make image r.redox-games` will make the FerrionOS image before the recipe building, thus the new recipe binary will not be included on your FerrionOS filesystem.

### Build System

- `make pull` - Update the source code of the build system without building: it runs `git pull` in the FerrionOS directory and makes the next build rebuild the Cookbook.
- `make cookbook` - Update Cookbook binaries with local changes (when `make pull` is not used)
- `make all` - Builds the entire system, checking for changes and only building as required. Only use this for the first build. If the system was successfully built previously, this command may report `Nothing to be done for 'all'`, even if some recipes have changed. Use `make rebuild` instead.
- `make rebuild` - Update all binaries from recipes with source code changes (it does not detect changes on the toolchain) and create a new image. This should be your normal `make` target.
- `make rebuild-push` - Similar to `make rebuild` but push updated recipes instead of a image rebuild, use this to preserve your changes on the FerrionOS image after a full update (it will update downloaded packages if `REPO_BINARY` is enabled)
- `make prefix` - Download upstream's prebuilt toolchain (GCC, Rust and Clang) and build relibc. With `PREFIX_BINARY=0` the toolchain is built from source instead.
- `make fstools` - Build the Cookbook, and the FerrionOS image builder (installer) and RedoxFS for the host.
- `make fstools_clean` - Clean the image builder, Cookbook and RedoxFS binaries.
- `make fetch` - Download or update the sources of the recipes that are included in your filesystem configuration, and of their package dependencies, without building them (for a recipe with the `binary` rule, the prebuilt package is downloaded). It also removes `$(BUILD)/repo.tag`, so that the next `make all` cooks again.
- `make unfetch` - Delete the sources and binaries of the recipes that your filesystem configuration names.
- `make cook` - Build recipes enabled in the active filesystem configuration
- `make repo` - Cook the recipes of your filesystem configuration and package them. Does nothing if `$(BUILD)/repo.tag` is up to date. You will not need this.
- `make repo_clean` - Clean all recipe binaries. It leaves `$(BUILD)/repo.tag` in place, so follow it with `make rebuild`: `make all` and `make repo` would find nothing to do.
- `make repo_clean_target` - Clean all recipe binaries compiled to the active CPU architecture (`ARCH=value` environment variable)
- `make repo-lock` - Lock the commit hashes of recipe sources using a Git repositories
- `make repo-unlock` - Unlock the commit hashes of recipe sources using a Git repositories
- `make repo-rollback.<rev>` - Check out the build system at the Git revision `<rev>` and lock the Git recipes to their last commit before the date of that revision, for debugging. Undo it with `git checkout master`, `make pull` and `make repo-unlock`. It does not work with revisions from before 2026.
- `make clean` - Clean all recipe binaries and packages, the build directory and the toolchain of the active configuration, and the Cookbook binaries (Note that `make clean` may require some tools to be built).
- `make static_clean` - Clean relibc, also in the sysroot of the toolchain, and the statically linked recipes that `static_clean` in `mk/prefix.mk` lists
- `make distclean` - Clean all recipe sources, binaries, and packages (**please back up your source changes before the execution of this command**).
- `make find.recipe` - Show the directory of a recipe
- `make repo-tree` - Show all recipe dependencies (`build.dependencies` data type) that will be built, also check what dependencies were built or not
- `make image-tree` - Show the package dependencies (`package.dependencies` data type) that will be pushed into the FerrionOS image, and the estimated size of the image
- `make image` - Builds a new image, `build/$ARCH/$CONFIG_NAME/harddrive.img`, without checking if any recipes have changed. It can save you some time if you are just updating one recipe with `make r.recipe`
- `make push` - Only install recipes with new changes in an existing FerrionOS image
- `make install` - Extract the packages of your filesystem configuration into `DESTDIR`, which defaults to `/`. It is meant for self-hosted development testing on a FerrionOS host: on any other host, pass `DESTDIR=<directory>`.
- `make mount` - Mounts the FerrionOS image as a filesystem at `$(BUILD)/filesystem`. **Do not use this if QEMU is running**, and remember to use `make unmount` as soon as you are done. This is not recommended, but if you need to get a large file onto or off of your FerrionOS image, this is available as a workaround.
- `make unmount` - Unmounts the FerrionOS image filesystem. Use this as soon as you are done with `make mount`, and **do not start QEMU** until this is done.
- `make live` - Creates a bootable image, `build/$ARCH/$CONFIG_NAME/redox-live.iso`. Recipes are not usually rebuilt.
- `make popsicle` - Flash the FerrionOS bootable image on your USB device using the [Popsicle](https://github.com/pop-os/popsicle) tool (the `popsicle-gtk` executable must be present on your shell `$PATH` environment variable, you can get the executable by extracting the AppImage, installing from the package manager or building from source)
- `make mount_live` - Mount the live disk ISO
- `make env` - Creates a shell with a build environment configured to use the cross toolchain. If you are using Podman Build it will change your current terminal shell to the container shell, you can use it to update crates of Rust programs or debug build issues such as missing packages (if you are using the Podman Build you can only use this command in one terminal shell, because it will block the build system directory access from other Podman shell)

### Podman

- `make container_shell` - Open the GNU Bash shell of the Podman container as the active shell of your terminal. Its user is the container's `root`, which rootless Podman maps to your own user on the host (do not use this command to replace the `make env` command because it does not set up the cross toolchain in the Podman container shell)
- `make container_clean` - This will discard images and other files created by Podman.
- `make container_touch` - If you have removed the file `build/container.tag`, but the container image is still usable, this will recreate the `container.tag` file and avoid rebuilding the container image.
- `make container_kill` - If you have started a build using Podman Build, and you want to stop it, `Ctrl-C` may not be sufficient. Use this command to terminate the most recently created container.

### Recipe

(The `recipe` part in `make x.recipe` is the recipe directory name)

(Recipe target options use the `make x.--option-name` syntax. Any recipe target (x) can be run on all recipes with the `--all` option, on all recipes that have a package in `repo` with `--all-compiled`, and on the ones of those whose rule is `binary` with `--all-binaries`)

- `make lc.recipe` (**use before doing local changes to a recipe source**) - Set the recipe rule to `local`, which does the following things:

  - The Cookbook neither fetches nor patches the `source` directory of the recipe, which keeps local changes, and the existing recipe binaries are cleaned
  - Automatically download sources if they don't exist
  - Enable source compilation with pre-built dependencies when `REPO_BINARY` is enabled

- `make f.recipe` (abbreviation of `fetch`) - Download one or multiple recipe sources
- `make f.--all-binaries` - Update all previously downloaded recipe packages
- `make r.recipe` (abbreviation of `repo`) - Build one or multiple recipes, checking if the recipe source has changed, and creating the executable, etc. e.g. `make r.redox-games` (it builds the Cookbook and the toolchain first if they are missing, but it does not replace the `make all` command: it builds neither the other recipes of your filesystem configuration nor the image)

  - Meta-packages need the `--with-package-deps` option, for example: `make r.meta-package,--with-package-deps`
  - The recipe is built even if it's not in your filesystem configuration.
  - This command will continue where you stopped the build process, it's useful to save time if you had a compilation error and patched a crate

- `make ri.recipe` - Build or rebuild the recipe and install it as `make i.recipe` does
- `make p.recipe` (abbreviation of `push`) - Install one or multiple recipe packages to an existing FerrionOS image (**do not use this if QEMU is running**)
- `make p.--all-compiled` - Install all compiled recipes to FerrionOS image
- `make i.recipe` (abbreviation of `install`) - Extract the package of a recipe, with its package dependencies, into `DESTDIR`, which defaults to `/`. It is meant for self-hosted development testing on a FerrionOS host: on any other host, pass `DESTDIR=<directory>`.
- `make bc.recipe` - Set the recipe rule to `binary` (pre-built package) and clean existing recipe binaries
- `make sc.recipe` - Set the recipe rule to `source` (enable compilation) and clean existing recipe binaries
- `make nc.recipe` - Set the recipe rule to `ignore` (disable installation on FerrionOS image) and clean existing recipe binaries
- `make cc.recipe` - Clear the recipe rule that the targets above wrote to `cookbook.lock`, so that the rule comes from the filesystem configuration or the default again, and clean existing recipe binaries
- `make c.recipe` (abbreviation of `clean`) - Clean one or multiple recipe binaries (the `target` directory of the recipe), while preserving the packages already in `repo`.
- `make u.recipe` (abbreviation of `unfetch`) - Clean one or multiple recipe source code and binaries, while preserving the packages already in `repo` (**please back up your source changes before the execution of this command**)
- `make u.--all` - Remove all previously downloaded source/binaries and preserve recipe packages
- `make rt.recipe` (abbreviation of `repo-tree`) - Show the recipe dependencies that will be built, also check what dependencies were built or not
- `make pt.recipe` (abbreviation of `push-tree`) - Show the package dependencies that will be pushed into the FerrionOS image
- `make rp.recipe` - A shortcut for `make r.recipe p.recipe`
- `make rp.--all-binaries` - Install all downloaded recipe packages to FerrionOS image
- `make cr.recipe` - A shortcut for `make c.recipe r.recipe`
- `make crp.recipe` - A shortcut for `make c.recipe r.recipe p.recipe`
- `make cri.recipe` - A shortcut for `make c.recipe r.recipe i.recipe`
- `make uc.recipe` - A shortcut for `make u.recipe c.recipe`
- `make ucr.recipe` - A shortcut for `make u.recipe c.recipe r.recipe` (**please back up your source changes before the execution of this command**)
- `make ucrp.recipe` - A shortcut for `make u.recipe c.recipe r.recipe p.recipe`
- `make ucri.recipe` - A shortcut for `make u.recipe c.recipe r.recipe i.recipe`
- `make ucf.recipe` - A shortcut for `make u.recipe c.recipe f.recipe`
- `make bcr.recipe` - A shortcut for `make bc.recipe r.recipe`
- `make bcrp.recipe` - A shortcut for `make bc.recipe rp.recipe`
- `make scr.recipe` - A shortcut for `make sc.recipe r.recipe`
- `make scrp.recipe` - A shortcut for `make sc.recipe rp.recipe`
- `make lcrp.recipe` - A shortcut for `make lc.recipe rp.recipe`
- `make x.--all` - Run any recipe target (x) in all recipes
- `make x.--category-directory-name` - Run any recipe target (x) in all recipes of some category directory at `recipes` (like `make u.--category-wip` which clean all recipe sources and binaries from the `wip` directory, for example), if you need to use a sub-category use `--category-directory-name.subdirectory`, with a dot in place of the slash

All recipe targets also support multiple recipe entries by separating each recipe name with a comma. for example: `make f.recipe1,recipe2` will download the sources of `recipe1` and `recipe2`

### QEMU/VirtualBox

- `make qemu` - Boot FerrionOS in QEMU, if a `build/$ARCH/$CONFIG_NAME/harddrive.img` file exists QEMU will run using that image. If you want to force a rebuild first, run the `make rebuild qemu` command. Sometimes `make qemu` will detect changes and rebuild, but this is not typical. The QEMU options are `make` variables defined in `mk/qemu.mk`, which is the source of truth for them
- `make qemu gpu=no` - Start QEMU without a GUI (disables Orbital).
- `make qemu gpu=virtio` - Start QEMU with the VirtIO GPU driver.
- `make qemu audio=no` - Start QEMU without a sound device.
- `make qemu usb=no` - Start QEMU without the xHCI USB controller.
- `make qemu uefi=no` - Boot without the UEFI firmware, which is the default on x86_64 and aarch64. On x86_64 this is a BIOS boot, which shows the menu of the bootloader.
- `make qemu live=yes` - Boot the live image `redox-live.iso` instead of `harddrive.img`. The bootloader loads the live image whole into memory.
- `make qemu disk=nvme` - Boot FerrionOS from a NVMe interface (high-performance SSD emulation). This is the default, except for the `raspi3bp` board.
- `make qemu disk=usb` - Boot FerrionOS from a virtual USB device.
- `make qemu disk=cdrom` - Boot FerrionOS from a virtual CD-ROM disk.
- `make qemu kvm=no` - Start QEMU without the [Linux KVM](https://en.wikipedia.org/wiki/Kernel-based_Virtual_Machine) acceleration if it's not supported.
- `make qemu iommu=yes` - Start QEMU with an emulated Intel [IOMMU](https://en.wikipedia.org/wiki/Input-output_memory_management_unit) (VT-d with interrupt remapping) attached to the virtual machine (x86-64 only). The kernel turns DMA remapping on with a one-to-one map, so every device still reaches all of the virtual machine's memory: this protects nothing yet (DMA protection is planned).
- `make qemu iommu=test` - The same with QEMU's test devices `iommu-testdev` and `edu` behind the IOMMU, for the IOMMU checks of `scripts/vm-test.py`.
- `make qemu share=/path/to/directory` - Share a host directory with FerrionOS using VirtIO-FS (it requires `virtiofsd` on the host), the directory is available at `/scheme/virtio-fs.host` inside of FerrionOS. Add `:ro` to the end of the directory (`share=/path/to/directory:ro`) to make it read-only.
- `make qemu share="tag1=/path/one tag2=/path/two"` - Share several host directories, each one is available at `/scheme/virtio-fs.<tag>`. Read the [VirtIO-FS Shared Directories](./virtio-fs.md) page for the rules and the troubleshooting.
- `make qemu share=redox` - Share the build system directory itself, it is available at `/scheme/virtio-fs.redox` (it includes the running disk image, read the warning in the [VirtIO-FS Shared Directories](./virtio-fs.md) page). `share="redox=/path"` is a normal share with the tag `redox`.
- `make qemu gdb=yes` - Start QEMU with its GDB server listening on TCP port 1234 and the virtual machine halted until a debugger connects (`gdb=nonblock` does not halt it). Run the `make gdb` command in another shell to debug the kernel, or read [GDB On QEMU](./troubleshooting.md#gdb-on-qemu) to debug a program of a recipe.
- `make gdb` - Start `rust-gdb` on the host system with the symbols of the kernel and connect it to the GDB server of QEMU.
- `make qemu option1=value option2=value` - Cumulative QEMU options are supported.
- `make ch` - Boot FerrionOS in [Cloud Hypervisor](./cloud-hypervisor.md) (x86-64 only), with the serial console on the terminal and the network provided by `passt`. The first run downloads Cloud Hypervisor and its firmware to `build/cloud-hypervisor` directory.
- `make ch-fetch` - Only download Cloud Hypervisor, `ch-remote` and the firmware, without building or starting anything.
- `make ch share=/path/to/directory` - Boot FerrionOS in Cloud Hypervisor and share host directories, the `share` option works the same as in `make qemu`.
- `make ch net=tap` - Boot FerrionOS in Cloud Hypervisor using the `redox0` tap device instead of `passt`. Use the `scripts/ch-tap-setup.sh` script in the build system to create the tap device.
- `make ch net=no` - Boot FerrionOS in Cloud Hypervisor without a network device.
- `make ch-shutdown` - Stop the Cloud Hypervisor virtual machine, run it from another terminal.
- `make virtualbox` - Boot FerrionOS in VirtualBox (inherited from upstream and not verified by FerrionOS), it requires the VirtualBox service to be running, run `systemctl status vboxdrv.service` to verify or `akmods; systemctl restart vboxdrv.service` to enable on systems using systemd.

## Environment Variables

- `$(BUILD)` - Represents the build directory of the active configuration, `build/$(ARCH)/$(CONFIG_NAME)`.
- `$(ARCH)` - Represents the CPU architecture directory at `build`
- `${TARGET}` - Represents the CPU architecture directory at the `recipes/recipe/target` directory
- `$(CONFIG_NAME)` - Represents your filesystem configuration directory at `build/$ARCH`

Use these variables with the `"` symbol to clean any spaces on the path, spaces are interpreted as command separators and will break the commands.

Example:

```
"${VARIABLE_NAME}"
```

If you have a directory inside the variable directory you can call it with:

```
"${VARIABLE_NAME}"/directory-name
```

Or

```
"${VARIABLE_NAME}/directory-name"
```

## Git Auto-checkout

The `make rebuild` and `make r.recipe` commands will Git checkout (change the active branch) of the recipe source to the revision or branch that the recipe names, or to the default branch of its repository if it names none (only recipes that fetch Git repositories are affected, thus all the system components).

If you are working in a separated branch on the recipe source and want to disable this, set the rule of your recipe to `local`, with the `make lc.your-recipe` command or on your filesystem configuration :

```
[packages]
...
your-recipe = "local"
...
```

## Update The Build System

This command updates your build system and the recipes: it runs `git pull` in the FerrionOS directory and makes the next build rebuild the Cookbook.

```sh
make pull
```

### Update FerrionOS

This command updates the sources and binaries of the system and the applications, and pushes them into the existing image.

```sh
make rebuild-push
```

(If you need a new FerrionOS image, run the `make rebuild` command)

Or (if you enabled the `REPO_BINARY` environment variable)

```sh
make fetch push
```

(If you need a new FerrionOS image, the `make rebuild` command also works with `REPO_BINARY` enabled)

If you want to just update the system without non-essential programs, run the following command:

```sh
make rp.sys,--with-package-deps
```

Or if you are using Orbital:

```sh
make rp.sys,sys-gui,--with-package-deps
```

Sometimes you need to update the statically linked recipes manually with the `make static_clean rebuild` command or also rebuild all dynamically linked recipes with the `make repo_clean rebuild` command (not needed if `REPO_BINARY` is enabled, except compiled recipes).

If you want to just rebuild the system without non-essential programs, run the following command:

```sh
make crp.sys,--with-package-deps
```

Or if you are using Orbital:

```sh
make crp.sys,sys-gui,--with-package-deps
```

(The Podman container image is built again automatically if the Containerfile changes, for example after `make pull`, but you can also force the container image to be updated with the `make container_clean` command)

## Fix Breaking Changes

To learn how to fix breaking changes before and after build system updates read [this](./troubleshooting.md#prevent-and-fix-breaking-changes) section.

### All recipes

To pass the new relibc changes for all recipes (programs are the most common case) you will need to rebuild all recipes, unfortunately it's not possible to use `make rebuild` because it can't detect the relibc changes to trigger a complete rebuild.

To clean all recipe binaries and trigger a complete rebuild, run the command below. The `make repo_clean` command leaves `repo.tag` in place, so `make repo` or `make all` would find nothing to do after it:

```sh
make repo_clean rebuild
```

### One Recipe

To pass the new relibc changes to one recipe, run:

```sh
make cr.recipe
```

## Configuration

You can find the global settings on the [Configuration Settings](./configuration-settings.md) page.

### Format

The filesystem configurations, the recipes and the `cookbook.toml` file use the [TOML](https://toml.io/en/) format, which has a very easy syntax and is very flexible. The `.config` file uses the syntax of `make`.

You can see what the format supports on the [TOML](https://toml.io/en/v1.0.0) website.

### Filesystem Customization

Read the [Filesystem Customization](./configuration-settings.md#filesystem-customization) section.

## Cross-Compilation

The FerrionOS build system is an example of [cross-compilation](https://en.wikipedia.org/wiki/Cross_compiler). The cross toolchain (the `prefix` directory, see `mk/prefix.mk`) runs on Linux, and produces executables for the `redox` target, which run on FerrionOS. Anything that is installed with your package manager is just part of the toolchain and does not go on FerrionOS.

In the background, the `make all` command downloads upstream's prebuilt toolchain to build all recipes (upstream's patched forks of rustc, GCC and LLVM).

If you are using Podman, which is the default, `make all` will install the toolchain and compile all recipes in a container.

The recipes produce executables for the `redox` target. At the end of the build process, these executables are installed inside the image.

`relibc`, the C library of FerrionOS, provides the [system calls](https://docs.rs/redox_syscall/latest/syscall/) to any software.

- [OSDev article about cross-compilation](https://wiki.osdev.org/Why_do_I_need_a_Cross_Compiler%3F)

## Build Phases

Every build system command/script has phases, read this page to know them.

- [Build Phases](./build-phases.md)

## Scripts

You can use these scripts to perform actions not implemented as `make` commands in the build system.

- To run a script use the following command:

```sh
scripts/script-name.sh input-text
```

The "input-text" is the word used by the script.

### Changelog

- `scripts/changelog.sh` - Show the changes of the components since the last Git tag. It is inherited from upstream and not adapted: it looks for `installer/target/release/redox_installer` and for `cookbook` and `rust` directories, which this repository does not have.

### Recipe Files

List the files of the mounted `desktop` image of the host's CPU architecture, each with the recipes that provide it. The script takes no argument, mounts and unmounts the image itself, and is inherited from upstream and not verified by FerrionOS.

```sh
scripts/installed-pkg-files.sh
```

### Recipe Categories

Run `make` options on some recipe category.

```sh
scripts/recipe-category.sh x category-name
```

Where `x` is your `make` option, it can be `f`, `r`, `c`, `u`, `p` or a combination that `make` understands, such as `cr` or `ucr`. A sub-category is written with its path, for example `wip/dev`.

### Include Recipes

Create a list with `recipe = {} #TODO` for quick testing of WIP recipes.

```sh
scripts/include-recipes.sh "TODO.text"
```

You will insert the text after the `#TODO` in the `text` part, it can be found on the `recipe.toml` file of the recipe.

If you want to add recipes to a `ci.toml` filesystem configuration, the recipe names must be sorted in alphabetical order, to do this from the output of this script use the following command:

```sh
scripts/include-recipes.sh "TODO.text" | sort
```

### Recipe Analysis

Show the directories and files on the `stage` and `sysroot` directories of some recipe (to identify packaging issues or violations).

```sh
scripts/show-package.sh recipe
```

### Recipe Commit Hash

Show the current Git commit of every fetched recipe source in `recipes/core`. The script takes no argument.

```sh
scripts/commit-hash.sh
```

### Package Size

Show the package size of the recipes (`stage.pkgar`). Without a recipe name it shows all compiled recipes. Run it inside the `make env` shell.

```sh
scripts/pkg-size.sh recipe
```

### Recipe Location

Show the location of the written recipe.

```sh
scripts/recipe-path.sh recipe
```

### Recipe Match

Search for some text in the files of all recipes (the `recipe.toml` files, patches and other files under `recipes`) and show the content of the files that match.

(Requires `bat` and `ripgrep` to be installed, run `cargo install bat ripgrep` to install)

```sh
scripts/recipe-match.sh "text"
```

### Print Recipe

Show the content of the recipe configuration.

```sh
scripts/print-recipe.sh recipe
```

### Recipe Executables

List the recipe executables to find duplicates and conflicts.

- By default the script will only verify duplicates, if the `-a` option is used it will print the executable names of all compiled recipes
- The `-arm64` option will show the ARM64 recipe executables

```sh
scripts/executables.sh
```

### Cargo Update

Download the recipe source and run `cargo update`

```sh
scripts/cargo-update.sh recipe
```

### Dual Boot

- `scripts/dual-boot.sh` - Install FerrionOS in a partition of your storage device (the one labeled `REDOX_INSTALL`, or the device that you pass) and add a boot entry (if you are using the [systemd-boot](https://www.freedesktop.org/wiki/Software/systemd/systemd-boot/) boot loader). It is inherited from upstream and not verified by FerrionOS.

### Ventoy

- `scripts/ventoy.sh` - Create and copy the FerrionOS bootable image to a [Ventoy](https://www.ventoy.net/en/index.html)-formatted device. It is inherited from upstream and not verified by FerrionOS.

### Recipe Debugging (Rust)

- `scripts/backtrace.sh` - Allow the user to copy a Rust backtrace from FerrionOS and retrieve the symbols (use the `-h` option to show the "Usage" message).

### Boot Test

- `scripts/vm-test.py` - Boots a FerrionOS image in QEMU through `make qemu` and checks it the way a person would: it watches the screen, reads the serial console and types.

### Security Audit

- `scripts/security-audit.py` - Audits the tree for security findings without building anything.
