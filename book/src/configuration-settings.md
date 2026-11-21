# Configuration Settings

The FerrionOS build system applies configuration settings from various places to determine the final FerrionOS image. Most of these settings ultimately come from the build system's [environment variables](#environment-variables) (or similarly-named Make variables) and the contents of the chosen [filesystem configuration](#filesystem-configuration) file.

- [Environment Variables](#environment-variables)
  - [.config](#config)
  - [Cookbook Environment Variables](#cookbook-environment-variables)
    - [cookbook.toml](#cookbooktoml)
  - [Changing the QEMU Properties](#changing-the-qemu-properties)
  - [Command Line](#command-line)
  - [mk/config.mk](#mkconfigmk)
    - [build.sh](#buildsh)
- [Filesystem Configuration](#filesystem-configuration)
  - [CPU Architecture Codenames](#cpu-architecture-codenames)
  - [Filesystem Size](#filesystem-size)
  - [Filesystem Customization](#filesystem-customization)
    - [Creating a custom filesystem configuration](#creating-a-custom-filesystem-configuration)
    - [Adding a package to the filesystem configuration](#adding-a-package-to-the-filesystem-configuration)
  - [Binary Recipe Mode](#binary-recipe-mode)
    - [REPO_BINARY](#repo_binary)
  - [Local Recipe Changes](#local-recipe-changes)
  - [Ignored Recipes](#ignored-recipes)
- [Advanced Cookbook Options](#advanced-cookbook-options)
  - [Cookbook TUI Mode](#cookbook-tui-mode)
  - [Cookbook Offline Mode](#cookbook-offline-mode)
  - [Cookbook Logs](#cookbook-logs)
  - [Cookbook Verbosity](#cookbook-verbosity)
  - [Cookbook Compressed Packages](#cookbook-compressed-packages)
  - [Cookbook Clean Build](#cookbook-clean-build)
  - [Cookbook Clean Target](#cookbook-clean-target)
  - [Cookbook Web Generator](#cookbook-web-generator)

## Environment Variables

The default values for the build system's Makefile variables are mostly defined in the `mk` directory&mdash;particularly in [`mk/config.mk`](#mkconfigmk). Local changes from the default values, however, should be applied in the [`.config`](#config) file, or temporarily on the `make` [command line](#command-line).

While if you are using Cookbook environment variables in `.config` you need to use the `export` command.

The build system uses GNU Make and Cookbook to coordinate the build system in order. The [build system reference](./build-system-reference.md#cookbook) and [porting application guide](./porting-applications.md#cookbook) have more information about Cookbook.

Three important Makefile variables of interest are `ARCH`, `CONFIG_NAME`, and `BOARD`, as they specify the system to be built. These, and other important environment variables, can be seen in the following table. The `mk/config.mk` file defines every build variable with its default and a comment, and it is right where this table disagrees.

| Variable | Definition |
|:---------|:-----------|
|`ARCH`| Specifies the [CPU architecture](#cpu-architecture-codenames) that the system is to be built for: `x86_64`, `i586`, `aarch64` or `riscv64gc`. The default value is the CPU architecture of the host, as `uname -m` prints it. |
| `CONFIG_NAME` | Determines the name of the filesystem configuration, and is normally used to construct the `FILESYSTEM_CONFIG` name (the `desktop` variant is used by default). |
| `BOARD` | For [supported QEMU single board computers](https://www.qemu.org/docs/master/system/targets.html) such as Raspberry Pi 3B+ (`raspi3bp`) that require special configuration. Defaults to empty. |
| `FILESYSTEM_CONFIG` | Determines the filesystem configuration file location. See the [Filesystem Configuration](#filesystem-configuration) section below. The default value is `config/$ARCH/$CONFIG_NAME.toml` if that file exists and `config/$CONFIG_NAME.toml` otherwise, or `config/$ARCH/$BOARD/$CONFIG_NAME.toml` if `$BOARD` is set, but this can be changed if the desired configuration file is in a different location. |
| `QEMU_MEM` | Sets the QEMU RAM memory quantity, e.g., `QEMU_MEM=2048`. |
| `QEMU_SMP` | Sets the QEMU CPU core quantity, e.g.,  `QEMU_SMP=4`. |
| `PREFIX_BINARY` | If set to 0 (`PREFIX_BINARY=0`), the build system will build the cross toolchain from source and will not download upstream's prebuilt toolchain from upstream's server (the address is in `mk/prefix.mk`). The default value is 1. |
| `PREFIX_USE_UPSTREAM_RUST_COMPILER` | If set to 1 (`PREFIX_USE_UPSTREAM_RUST_COMPILER=1`) the build system will download the official nightly binaries of the Rust project instead of building the Rust compiler from the `rust` recipe (only used with `PREFIX_BINARY=0`). The comment in `mk/config.mk` calls it experimental and limits it to the Tier 2 targets of Rust. |
| `export COOKBOOK_GIT_TREELESS` | If set to `true` it will enable treeless Git source clone in all recipes to reduce testing time and save storage |
| `REPO_BINARY` | If set to 1 (`REPO_BINARY=1`), the build system will download/install upstream's pre-compiled packages from upstream's package server by default, rather than build them from source (i.e., recipes). The `base`, `bootloader`, `kernel` and `userutils` packages are still built from source, see the [REPO_BINARY](#repo_binary) section. |
| `REPO_OFFLINE` | Enable the offline mode of Cookbook where recipe sources will not be updated and download the smallest amount of data from the Internet as possible. |
| `REPO_NONSTOP` | Enable the nonstop mode of Cookbook where recipe build failure will not stop the compilation of other recipes. |
| `FILESYSTEM_SIZE` | The size in MiB of the filesystem contained in the final FerrionOS image. See the [Filesystem Size](#filesystem-size) section before changing it. |
| `REDOXFS_MKFS_FLAGS` | Flags to `redoxfs-mkfs`, the program that creates a RedoxFS filesystem, used when `$(BUILD)/filesystem.img` is built (`mk/disk.mk`). The `harddrive.img` and `redox-live.iso` images are created by the installer and do not use them. With the `--encrypt` option, `redoxfs-mkfs` asks for a password and creates an encrypted filesystem. |
| `VERIFIED_BOOT` | If set to 1, which is the default (0 for `i586`), `harddrive.img` is built as a verified-boot image: the build signs its kernel and `initfs`, and its UEFI bootloader starts no other ([Verified Boot](./boot-process.md#verified-boot)). The signing needs FUSE, OpenSSL 3 and Python 3 on the host, and it stops the build when `FSTOOLS_NO_MOUNT=1` is set. Set it to 0 (`VERIFIED_BOOT=0`) for an image whose bootloader verifies nothing. Live images are not covered. |
| `VERIFIED_BOOT_KEYS` | The directory of the verified-boot development key pair, which the first build creates. The default value is `build/verified-boot`. |
| `VERIFIED_BOOT_SIGNER` | A PKCS#11 URI of an Ed25519 key on a hardware token, which then signs the image instead of the development key. Empty by default; `scripts/verified-boot-sign.sh` describes it. |
| `PODMAN_BUILD` | If set to 0 (`PODMAN_BUILD?=0`), the build system will use the build environment from your Linux distribution or Unix-like system instead of Podman, see the [Native Build](./building-redox.md) page for more information. The default value is 1, a Podman build. If used in the `.config` file it *must* include the `?` symbol in the end of the variable name to work correctly (`PODMAN_BUILD?=1`) |
| `SCCACHE_BUILD` | If set to 1 it will enable the [sccache](https://github.com/mozilla/sccache) object cache for Native Build. See [sccache in Native Build](./advanced-build.md#sccache) page for more information.
| `PODMAN_CACHE_PULL` | If set to 1, which is the default, the build of the Podman container image pulls a layer cache from upstream's registry (`PODMAN_CACHE_PATH` in `mk/podman.mk`) instead of running every step of the Containerfile. Set it to 0 if the cache is broken |
| `FSTOOLS_IN_PODMAN` | If set to 1 (`FSTOOLS_IN_PODMAN=1`), the build system will build the installer inside Podman to avoid FUSE in the host system. See the [Installing without FUSE](./advanced-podman-build.md#installing-without-fuse) page for more information. |
| `FSTOOLS_NO_MOUNT` | If set to 1 (`FSTOOLS_NO_MOUNT=1`), the installer will not use FUSE to create images. See the [Installing without FUSE](./advanced-podman-build.md#installing-without-fuse) section for more information. |
| `CONTAINERFILE` | The Podman container configuration file. See the [Podman Build](./podman-build.md) page for more information. |
| `COOKBOOK_MAKE_JOBS` | The number of maximum CPU cores used when building recipes, default is using all CPU cores from `nproc`. |
| `CI` | If set to any non-empty value (`CI=1`), the Cookbook will not activate its TUI, and it fetches and cooks the recipes one after the other instead of in parallel. |

The FerrionOS image that is built is named `build/$ARCH/$CONFIG_NAME/harddrive.img`, and the live image that `make live` builds is named `build/$ARCH/$CONFIG_NAME/redox-live.iso`.

### .config

The purpose of the `.config` file is to allow default configuration settings to be changed without explicitly setting those changes in every `make` command (or modifying the contents of the `mk` directory). The file is also included in the `.gitignore` list to ensure it won't be committed by accident.

To permanently override the settings in the [`mk/config.mk`](#mkconfigmk) section, add a `.config` file to the FerrionOS directory (i.e., where `make` commands are run) and set the overriding values in that file. `mk/config.mk` includes the file as a makefile, so it uses the syntax of `make`: `VARIABLE?=value` for a build variable, and `export VARIABLE=value` for an environment variable.

For example, the following configuration specifies the `desktop-minimal` image variant will be built for the `i586` CPU architecture. These settings will be applied implicitly to all subsequent `make` commands:

```
ARCH?=i586
CONFIG_NAME?=desktop-minimal
```

> 📝 **Note:** `PODMAN_BUILD?=1` *must* include the question mark to work correctly

> 📝 **Note:** Comments are supported using the `#` character

> 📝 **Note:** Any QEMU option can be inserted

> 📝 **Note:** The bootstrap scripts do not write the `.config` file when they are run with `-d` (`./podman_bootstrap.sh -d`), so the file exists only after you create it. Without it, the default values of `mk/config.mk` apply, a Podman build among them.

> 💡 **Tip:** When adding environment variables in the `.config` file you can add the `?` symbol at the end of variable names to allow the variable value to be overridden on the command line shell environment.

### Cookbook Environment Variables

Cookbook (the build system) has environment variables that can be tuned for more customization. To set these environment variables, write `export KEY=VALUE` to `.config`. Note that these are environment, not Makefile variables, so the `export` keyword is necessary.

Each Cookbook configuration defaults to environment variables. The list that counts is `REPO_HELP_STR` in `src/bin/repo/main.rs`, and the default values are set in `src/config.rs`. A `COOKBOOK_*` variable that takes `true` or `false` needs exactly one of these words: any other value, such as `1`, leaves the default value in place.

| Environment Variable    | How to use the variable      | Description                            |
|:------------------------|:-----------------------------|:---------------------------------------|
| `CI`                    | `CI=1`, `CI=`                | Disables TUI                           |
| `COOKBOOK_MAKE_JOBS`    | `COOKBOOK_MAKE_JOBS=4`       | Override build job count               |
| `COOKBOOK_LOGS`         | `COOKBOOK_LOGS=true`         | Override whether to save logs          |
| `COOKBOOK_GIT_TREELESS` | `COOKBOOK_GIT_TREELESS=true` | Globally enable treeless Git clone     |
| `COOKBOOK_OFFLINE`      | `COOKBOOK_OFFLINE=true`      | Override offline behaviour (see notes) |
| `COOKBOOK_VERBOSE`      | `COOKBOOK_VERBOSE=false`     | Override verbosity                     |
| `COOKBOOK_VERBOSE_CMD`  | `COOKBOOK_VERBOSE_CMD=true`  | Trace the commands of build scripts    |
| `COOKBOOK_NONSTOP`      | `COOKBOOK_NONSTOP=true`      | Override nonstop (see notes)           |
| `COOKBOOK_CLEAN_TARGET` | `COOKBOOK_CLEAN_TARGET=true` | Clean target directory after build     |
| `COOKBOOK_CLEAN_BUILD`  | `COOKBOOK_CLEAN_BUILD=true`  | Clean build directory before build     |
| `COOKBOOK_WRITE_FILETREE` | `COOKBOOK_WRITE_FILETREE=true` | Write the `stage.files` file tree |
| `COOKBOOK_COMPRESSED`   | `COOKBOOK_COMPRESSED=true`   | Override package compression           |
| `COOKBOOK_WEB`          | `COOKBOOK_WEB=true`          | Override web content generation        |
| `REDOXER_USE_CLANG`     | `REDOXER_USE_CLANG=true`     | Use Clang as the default compiler      |
| `REDOXER_USE_LTO`       | `REDOXER_USE_LTO=true`       | Enable LTO compilation by default      |

> 📝 **Note:** Use `REPO_OFFLINE=1` and `REPO_NONSTOP=1` to set these options instead of `export COOKBOOK_OFFLINE=true` and `export COOKBOOK_NONSTOP=true`: `mk/config.mk` turns them into the Cookbook variables, and a Podman build passes them on to the container.
> 💡 **Tip:** Running Cookbook with `CI=1 COOKBOOK_LOGS=true COOKBOOK_VERBOSE=false` will hide successful build logs in the terminal

#### `cookbook.toml`

Cookbook also has a configuration file, `cookbook.toml` in the FerrionOS directory (it is ignored by `git`). This file is rarely used because `.config` should be sufficient. Any configuration in this file will override configuration from `.config` or environment variables. The `cookbook.toml` configuration below can be used as a template:

The `cookbook.toml` file mainly configures Cookbook options (`[cook]`) and mirrors (`[mirrors]`). The keys of `[cook]` are the fields of `CookConfigOpt` in `src/config.rs`. Mirrors are used to replace the main code and binary source URLs used in Cookbook recipes, useful for a quick way to use alternative sources when the main server is offline or slow. A mirror key is the beginning of a URL without `https://`, and the longest key that matches a URL wins.


```toml
# These options have defaults set below
#[cook]
#jobs = <nproc>
#nonstop = false
#offline = false
#tui = true # false if the CI variable is set
#logs = true # the same value as tui
#verbose = true
#verbose_cmd = false
#compressed = false
#git_treeless = false
#clean_build = false
#clean_target = false
#write_filetree = false

[mirrors]
# The uncommented option below is the default if [mirrors] is not set
# see the list of GNU FTP mirrors at: https://www.gnu.org/prep/ftp.en.html
"ftp.gnu.org/gnu" = "mirrors.ocf.berkeley.edu/gnu"
# "github.com/foo/bar" = "github.com/baz/bar"
```

> 💡 **Tip:** Mirrors option can also be used to override the address of upstream's package server, the source of the [pre-compiled packages](#repo_binary) (`REMOTE_PKG_SOURCE` in `src/lib.rs`).

### Changing the QEMU Properties

To change the CPU core and RAM memory quantities used when running the FerrionOS image in QEMU, add the following environment variables to your `.config` file:

```
QEMU_SMP?=<number-of-cpu-threads>
QEMU_MEM?=<number-of-memory-in-mib>
```

There is also an option to customize the QEMU machine and CPU type, which is influenced by how `ARCH` and `BOARD` is set. `QEMU_CPU` only applies with `kvm=no`, because with KVM the CPU type is `host`. Read the QEMU documentation for more information:

```
QEMU_MACHINE?=<qemu-machine>
QEMU_CPU?=<qemu-cpu-type>
```

There are more options that can be customized. They are `make` variables defined in `mk/qemu.mk`, and that file is the source of truth for them:

```
audio?=<no|ac97|hda>
disk?=<ata|nvme|usb|virtio|cdrom|sdcard>
net?=<no|e1000|rtl8139|virtio|usb-net|redir>
netboot?=<yes|no>
kvm?=<yes|no>
bridge?=<host-bridge>
gpu?=<no|vga|ramfb|multi|virtio|virtio-sdl|virtio-gl>
uefi?=<yes|no>
gdb?=<yes|nonblock|no>
live?=<yes|no>
usb?=<yes|no>
serial?=<yes|no>
iommu?=<yes|no>
qemu_serial_logfile?=<file>
share?=<directory|tag=directory ...|redox>[:ro]
share_tag?=<tag>
share_sockdir?=<directory>
QEMUFLAGS?=<extra-qemu-flags>
```

- `audio`: `no` removes the sound device, `ac97` selects AC'97, and any other value selects Intel HDA.
- `net`: `no` removes the network card. `redir` uses the e1000 card and forwards the host ports 8022 and 8080 to the ports 22 and 80 of FerrionOS.
- `netboot=yes` starts iPXE (`/usr/lib/ipxe/ipxe-amd64.efi` on the host), with the build directory served over TFTP and `redox.ipxe` as the boot file.
- `bridge` attaches the network card to the host bridge that it names.
- `uefi=no` boots without the UEFI firmware, and `i586` always does.
- `live=yes` boots `redox-live.iso` instead of `harddrive.img`.
- `usb=no` leaves out the xHCI USB controller.
- `serial=no` leaves out the serial port and connects the debug console (I/O port 0x402) to the terminal instead.
- `iommu=yes` attaches an emulated Intel IOMMU (VT-d) with interrupt remapping to the virtual machine. It also splits the interrupt controller between KVM and QEMU, because interrupt remapping does not work with the controller that KVM emulates by default. It works on x86-64 only, where the machine is `q35`. FerrionOS does not use the IOMMU yet.
- `gdb=yes` starts the GDB server of QEMU and halts the virtual machine until a debugger connects, and `gdb=nonblock` does not halt it.
- `qemu_serial_logfile` names a file that also receives the serial console.
- `QEMUFLAGS` holds extra QEMU flags. Set it in `.config` or in the environment: your value replaces the default value, and the makefile appends its own flags to it. On the `make` command line, `QEMUFLAGS=` replaces every flag that the makefile adds.

The default values depend on `ARCH`, and `mk/qemu.mk` fixes some options for some CPU architectures.

The `share` options share host directories with FerrionOS using VirtIO-FS, read the [VirtIO-FS Shared Directories](./virtio-fs.md) page for more information. They are used by both `make qemu` and `make ch`, the options of Cloud Hypervisor are described in the [Cloud Hypervisor](./cloud-hypervisor.md) page.

### Command Line

The default settings in `mk/config.mk` can be overridden by explicitly setting them on the `make` command line.

For example, the following command builds the `demo` image variant and loads it into QEMU:

```sh
make CONFIG_NAME=demo qemu
```

Some environment variables can also be set for the lifetime of the current shell by setting them at the command line:

```sh
export ARCH=i586; make all
```

Overriding settings in this way is only temporary, however. Additionally, a [Podman Build](./podman-build.md) passes only these variables from the command line or the environment on to the container (`PODMAN_CONFIG` in `mk/podman.mk`): `ARCH`, `BOARD`, `CONFIG_NAME`, `FILESYSTEM_CONFIG`, `PREFIX_BINARY`, `CI`, `COOKBOOK_MAKE_JOBS`, `COOKBOOK_LOGS`, `COOKBOOK_COMPRESSED`, `COOKBOOK_VERBOSE`, `COOKBOOK_VERBOSE_CMD`, `COOKBOOK_CLEAN_TARGET`, `REPO_APPSTREAM`, `REPO_BINARY`, `REPO_NONSTOP`, `REPO_OFFLINE`, `TESTBIN`, `HOSTED_REDOX`, `PREFIX_USE_UPSTREAM_RUST_COMPILER`, `VERIFIED_BOOT`, `VERIFIED_BOOT_KEYS` and `VERIFIED_BOOT_SIGNER`. Every other setting that the build in the container reads must be set in [`.config`](#config), which is read in the container too.

### mk/config.mk

The FerrionOS build system uses several Makefiles, most of which are in the `mk` directory. Most settings of interest have been grouped together in `mk/config.mk`.

Feel free to open `mk/config.mk` in your favorite editor and have a look through it; just be sure not to apply any changes:

```sh
nano mk/config.mk
```

The `mk/config.mk` file should never be modified directly, as doing so could create conflicts in the `make pull` command, which runs `git pull`.

To apply lasting changes to environment variables, please refer to the [`.config`](#config) section. To apply changes only *temporarily*, see the [Command Line](#command-line) section.

#### build.sh

The `build.sh` script exports `ARCH`, `CONFIG_NAME` and `FILESYSTEM_CONFIG` to the environment and then runs `make` with the remaining arguments. It is inherited from upstream and has not followed the changes of `mk/config.mk`, as described below, so pass `ARCH=` and `CONFIG_NAME=` to `make` instead:

```sh
make ARCH=x86_64 CONFIG_NAME=server qemu
```

The script is called this way:

```sh
./build.sh [-a <ARCH>] [-c <CONFIG_NAME>] [-f <FILESYSTEM_CONFIG>] <TARGET> ...
```

The `TARGET` parameter may be any valid `make` target. Additional variable settings may also be included, such as `gpu=no`

| Option | Description |
|:-------|:------------|
| `-a <ARCH>` | The CPU architecture you are building for, `x86_64`, `i586`, `aarch64` or `riscv64gc`. The options `-X`, `-A` and `-6` can be used as shorthands for `-a x86_64`, `-a aarch64` and `-a i586`, respectively. The usage text of the script also lists `-5` and `-R`, but its option parser (`getopts ":c:f:a:dhXA6"`) rejects them. |
| `-c <CONFIG_NAME>` | The name of the filesystem configuration which appears in the name of the image being built. |
| `-f <FILESYSTEM_CONFIG>` | Determines the filesystem configuration file location, which can be in any location but is normally in directory `config/$ARCH`.<br><br>📝 **Note:** If you *do* specify `-f <FILESYSTEM_CONFIG>`, the file path determines the other values. `CONFIG_NAME` is always the *basename* of the file without `.toml`, also if `-c` is given. Without `-a`, `ARCH` is the name of the directory that holds the file, and the script does not check it: for a file outside of `config/$ARCH`, specify `-a ARCH`. |

Without `-f`, the script sets `FILESYSTEM_CONFIG` to `config/$ARCH/$CONFIG_NAME.toml`. It has no fallback to `config/$CONFIG_NAME.toml`, so the build fails for a configuration that exists only at the top level of the `config` directory, such as `server` or `minimal`.

The default values of the script for `ARCH` and `CONFIG_NAME` are `x86_64` and `desktop`, respectively, while `make` itself takes the CPU architecture of the host as the default.

## Filesystem Configuration

The packages to be included in the final FerrionOS image are determined by the chosen **filesystem configuration** file, which is a `.toml` file (e.g., `config/x86_64/desktop.toml`). Open `desktop.toml` and have a look through it:

```sh
nano config/x86_64/desktop.toml
```

The configurations at the top level of the `config` directory, such as `desktop`, `server` and `minimal`, can be built for every CPU architecture. A directory named after a CPU architecture holds the configurations that exist only for it, such as `demo`, or that replace a shared one for it, such as `config/x86_64/desktop.toml`. Feel free to browse the `config` directory for more examples.

For more details on the filesystem configuration, and how to add additional packages to the build image, please see the [Including Programs in FerrionOS](./including-programs.md) page.

Feel free to create your own **filesystem configuration**.

### CPU Architecture Codenames

The FerrionOS build system can cross-compile to other CPU architectures. The CPU architecture that FerrionOS is built for (specified by the `ARCH` environment variable) usually determines the filesystem configuration file that will be used by the build system.

The build system knows the CPU architectures below, and `ARCH` takes exactly the name in the first column. FerrionOS has verified an image only for `x86_64`: the others are inherited from upstream and not verified.

| CPU Architecture | Other Aliases |
|:---------------- |:----------- |
| `i586` | x86 (32-bit), IA32 , `x86` |
| `x86_64` | x86 (64-bit), `x86-64`, `amd64`, `x64` |
| `aarch64` | ARM (64-bit), ARMv8, `ARM64` |
| `riscv64gc` | RISC-V (64-bit) |

The filesystem configurations for a given CPU architecture can be found in the [`config`](https://github.com/CallMeOuta/FerrionOS/tree/master/config) folder's correspondingly named sub-directory (e.g. `config/x86_64`).

### Filesystem Size

The filesystem size is the total amount of storage space allocated for the filesystem that is built into the image, including all programs. It is specified in mebibytes (MiB). The `server` configuration uses 512 and the `desktop` configuration 650, and some configs (e.g., `demo`) are larger. The filesystem must be large enough to accommodate the packages included in the filesystem. For the live image (`redox-live.iso`), the filesystem must not exceed the size of your system's RAM, and must also leave room for the package's installation and system execution.

The filesystem size is normally set from the **filesystem configuration** file, e.g. `config/x86_64/demo.toml`.

```toml
[general]
...
filesystem_size = 3000
...
```

To change this, it is recommended that you create your own filesystem configuration and apply changes there. However, this can be temporarily overridden on the `make` command line, e.g.:

```sh
make FILESYSTEM_SIZE=512 image qemu
```

> ⚠️ **Warning:** setting the `filesystem_size` value too low will produce an error resembling the following:
>
> ```
> thread 'main' panicked at src/lib.rs:94:53:
> called `Result::unwrap()` on an `Err` value: Error(Path("/tmp/redox_installer_759506/include/openssl/.pkgar.srtp.h"), State { next_error: Some(Os { code: 28, kind: StorageFull, message: "No space left on device" }), backtrace: InternalBacktrace { backtrace: None } })
> ```

### Filesystem Customization

The FerrionOS image can be customized with your own filesystem configuration file. Name it `config/my_<name>.toml`: `git` ignores the files whose names start with `my_`. Do not copy an existing configuration: include it, and write only what differs. The files in the `config` directory do the same, for example `config/x86_64/desktop.toml` includes `../desktop.toml`.

#### Creating a custom filesystem configuration

The following items describe the process for creating a custom filesystem configuration file (`config/my_desktop.toml`):

1. Create the `config/my_desktop.toml` file with the following content, which includes an existing filesystem configuration and changes one setting:

    ```toml
    include = ["desktop.toml"]

    [general]
    filesystem_size = 1024
    ```

2. Add the following to the `.config` file to set the new configuration as the build system's default:

    ```
    CONFIG_NAME?=my_desktop
    ```

The paths in `include` are relative to the file that includes. The `desktop.toml` file above is `config/desktop.toml`, the desktop configuration shared by all CPU architectures. On an x86_64 host the default image is built from `config/x86_64/desktop.toml`, which includes the shared one and adds the fish, zsh and Nushell shells: to start from it, write `include = ["x86_64/desktop.toml"]`.

The included configurations are merged in the order of the list, and your file is merged last:

- The `[general]` section is merged key by key, and the later value wins.
- The `[packages]` section is merged by package name. Writing `name = "ignore"` drops a package that an included configuration brings.
- The `[[files]]` entries are appended. An entry with the same `path` as an earlier one replaces that file in the image.
- A `[users.<name>]` or `[groups.<name>]` table replaces the earlier table of the same name as a whole.

Many filesystem configuration settings can be adjusted. See the templates in the `config` folder for reference.

> 💡 **Tip:** The `.gitignore` file ignores every file whose name starts with `my_`. Be sure to follow this convention for all custom filesystem configurations to avoid accidentally committing them.

#### Adding a package to the filesystem configuration

In the following example, the `acid` package is added to the `my_desktop.toml` configuration:

1. Open the `config/my_desktop.toml` file:

    ```sh
    nano config/my_desktop.toml
    ```

2. Add the `acid` package to the `[packages]` section:

    ```toml
    [packages]
    acid = {}
    ```

3. Build the `acid` package and update the FerrionOS image:

    ```sh
    make rp.acid
    ```

The `acid` package is now included in your FerrionOS image.

### Binary Recipe Mode

By default, the FerrionOS build system builds all packages from source (i.e., recipes). If you want to use upstream's pre-built packages, however, there is a recipe rule and a build variable for it. A pre-built package is upstream's build of upstream's source, downloaded from upstream's package server (`REMOTE_PKG_SOURCE` in `src/lib.rs`). FerrionOS publishes no packages.

This is useful for some purposes, such as producing development builds, checking a package of upstream's package server, and reducing image build time with large programs.

- This mode will use the pre-built packages of recipe dependencies if compilation is needed
- If a pre-built package of the recipe is not available, it will fallback to source compilation

You can learn how to enable this mode in one recipe below:

- Temporary pre-built recipe package

The following command will change the recipe rule to `binary` and install to an existing FerrionOS image, the package will be removed once a new FerrionOS image is created. The rule stays in the `cookbook.lock` file until the `make cc.recipe-name` command clears it.

```sh
make bcrp.recipe-name
```

- Permanent pre-built recipe package

1. Open the `config/my_config.toml` file:

    ```sh
    nano config/my_config.toml
    ```

2. Add the binary package below the `[packages]` section:

    ```toml
    [packages]
    ...
    recipe-name = "binary"
    ...
    ```

3. Download and add the binary package on your FerrionOS image:

    ```sh
    make rp.recipe-name
    ```

4. Open QEMU to verify your package:

    ```sh
    make qemu
    ```

#### REPO_BINARY

In the previous example, the build system's default behavior was overridden by explicitly setting a package to use a pre-built binary. To configure the build system to download pre-built packages by *default*, however, we can set the `REPO_BINARY` environment variable (`REPO_BINARY?=1`).

When `REPO_BINARY` is enabled, the FerrionOS image is made to use pre-built binaries for all packages assigned to `{}`; when `REPO_BINARY` is *disabled*, however, those same packages are compiled from source (i.e., recipes). A package assigned to `"source"` is always compiled: `config/base.toml` does this for `base`, `bootloader`, `kernel` and `userutils`, because FerrionOS carries patches for them and upstream's pre-built packages lack those patches.

(This environment variable has the same source compilation behavior of `recipe = "binary"` seen above)

For example:

```toml
[packages]
...
recipe-name1 = {} # use the REPO_BINARY setting ("source" if 0; "binary" if 1)
recipe-name2 = "binary" # pre-built package
recipe-name3 = "source" # source-based recipe
...
```

If you just want to use pre-built packages in one filesystem configuration, assign `"binary"` to its packages, or pass `REPO_BINARY=1` on the `make` command line when you build it. A filesystem configuration has no setting that turns `REPO_BINARY` on: its `[general]` section has no such key (`src/config/general.rs` in the `installer` source).

You can also use the following command to quickly enable source compilation in one recipe:

```sh
make sc.recipe-name
```

### Local Recipe Changes

By default every time a recipe build is triggered, Cookbook will update the recipe source. Cookbook will check the tarball BLAKE3 hash from the recipe configuration (`recipe.toml`), or pull from the `origin` remote when the recipe source is a Git repository. This will also remove local changes that are not saved in a branch.

To preserve and use local changes and skip updating the source for a specific recipe, set the recipe rule to `local` before the first change. Under this rule, Cookbook builds what is in the `source` directory of the recipe, and neither fetches nor patches it. The steps are:

1. Fetch the source of the recipe:

    ```sh
    make f.recipe-name
    ```

2. Set the recipe rule to `local`:

    ```sh
    make lc.recipe-name
    ```

3. Change the files in the `source` directory of the recipe, then cook the recipe:

    ```sh
    make r.recipe-name
    ```

4. Push the package into the image (QEMU must not be running):

    ```sh
    make p.recipe-name
    ```

5. Clear the recipe rule when the work is done:

    ```sh
    make cc.recipe-name
    ```

The recipe rule can also be set in the filesystem configuration:

```toml
[packages]
...
package-name = "local"
...
```

The `local` rule does not protect the `source` directory from the commands that delete it: `make u.recipe-name` and the other recipe targets with `u` among their letters, `make unfetch` and `make distclean`.

A change in the `source` directory exists only in your working directory. It lasts only as a patch file beside the `recipe.toml` file of the recipe, listed in `patches` of its `[source]` section.

### Ignored Recipes

You can disable the installation of some recipes in the FerrionOS image by ignoring, this is commonly used to disable recipes imported from other filesystem configurations to yours using the `include` data type.

To quickly ignore them use the following command:

```sh
make nc.recipe-name
```

You can also do this on your filesystem configuration:

```toml
[packages]
recipe-name = "ignore"
```

## Advanced Cookbook Options

This section has details of the [Cookbook Environment Variables](#cookbook-environment-variables) options.

### Cookbook TUI Mode

Cookbook has a TUI mode which is enabled by default. This mode speedup the build process because it allows the source fetch and compilation to be done in parallel. When the `CI` environment variable is set the TUI mode will be disabled, which is desired for builds under Continuous Integration (CI). The TUI needs a terminal and waits at a prompt when a recipe fails, so a long or unattended build is run with `CI=1`.

### Cookbook Offline Mode

Cookbook also has a mode where it will reduce Internet activity by adding `REPO_OFFLINE=1` into `.config`. This mode is useful when you are in places where the Internet connection is slow or absent, or when you want a fixed build system state or faster incremental compilation.

In this mode, Cookbook will not update the source of any recipe or a package binary if also set with `REPO_BINARY=1`. It also adds the `--offline` option to some Cargo build methods inside recipes where it is supported. When Cookbook or Cargo must access the Internet because sources do not exist locally it will throw an error instead.

To temporarily allow Cookbook and Cargo to have Internet activity and update sources, run `make f.package-name` (single recipe source fetch) or `make fetch` (to fetch all enabled recipe sources), as these commands will ignore the `REPO_OFFLINE` environment variable.

### Cookbook Logs

When `export COOKBOOK_LOGS=true` is set, all build logs will be written into the `build/logs/$TARGET` directory, one file for each recipe. The default value is the TUI setting (`src/config.rs`): logs are enabled when TUI mode is enabled, because build logs are captured anyway, and disabled with `CI=1`, unless `COOKBOOK_LOGS=true` is set as well. When logs are enabled it can alter the build script and maybe hide some essential progress bar. The convention for a long build is to set both, as in `CI=1 COOKBOOK_LOGS=true make all`.

Under the hood when the TUI mode or log capture is enabled, Cookbook will try to provide pseudo-terminal support and pipe them as-is to the terminal, then strip colors before saving them to logs.

### Cookbook Verbosity

When `export COOKBOOK_VERBOSE=false` is set, it disables certain debug logs, and if combined with `CI=1 COOKBOOK_LOGS=true`, it will hide success build logs to the terminal. The `set -x` command of GNU Bash in the build scripts is a separate setting, `COOKBOOK_VERBOSE_CMD=true`, which is disabled by default (`src/config.rs`).

The combination `CI=1 COOKBOOK_LOGS=true COOKBOOK_VERBOSE=false` creates a minimally distracting build output on terminal, and it will not replace the build log of a recipe that is cached instead of built.

## Cookbook Compressed Packages

When `export COOKBOOK_COMPRESSED=true` is set, it will package software with compression turned on. Under the hood it's compressing with the [LZMA2](https://en.wikipedia.org/wiki/LZMA) algorithm which is quick to decompress but slow to compress. 

It is not enabled by default because it uses considerable CPU time to compress and storage saving is negligible compared to [Cookbook Clean Target](#cookbook-clean-target).

## Cookbook Clean Build

When `export COOKBOOK_CLEAN_BUILD=true` is set, it will always remove [the build directory](./build-system-reference.md#cookbook) before building. This is intended for build servers where build consistency matters and avoid a build failure due to a stale `build` directory. It has the side effect of disabling incremental compilation thus generally avoided for local development.

## Cookbook Clean Target

When `export COOKBOOK_CLEAN_TARGET=true` is set, it will remove [the build, stage, and sysroot directories](./build-system-reference.md#cookbook) after a build success. This is intended to save storage space for local development that frequently jumps between recipes.

Unlike `COOKBOOK_CLEAN_BUILD`, it allows inspection of `build` directory and allows incremental compilation if build is failing. It does not allow visualization of the `build` and `stage` directories but creates `stage.files` which contains tree of files in the `stage` directory before deletion.

## Cookbook Web Generator

When `export COOKBOOK_WEB=true` is set, it will create a `web` directory containing HTML files for published `repo`. It contains much more information including the published file tree and dependents, intended for easy browsing.
