# Troubleshooting the Build

This page covers the troubleshooting methods and tips for the FerrionOS build system.

(You must read the [Build System](./build-system-reference.md) page before)

- [Notes](#notes)
- [Setup](#setup)
    - [Podman](#podman)
        - [Manual Configuration](#manual-configuration)
    - [Native Build](#native-build)
    - [Git](#git)
- [Building The System](#building-the-system)
    - [.config and mk/config.mk](#config-and-mkconfigmk)
    - [Prefix](#prefix)
    - [Filesystem Configuration](#filesystem-configuration)
    - [Fetch](#fetch)
    - [Cookbook](#cookbook)
    - [Create the Image with FUSE](#create-the-image-with-fuse)
- [Solving Compilation Problems](#solving-compilation-problems)
    - [Environment Leakage](#environment-leakage)
    - [Update Your Build System](#update-your-build-system)
    - [Prevent and Fix Breaking Changes](#prevent-and-fix-breaking-changes)
    - [Update Your Branch](#update-your-branch)
    - [Update Crates](#update-crates)
    - [Verify The Dependency Tree](#verify-the-dependency-tree)
- [Debug Methods](#debug-methods)
    - [Boot](#boot)
    - [Applications](#applications)
    - [Symbols](#symbols)
- [Kill A Frozen FerrionOS VM](#kill-a-frozen-ferrionos-vm)
- [Kernel Panic](#kernel-panic)
    - [QEMU](#qemu)
    - [Real Hardware](#real-hardware-1)

## Notes

This section contains details that apply to FerrionOS problems on virtual machines and real hardware.

### General

Problems in both virtual machines and real hardware.

- If you aren't doing development and has a compilation or runtime problem be sure to verify if your build system/recipes sources and binaries are up-to-date or holding breaking changes. A build system update, single or complete recipe binary cleanup may fix your problems in most cases

Read the [Debug Methods](#debug-methods) and [Boot](#boot) sections for more details.

### Virtual Machine

- CPU acceleration (Linux KVM or Windows Hyper-V) may hide or trigger bugs because accelerated and emulated CPU virtualization have different timings that may change bug behavior, disable/enable them to verify if the bug still happen or is easier to reproduce (you can use the `kvm=no` option to disable Linux KVM in the FerrionOS build system, for example: `make qemu kvm=no` )

- The Orbital mouse cursor don't stop to work if you are using VirtIO and Orbital hangs, due to being processed on host. Which may give a false positive

### Real Hardware

- Test if your boot problem happens with live mode enabled and disabled: the `L` key toggles live mode in the bootloader's menu. On UEFI the menu appears only when a key is pressed in the first second after power-on, because FerrionOS [boots quietly](./boot-process.md#quiet-boot); a BIOS boot shows the menu without a key press
- If possible verify if your boot problem happens in UEFI and BIOS (UEFI has BIOS emulation which is called "CSM mode" and can be enabled in the UEFI settings)
- Photos are safer and faster to send boot logs than boot log text written by hand, which is error-prone and time consuming
- Verify if your computer has enough RAM to load the whole FerrionOS image: in live mode the bootloader loads the image into memory, because data streaming from USB is not supported yet. If it has 1GB or less of RAM, use the `server`, `desktop-minimal` or `desktop` configuration to avoid OOM panics
- If you have a very weak 64-bit Intel or AMD CPU (single core and L cache smaller than 1MB, like Intel Atom CPUs released before 2010) and 1GB or less of RAM, a 32-bit image with the `server`, `desktop-minimal` or `desktop` configuration is the path inherited from upstream to avoid OOM panics. FerrionOS publishes no images: build one with `ARCH=i586`. FerrionOS has not verified the i586 architecture

Read the [Debug Methods](#debug-methods) and [Boot](#boot) sections for more details.

### Reporting

Read the [Participating](./contributing.md) page to learn how to report a problem. A report can use these two terms:

- "Fresh build" or "clean build": you rebuilt all build system and recipe binaries from scratch (`make clean all` command)
- "Fresh clone" or "fresh copy": you downloaded a new copy of the build system with `git clone`

## Setup

The build system is downloaded with `git clone` (read the [Git](#git) section). The repository does not contain the sources of the system components, it only contains the build system: the cookbook fetches the source of each recipe when it builds it. Inside the FerrionOS directory, `./podman_bootstrap.sh -d` installs Podman and the other host tools of a Podman build, and `./native_bootstrap.sh -d` installs the Linux tools and libraries required to support the toolchain and build all recipes natively. Always pass `-d`: without it the scripts clone upstream's build system into a new `redox` directory and delete themselves.

### Podman


- If your Podman environment becomes broken, you can use `make container_clean all PODMAN_CACHE_PULL=0` (`podman system reset`) or `rm -rf build/podman`. In some cases, you may need to run the `sudo rm -rf build/podman` command.

- If any command ask your to choose an image repository (after the `make container_clean` command execution) select the first item, it will give an error and you need to run the `time make all` command again

- If your build appears to be missing libraries, have a look at the [Debugging Your Podman Build Process](./advanced-podman-build.md#debugging-your-build-process) section.

#### Manual Configuration

If you have problems setting Podman to rootless mode, do the following steps:

(These commands were taken from the official [Podman rootless wiki](https://github.com/containers/podman/blob/main/docs/tutorials/rootless_tutorial.md) and [Shortcomings of Rootless Podman](https://github.com/containers/podman/blob/main/rootless.md), thus it could be broken/wrong in the future, read the wiki to see if the commands match)

- Install the `podman`, `crun`, `slirp4netns` and `fuse-overlayfs` packages on your system.
- `podman ps -a` - This command will show all your Podman containers, if you want to remove all of them, run `podman system reset`
- Follow [this step](https://github.com/containers/podman/blob/main/docs/tutorials/rootless_tutorial.md#cgroup-v2-support) if necessary (if the Podman of your distribution use cgroups V2), you will need to edit the `containers.conf` file at `/etc/containers` or your user folder at `~/.config/containers`, change the line `runtime = "runc"` to `runtime = "crun"`
- Execute the `cat /etc/subuid` and `cat /etc/subgid` commands to see user/group IDs (UIDs/GIDs) available for Podman.

If you don't want to edit the file, you can use this command:

```sh
sudo usermod --add-subuids 100000-165535 --add-subgids 100000-165535 your-user
```

You can use the values `100000-165535` for your user, just edit the two text files, for example with `sudo nano /etc/subuid` and `sudo nano /etc/subgid`, when you finish, press Ctrl+X to save the changes.

- After the change on the UID/GID values, execute this command:

```sh
podman system migrate
```

- If you have a network problem on the container, this command will allow connections on the port 443 (without root):

```sh
sudo sysctl net.ipv4.ip_unprivileged_port_start=443
```

- Hopefully, you have a working Podman build now.

If you still have problems with Podman, or have improvements for this section, read the [Participating](./contributing.md) page to learn how to report them.

### Native Build

Not all Linux distributions are supported by `native_bootstrap.sh`, so if you have frequent compilation problems try a [Podman](https://podman.io/) build: run `./podman_bootstrap.sh -d` and remove `PODMAN_BUILD?=0` from your `.config` file.

If you want to support your Unix-like system without Podman, you can try to install the Debian/Ubuntu package equivalents for your system from your package manager/software store, you can see them on the `ubuntu()` function of the [native_bootstrap.sh](https://github.com/CallMeOuta/FerrionOS/blob/master/native_bootstrap.sh) script.

The `native_bootstrap.sh` script and `redox-base-containerfile` are inherited from upstream, where they cover the build system packages needed by the recipes of the [demo.toml](https://github.com/CallMeOuta/FerrionOS/blob/master/config/x86_64/demo.toml) filesystem configuration. FerrionOS has not verified the `demo` configuration.

(Note that some systems may have build environment problems hard and time consuming to fix, on these systems Podman will fix most headaches)

### Git

The build system is downloaded with:

```sh
git clone https://github.com/CallMeOuta/FerrionOS.git
```

```sh
cd FerrionOS
```

- Ensure that all the libraries and packages required by the build are installed by running `./native_bootstrap.sh -d` or, if you will be using the Podman build, the `./podman_bootstrap.sh -d` command.

## Building The System

When you run `make all`, the following steps occur.

### .config and mk/config.mk

- `make` scans [.config](./configuration-settings.md#config) and [mk/config.mk](./configuration-settings.md#mkconfigmk) for settings, such as the CPU architecture, configuration name, and whether to use **Podman** during the build process. Read through the [Configuration Settings](./configuration-settings.md) page to make sure you have the settings that are best for you.

### Prefix

The cross toolchain, referred to as **prefix** because it is prefixed with the CPU architecture name, is downloaded and/or built: with `PREFIX_BINARY=1` (the default) the prebuilt compilers come from upstream's server (`mk/prefix.mk`), and `PREFIX_BINARY=0` builds them from source. It holds modified versions of `cargo`, `rustc`, `gcc` and many other tools, and is placed in the `prefix` directory.

If you have a problem with the toolchain, try the `rm -rf prefix` and `make prefix` or `make clean all` (if `make prefix` is not enough) commands.

### Filesystem Configuration

The list of recipes to be built is read from the [filesystem configuration](./configuration-settings.md#filesystem-configuration) file, which is specified in [.config](./configuration-settings.md#config) or `mk/config.mk`. If your recipe is not being included in the build, verify if you have set the `CONFIG_NAME` or `FILESYSTEM_CONFIG` in the `.config` file.

### Fetch

Each recipe source is downloaded using `git` or `wget`, according to the `[source]` section of the `recipe.toml` file. The source is placed at `recipes/category/recipe-name/source`.

If you are doing work on the source of a recipe, set the rule of the recipe to `local` with `make lc.recipe-name` before the first edit: the cookbook then builds what is in the `source` directory and neither fetches nor patches it. To discard your changes to the source of a recipe, or to update to the latest version, put the default rule back with `make cc.recipe-name` and use `make uc.recipe-name` to remove both the source and any compiled code.

Fetching has no tag file: a cook fetches each recipe before it builds it (unless `REPO_OFFLINE=1` is set) and leaves a source that is up-to-date alone. Use `make fetch` to fetch the sources of all recipes of your filesystem configuration without building them.

### Cookbook

Each recipe is built according to the `recipe.toml` file. The recipe binaries or library objects are placed in the `target` directory, in a subdirectory named based on the CPU architecture.

If you have a problem with a recipe you are building, try the `make c.recipe-name` command. A common problem when building on unsupported systems is that certain recipes will fail to build due to missing dependencies. Try using the [Podman Build](./podman-build.md) or manually installing the recipe dependencies.

After all recipes are cooked, a tag file is created as `build/$ARCH/$CONFIG_NAME/repo.tag`. If this file is present, cooking is skipped. You can remove it manually, or use `make rebuild`, which will force refetching and rebuilding.

### Create the Image with FUSE

To build the final FerrionOS image, `redox_installer` uses [FUSE](https://github.com/libfuse/libfuse), creating a virtual filesystem and copying the recipe packages into it. This is done outside of Podman, even if you are using Podman Build, unless `FSTOOLS_IN_PODMAN=1` is set.

On some Linux distributions, FUSE may not be permitted for some users, or `podman_bootstrap.sh` and `native_bootstrap.sh` might not install it correctly. Investigate whether you can address your FUSE issues, or read the [Installing without FUSE](./advanced-podman-build.md#installing-without-fuse) section.

## Solving Compilation Problems

- Verify your Rust version (run `make env` and `cargo --version`, then `exit`): this shell uses the Rust compiler of the cross toolchain in the `prefix` directory. The cookbook itself is built with the nightly that `rust-toolchain.toml` pins, so the latest Rust nightly is not needed.

    - [rustup](https://www.rustup.rs) installs the pinned toolchain when it is missing. A native build needs it (`mk/depends.mk` stops without it). A Podman build also compiles the installer and RedoxFS tools with the `cargo` of the host (`mk/fstools.mk`), unless `FSTOOLS_IN_PODMAN=1` is set.

- Verify if your `make` and `nasm` are up-to-date.
- Verify if the build system is using the latest commit by running the `git branch -v` command.
- Verify if the recipe source is using the latest commit of the default branch, to do this run:

```sh
cd recipes/some-category/recipe-name/source
```

```sh
git branch -v
```

- Run `make clean pull fetch` to remove all your compiled binaries and update all sources.
- Sometimes a new commit briefly breaks the build: a git recipe without `rev` in its `recipe.toml` file follows the branch head of its repository, so any fetch can bring in a broken commit. The `target/<target>/source_info.toml` file in the recipe directory records the revision that was built.
- Sometimes both the source and the binary of some recipe is wrong, run `make ucr.recipe-name` and verify if it fix the problem.

#### Environment Leakage

Environment leakage is when some program or library is not fully cross-compiled to FerrionOS, thus its dependency chain has Linux references that don't work on FerrionOS.

It usually happens when the program or library get objects from outside the FerrionOS build system PATH.

- During a cook, the FerrionOS build system puts the `bin` directories of the cross toolchain, of the build system and of the recipe's `host:` dependencies in front of the PATH of the host system or of the Podman container, which provides the host build tools
- The program build system must use the host system build tools and the Cookbook recipe dependencies, not the host system libraries.
- The most common way to detect this is to install the `*-dev` dependency package equivalent to the program recipe dependency, for example:

The program named "my-program" needs to use the OpenSSL library, thus you add the `openssl3` recipe on the `recipe.toml` of the program, but the program don't detect the OpenSSL source code.

Then you install the `libssl-dev` package on your Ubuntu system and rebuild the program with the `make cr.my-program` command, then it finish the build process successfully.

But when you try to open the executable of the program inside of FerrionOS, it doesn't work. Because it contains Linux references.

To fix this problem you need to find where the program build system get the OpenSSL source code and patch it with `${COOKBOOK_SYSROOT}` environment variable (where the `openssl3` recipe contents were copied)

### Update Your Build System

Sometimes your build system can be outdated because you forgot to run `make pull` before other commands, read [this](./build-system-reference.md#update-the-build-system) section to learn the complete way to update the build system.

### Prevent and Fix Breaking Changes

Sometimes a commit changes the build system or a recipe in a breaking way. Read the repository's commit history before updating (run `git fetch` and `git log HEAD..origin/master`, or read the [commit list](https://github.com/CallMeOuta/FerrionOS/commits/master)) to know if a commit contains breaking changes. If one does, clean up your recipe or build system tooling binaries before you update the recipe or build system source, to avoid conflicts with the new configuration.

#### Build System Breakage Prevention

The following methods can prevent a build system breakage after updates that change file configuration behavior.

- Wipe all statically linked recipe binaries, update build system source and rebuild the system (most common prevention)

```sh
make static_clean pull rebuild
```

Or also clean the toolchain if needed:

```sh
make static_clean prefix_clean pull prefix rebuild
```

- Wipe all recipe binaries, update build system source and rebuild the system (second most common prevention)

```sh
make repo_clean pull rebuild
```

Or also clean the toolchain if needed:

```sh
make repo_clean prefix_clean pull prefix rebuild
```

- Wipe all recipe binaries, toolchain and Podman container, update build system source and rebuild the system

```sh
make clean container_clean pull all
```

- Wipe all recipe binaries/sources, update build system source and rebuild the system (least common prevention)

```sh
make distclean pull all
```

#### Build System Fixing

If the breaking change affect multiple recipes or any recipe can't be built, read the following instructions:

- Wipe the build system binaries and build the system (most common fix)

```sh
make clean all
```

Check if the compilation or runtime error continues after this command, if the error continues run the command below:

- Wipe and rebuild the filesystem tooling

```sh
make fstools_clean fstools
```

Check if the compilation or runtime error continues after this command, if the error continues run the command below:

- Wipe the Podman container (not common fix)

```sh
make container_clean
```

Check if the compilation or runtime error continues after this command, if the error continues it doesn't happen because of breaking changes on the build system.

#### Recipe Fixing

Some types of recipe errors can be backwards-incompatible build system, system component or relibc changes after the `make pull rebuild` command execution. Run the following tests to verify if the recipe error is an isolated problem or a breaking change:

- Rebuild the recipe binaries

```sh
make cr.recipe-name
```

Check if the compilation or runtime error continues after this command, if the error continues run the following command:

- Wipe the recipe sources and binaries and rebuild

```sh
make ucr.recipe-name
```

Check if the compilation or runtime error continues after this command, if the error continues run the following command:

- Reconfigure the cross toolchain and rebuild the recipe

```sh
rm -rf prefix
```

```sh
make prefix cr.recipe-name
```

Check if the compilation or runtime error continues after this command, if the error continues run the following command:

- Wipe all statically linked recipe binaries and rebuild the system (run this command if the binaries of multiple recipes are broken)

```sh
make static_clean rebuild
```

Or also clean the toolchain if needed:

```sh
make static_clean prefix_clean prefix rebuild
```

Check if the compilation or runtime error continues after this command, if the error continues run the following command:

- Wipe all recipe binaries and rebuild the system (run this command if the binaries of multiple recipes are broken)

```sh
make repo_clean rebuild
```

Check if the compilation or runtime error continues after this command, if the error continues run the following command:

- Wipe all build system binaries and rebuild the system (run this command if the binaries of multiple recipes are broken)

```sh
make clean all
```

Or clean the Podman container if needed:

```sh
make clean container_clean all
```

Check if the compilation or runtime error continues after this command, if the error continues run the following command:

- Wipe all recipe sources and build system binaries and rebuild the system (run this command if the sources and binaries of multiple recipes are broken)

```sh
make distclean all
```

Check if the compilation or runtime error continues after this command, if the error continues read the section below.

#### New Build System Copy

If the methods above do not work, download a new copy of the build system into another directory with the following commands:

```sh
git clone https://github.com/CallMeOuta/FerrionOS.git
```

```sh
cd FerrionOS
```

```sh
CI=1 COOKBOOK_LOGS=true make all
```

In the last command, `CI=1` turns off the terminal interface of the cookbook and `COOKBOOK_LOGS=true` keeps the output of each recipe in `build/logs/<target>/<recipe>.log`.

### Update Your Branch

If you are doing local changes on the build system, probably you left your branch active on the folder (instead of `master` branch).

New branches don't sync automatically with `master`, thus if the `master` branch receive new commits, you wouldn't use them because your branch is outdated.

To fix this, run:

```sh
git checkout master
```

```sh
git pull
```

```sh
git checkout your-branch
```

```sh
git merge master
```

Or

```sh
git checkout master
```

```sh
git pull
```

```sh
git merge your-branch master
```

### Update Crates

Sometimes a Rust program uses an old crate version lacking support for the `redox` target, read [this](./porting-applications.md#update-crates) section to learn how to update them.

### Verify The Dependency Tree

Some crates take a long time to do a new release (years in some cases), thus these releases will hold old versions of other crates, versions where support for the `redox` target is not available (causing errors during the program compilation).

The `redox_syscall` crate is the most affected by this, some crates hold a very old version of it and will require patches (`cargo update -p` alone doesn't work).

To identify which crates are using old versions of crates such as `redox_syscall` you will need to verify the dependency tree of the program, inside the program source directory, run:

```sh
cargo tree --target=x86_64-unknown-redox
```

(If you aren't building FerrionOS for x86_64 change `x86_64` in `x86_64-unknown-redox` to the CPU architecture that you are using)

This command will draw the dependency tree and you will need to find the crate name on the tree.

If you don't want to find it, you can use the `grep` tool with a pipe to see all crate versions used in the tree, sadly `grep` don't preserve the tree hierarchy, thus it's only useful to see versions and if some patched crate works (if the patched crate works all crate matches will report the most recent version).

To do this, run:

```sh
cargo tree --target=x86_64-unknown-redox | grep crate-name
```

## Debug Methods

### Boot

FerrionOS [boots quietly](./boot-process.md#quiet-boot) on UEFI: the screen shows a splash instead of the boot log. Press a key in the first second after power-on to get the bootloader's menu and the text log, or press a key during the splash to see the boot log. Kernel messages always go to the serial port. A BIOS boot shows the menu and the boot log without a key press.

If your boot hangs and the log don't show the reason, you can use the following environment variables to help:

- `BOOTSTRAP_LOG_LEVEL=value` : Bootstrap and process manager logging verbosity level
- `INIT_LOG_LEVEL=value` : Init logging verbosity level
- `DRIVER_LOG_LEVEL=value` : Logging verbosity level of all drivers
- `DRIVER_*_LOG_LEVEL=value` : Driver-specific logging verbosity level, for example: `DRIVER_PS2_LOG_LEVEL=value` for PS/2 logging and `DRIVER_USB_LOG_LEVEL=value` for USB logging
- `RELIBC_LOG_LEVEL=value` : Relibc logging verbosity level, you need to disable the `no_trace` feature flag by removing it from the `default` feature list in `Cargo.toml` of the relibc source (recipe `recipes/core/relibc`) and run the `make rp.relibc` (if the application is dynamically linked) or `make crp.relibc,app-name` (if the application is statically linked) command to use it. Run `make f.relibc` and `make lc.relibc` before the edit, so the cookbook builds your copy
- `INIT_SKIP=executable-name` : Skip the execution of executables with hangs or errors, commas are supported if you want to skip multiple executables

They accept the following values:

- `ERROR` value: Known event that is a fatal error but recoverable.
- `WARN` value: Unexpected event coming from unexpected condition.
- `INFO` value: Significant event mostly useful for developer.
- `DEBUG` value: Detailed event monitoring to show how the service is being used.
- `TRACE` value: Very verbose information which is only useful when debugging.

Once you determine what you need, press the `E` key in the bootloader's menu to open the boot environment editor, add the variables in the last lines and press Enter on an empty line to boot. The bootloader of a verified-boot image, which is the default, does not offer the editor: build the image with `make image VERIFIED_BOOT=0` to get it. For example:

```
default environment variables here
INIT_LOG_LEVEL=DEBUG
DRIVER_LOG_LEVEL=DEBUG
```

You can see an example output below:

```
2026-01-12T22-27-51.758Z [@ps2d::controller:468 WARN] ps2d: post-test unexpected value: 9C
2026-01-12T22-27-51.760Z [@ps2d::controller:337 ERROR] ps2d: keyboard failed to reset: 55
```

Press Shift+Up or Shift+Down to scroll the log, if nothing happens probably the input is not working.

To disable the environment variables after boot run the `export *_LOG_LEVEL=OFF` command, for example: the `export RELIBC_LOG_LEVEL=OFF` command will disable relibc logging.

### Applications

- Use the `LD_DEBUG=all app-name` environment variable and value to show all called dynamically linked libraries on application execution and in what library a dynamically linked application crashed.

- Disable the `no_trace` relibc feature flag (by removing it from the `default` feature list in `Cargo.toml` of the relibc source (recipe `recipes/core/relibc`) and run the `make rp.relibc` (if the application is dynamically linked) or `make crp.relibc,app-name` (if the application is statically linked) command to use it, as the [Boot](#boot) section describes) and use the `RELIBC_LOG_LEVEL=trace app-name` environment variable and value to possibly show where a application error and panic (like if you get the `EINVAL` error where it shouldn't be) or hang happens. It's mostly useful for hangs but can also be for crashes, depending on type.

  - If the application is waiting for the last file descriptor shown in relibc log, run `cat /scheme/sys/iostat` for more details.

- Run the `cat /scheme/sys/block` command to know in what kernel function makes the application blocked.

- If you see a unexpected system slowdown when running a application and the `ps` command log don't give a clue, run the `cat /scheme/sys/stat` command to show kernel statistics and know more details.

- Use the `dmesg` command to read the kernel and userspace daemons log.

- If Orbital hangs you need to verify if the system also froze by pressing Super+F1 to see the boot log or Super+F2 to switch to other `tty`, login as `root` and run `dmesg` to read the system log ("Super" is the key with Windows logo).

- You can start the QEMU with the `make qemu gpu=no` command to easily copy the terminal text.

- You can write to the `/scheme/debug` scheme, which will output on the console, but you must be the `root` user. This is useful if you are debugging a program where you need to use Orbital but still want to capture messages.

- Currently, the build system strips function names and other symbols from programs, as support for symbols is not implemented yet.

- To use GDB add the `gdbserver` recipe in your filesystem configuration, run the `make qemu gdb=yes` command in one shell, start the `gdbserver` program on QEMU and run the `make gdb` command in another shell.

- Use the following command for advanced logging:

```sh
make some-command 2>&1 | tee file-name.log
```

- Read [this](https://en.wikipedia.org/wiki/Debugging#Techniques) Wikipedia section to learn more/about debugging techniques.

### Symbols

You will see the available debug methods for recipes on this section.

- If you change the recipe build mode (`release` to `debug` or the opposite) while debugging, don't forget to rebuild with `make cr.recipe-name` because the build system may not detect the changes.

- `REPO_DEBUG` - This environment variable will build the recipe binaries with assertions, checking and symbols.

(Debugging with symbols inside of FerrionOS is not supported yet)

To enable them you can use the following commands or scripts:

- Permanently enable `REPO_DEBUG` for all recipes by adding the following text to your `.config` file:

```
REPO_DEBUG?=1
```

- Enable the `REPO_DEBUG` environment variable for one command, rebuild/package a recipe and add to the FerrionOS image:

```sh
REPO_DEBUG=1 make crp.recipe-name
```

- Enable the `REPO_DEBUG` environment variable for multiple commands, rebuild/package a recipe and add to the FerrionOS image:

```sh
export REPO_DEBUG=1
```

```sh
make crp.recipe-name
```

- Enable the `COOKBOOK_DEBUG` and `COOKBOOK_NOSTRIP` (they are equivalent to `REPO_DEBUG` environment variable) inside the `recipe.toml` :

```toml
template = "custom"
script = """
COOKBOOK_DEBUG=true
COOKBOOK_NOSTRIP=true
"""
```

#### Rust Backtrace

A backtrace helps you to detect bugs that happen with unexpected input parameters, and allow you to trace back through the callers to see where the bad data is coming from.

You can see how to use it below:

- Start QEMU with logging:

```sh
make qemu 2>&1 | tee file-name.log
```

- Enable this environment variable globally (on FerrionOS):

```sh
export RUST_BACKTRACE=full
```

- Run the program and repeat the bug (capturing a backtrace in the log file)
- Close QEMU
- Open the log file, copy the backtrace and paste in an empty text file

- Run the `backtrace.sh` script in the FerrionOS directory (on Linux):

```sh
scripts/backtrace.sh -r recipe-name -b your-backtrace.txt
```

It will print the file and line number for each entry in the backtrace.

(This is the most simple example command, use the `-h` option of the `backtrace.sh` script to see more combinations)

#### GDB On QEMU

Use the following instructions to debug a recipe with GDB:

- Build or rebuild the recipe with assertions/checking/symbols and install into the FerrionOS image:

```sh
make crp.recipe-name REPO_DEBUG=1
```

If you want to permanently enable debug binaries add the following environment variable to your `.config` file:

```
REPO_DEBUG?=1
```

- Build and install the GDB server into the FerrionOS image

```sh
make rp.gdbserver
```

- Start QEMU with the GDB configuration enabled:

```sh
make qemu kvm=no QEMU_SMP=1 gdb=yes
```

If the recipe has one executable, run the following command:

```sh
make debug.recipe-name
```

If the recipe has multiple executables use the following command:

```sh
make debug.recipe-name DEBUG_BIN=executable-name
```

## Kill A Frozen FerrionOS VM

Sometimes FerrionOS can freeze or rarely get a kernel panic, to kill the QEMU process run this command:

```sh
pkill qemu-system
```

## Kernel Panic

A kernel panic is when some bug prevents the safe execution of the kernel code, thus the system needs to be restarted to avoid memory corruption.

The kernel uses the following panic message format:

```
KERNEL PANIC: panicked at some-path/file-name.rs:line-number:character-position:
the panic description goes here
```

- You can use the following command to search it in a big log:

```sh
grep -nw "KERNEL PANIC" --include "file-name.log"
```

### QEMU

If you get a kernel panic in QEMU, copy the terminal text (the terminal that runs `make qemu` shows the serial port, where the kernel messages always go) or capture a screenshot and read the [Participating](./contributing.md) page to learn how to report it.

### Real Hardware

If you get a kernel panic in real hardware, capture a photo and read the [Participating](./contributing.md) page to learn how to report it. On UEFI a quiet boot keeps the kernel messages off the screen: press a key in the first second after power-on to boot with the text log, as the [Boot](#boot) section describes. The messages always go to the serial port.
