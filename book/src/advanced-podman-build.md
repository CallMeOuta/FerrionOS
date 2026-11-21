# Advanced Podman Build

To make the FerrionOS build process more consistent across platforms, **Rootless Podman** is used for major parts of the build.

Before reading through this section, make sure you have already read:
- [Podman Build](./podman-build.md)
- [Build System Reference](./build-system-reference.md)

This chapter provides a detailed discussion, including tips, tricks and troubleshooting, as well as some extra detail for those who might want to leverage or improve FerrionOS's use of Podman.

## Build Environment

- Only the variables that `PODMAN_CONFIG` in `mk/podman.mk` lists are passed from the environment and the `make` command line to the part of `make` that is done in **Podman**: `ARCH`, `BOARD`, `CONFIG_NAME`, `FILESYSTEM_CONFIG`, `PREFIX_BINARY`, `CI`, `COOKBOOK_MAKE_JOBS`, `COOKBOOK_LOGS`, `COOKBOOK_COMPRESSED`, `COOKBOOK_VERBOSE`, `COOKBOOK_VERBOSE_CMD`, `COOKBOOK_CLEAN_TARGET`, `REPO_APPSTREAM`, `REPO_BINARY`, `REPO_NONSTOP`, `REPO_OFFLINE`, `TESTBIN`, `HOSTED_REDOX`, `PREFIX_USE_UPSTREAM_RUST_COMPILER`, `VERIFIED_BOOT`, `VERIFIED_BOOT_KEYS` and `VERIFIED_BOOT_SIGNER`. You must set any other variable that the build in the container reads, e.g. `REPO_DEBUG` or `COOKBOOK_CLEAN_BUILD`, in [.config](./configuration-settings.md#config) and not on the command line or in your environment.

- If you are building your own software to add in FerrionOS, and you need to install additional packages using `apt-get` for the build, follow the [Adding Packages to the Build](#adding-packages-to-the-build) section.

## Installation

Most of the packages required for the build are installed in the container as part of the build process. However, some packages need to be installed on the host operating system. You may also need to install an emulator such as **QEMU**. For most Linux distributions, this is done for you by running `./podman_bootstrap.sh -d` in the FerrionOS directory.

If you can't use the `podman_bootstrap.sh` script, you need to install at least:

- Podman 4.0 or later
- Rust
- libfuse 3.x (to build an image)
- QEMU (to run the image)
- virtiofsd (optional, to share directories with FerrionOS, see [VirtIO-FS Shared Directories](./virtio-fs.md))
- passt (optional, for the default network of `make ch`, see [Cloud Hypervisor](./cloud-hypervisor.md))

You can attempt to install the necessary packages below. The package lists are inherited from upstream, and FerrionOS has not verified them.

### Pop!_OS/Ubuntu/Debian

```sh
sudo apt-get install git make curl podman fuse fuse-overlayfs slirp4netns qemu-system-x86 qemu-kvm qemu-system-arm qemu-system-riscv
```

> ⚠️ **Warning:** Ubuntu 22.04 ships with old Podman (3.x version), which will have issues. The official [Podman installation guide](https://podman.io/docs/installation#ubuntu) requires Ubuntu 20.10 and newer to operate. If you do use the 22.04 version or older, please read [Gory Details](#gory-details).

### Arch Linux

```sh
sudo pacman -S --needed git make curl podman fuse3 fuse-overlayfs slirp4netns qemu-system-x86 qemu-system-arm qemu-system-riscv
```

### Fedora

```sh
sudo dnf install git-all make curl podman fuse3 fuse-overlayfs slirp4netns qemu-system-x86 qemu-kvm qemu-system-arm qemu-system-riscv
```

### OpenSUSE

```sh
sudo zypper install git make curl podman fuse fuse-overlayfs slirp4netns
```

### FreeBSD

```sh
sudo pkg install git gmake curl fusefs-libs3 podman
```

### MacOS

FerrionOS has not verified a build on MacOS. The build files carry upstream's code for MacOS hosts, which upstream describes as experimental: even with Podman, a [clock skew](https://github.com/containers/podman/issues/26185) breaks the Makefile caching mechanism.

An alternative is to install QEMU, VirtualBox or [UTM](https://mac.getutm.app/), create an ARM64 or x86-64 Linux virtual machine, and follow the Podman or Native Build in it.

To build on MacOS itself, read [Installing without FUSE](#installing-without-fuse) first: installing FUSE requires turning off [SIP](https://en.wikipedia.org/wiki/System_Integrity_Protection) on Apple Silicon-based MacOS.

- Homebrew

```sh
sudo brew install git make curl osxfuse podman fuse-overlayfs slirp4netns
```

- MacPorts

```sh
sudo port install git gmake curl osxfuse podman
```

### NixOS

Before building FerrionOS on NixOS, you must have configured Podman on your system. Just follow the instructions of the [NixOS wiki](https://nixos.wiki/wiki/Podman):

```nix
{ pkgs, ... }:
{
  # Enable common container config files in /etc/containers
  virtualisation.containers.enable = true;
  virtualisation = {
    podman = {
      enable = true;

      # Create a `docker` alias for podman, to use it as a drop-in replacement
      dockerCompat = true;

      # Required for containers under podman-compose to be able to talk to each other.
      defaultNetwork.settings.dns_enabled = true;
    };
  };

  # Useful other development tools
  environment.systemPackages = with pkgs; [
    dive # look into docker image layers
    podman-tui # status of containers in the terminal
    docker-compose # start group of containers for dev
    podman-compose # start group of containers for dev
  ];
}

```

You will then have to configure your user to be able to use Podman:
```nix
users.extraUsers.${user-name} = {
  subUidRanges = [{ startUid = 100000; count = 65536; }];
  subGidRanges = [{ startGid = 100000; count = 65536; }];
};
```

The last step is to activate the development shell:
```sh
nix develop --no-warn-dirty --command $SHELL
```


## `build/container.tag`

The building of the **image** is controlled by the *tag* file `build/container.tag`. If you run `make all` with `PODMAN_BUILD=1`, which is the default, the file `build/container.tag` will be created after the image is built. This file tells `make` that it can skip the image update after the first time.

Many targets in the Makefiles `mk/*.mk` include `build/container.tag` as a dependency. If the *tag* file is missing, building any of those targets may trigger an image to be created, which can take some time.

If only the *tag* file is missing, and you are confident that your **image** is correct and that the container's home directory `build/podman` in this working directory is valid, you can save a few minutes with

```sh
make container_touch
```

This will create the file `build/container.tag` without rebuilding the image. However, it will fail if the image does not exist. If it fails, just do a normal `make`, it will create the container when needed. A new working directory has no `build/podman` yet: let `make` build the container there, which also installs Rust into `build/podman`.

## Installing without FUSE

If installing FUSE is difficult for your operating system, you can avoid installing it by adding `FSTOOLS_IN_PODMAN=1` in the `.config` file. It makes the installer run inside Podman. If you do set it, then you can avoid installing FUSE and Rust altogether in your host system.

An additional environment variable (`FSTOOLS_NO_MOUNT`) can also be set to not use FUSE during image creation, however image creation relies on correct files permission bit and ownership which means it only can be used within Podman. This configuration can be used as a last resort if FUSE is not working at all. The signing for [verified boot](./boot-process.md#verified-boot) mounts the image through FUSE, so a build with `FSTOOLS_NO_MOUNT=1` also needs `VERIFIED_BOOT=0`; without it the build stops with a message that says so.

## Cleaning Up

To remove the **base image**, any lingering containers, the container's home directory `build/podman`, including the **Rust** installation, and `build/container.tag`, run:

```sh
make container_clean
```

- To verify that everything has been removed

```sh
podman ps -a
```

- Show any remaining images or containers

```sh
podman images
```

- Remove **all** images and containers. You still may need to remove `build/container.tag` if you did not do `make container_clean`.

```sh
podman system reset
```

**Note:**

- `make clean` does **not** run `make container_clean` and will **not** remove the container image.
- If you already did `make container_clean`, `make clean` cannot run the Cookbook: it only removes `repo`, `build/$ARCH/$CONFIG_NAME`, `prefix/$TARGET` and the binaries of the Cookbook and of the host tools, and leaves the `target` directories of the recipes in place.

## Debugging Your Build Process

If you are developing your own components and wish to do one-time debugging to determine what library you are missing in the **Podman Build** environment, the following instructions can help. Note that your changes will not be persistent. After debugging, **you must** [Add your Libraries to the Build](#adding-packages-to-the-build). With `PODMAN_BUILD=1`, run the following command:

```sh
make container_shell
```

- Within that environment, you can build the FerrionOS components with:

```sh
make repo
```

- If you need another `ARCH` or `CONFIG_NAME`, leave the shell and start it again with them, for example as below. The shell gets `ARCH`, `CONFIG_NAME` and `FILESYSTEM_CONFIG` from the `make` that started it, so changing only one of them inside the shell leaves the others at their old values.

```sh
make container_shell ARCH=aarch64 CONFIG_NAME=server
```

**Note**: Your changes will not persist once the shell has been exited, because the container is discarded.

Type `exit` once you have determined how to solve your problem.

## Adding Packages to the Build

> 📝 **Note:** A build tool that a recipe needs can also be cooked for the build machine by listing it in the recipe with the `host:recipe-name` syntax, which needs no change to the container.

This method can be used if you want to make changes/testing inside the Debian container with `make env`.

The default **Containerfile**, `podman/redox-base-containerfile`, installs the packages for a normal FerrionOS build.

However, you cannot easily add packages after the **base image** is created. You must add them to your own Containerfile and rebuild the container image.

Copy `podman/redox-base-containerfile` to a file whose name starts with `my_`, which Git ignores, and add to the list of packages in the initial `apt-get`.

```sh
cp podman/redox-base-containerfile podman/my_containerfile
```

```sh
nano podman/my_containerfile
```

```
...
        zip \
        zstd \
        MY_PACKAGE \
    && if [ "$(uname -m)" = "x86_64" ]; then \
...
```

Make sure you include the continuation character `\` at the end of the line, because the command continues after the list of packages.


Then, edit [.config](./configuration-settings.md#config), and change the variable `CONTAINERFILE` to point to your Containerfile, e.g.

```
CONTAINERFILE?=podman/my_containerfile
```

If your Containerfile is newer than `build/container.tag`, a new **image** will be created. You can force the image to be rebuilt with `make container_clean`.

If you feel the need to have more than one image, you can set the variable `IMAGE_TAG` (`redox-base` by default, see `mk/podman.mk`) in `.config` to give the image a different name.

If you just want to install the packages temporarily, run `make container_shell`, use `apt-get install` in that shell and build from the same shell. Each `make` command starts a new container from the image, so packages installed in one container are in no other one, and they are gone when the shell exits.

## Summary of Podman-related Make Targets, Variables and Podman Commands

- `PODMAN_BUILD?` - If set to 1 in [.config](./configuration-settings.md#config), or in the environment, or on the `make` command line, much of the build process takes place in **Podman**. It is 1 by default (`mk/config.mk`).

- `CONTAINERFILE`-  The name of the containerfile used to build the image. This file includes the `apt-get` command that installs all the necessary packages into the image. If you need to add packages to the build, edit your own containerfile and change this variable to point to it.

- `make build/container.tag` - If no container image has been built, build one. It's not necessary to do this, it will be done when needed.

- `make container_touch` - If a container image already exists and the container's home directory `build/podman` is valid, but there is no *tag* file, create the *tag* file so a new image is not built.

- `make container_clean` - Remove the container image, the container's home directory `build/podman` and the *tag* file.

- `make container_shell` - Start an interactive Podman `bash` shell in the same environment used by `make`; for debugging the `apt-get` commands used during image build.

- `make env` - Start an interactive `bash` shell with the `prefix` tools in your PATH. Automatically determines if this should be a Podman shell or a host shell, depending on the value of `PODMAN_BUILD`.

- `make repo` - Used while in a Podman shell to build all the FerrionOS component packages. `make all` will not complete successfully, since part of the build process must take place on the host.

- `podman system reset` - Use this command when `make container_clean` is not sufficient to solve problems caused by errors in the container image. It will remove all images, use with caution. If you are using **Podman** for any other purpose, those images will be deleted as well.

## Gory Details

If you are interested in how your working directory is used for builds in **Podman**, the following configuration details may be interesting.

The build process is performed in your normal working directory, e.g., `~/FerrionOS`. Compilation of the FerrionOS components is performed in the container, but the final FerrionOS image (`build/$ARCH/$CONFIG_NAME/harddrive.img` or `build/$ARCH/$CONFIG_NAME/redox-live.iso`) is constructed using [FUSE](https://github.com/libfuse/libfuse) running directly on your host machine.

Podman is enabled by default: `mk/config.mk` sets `PODMAN_BUILD?=1`. Setting `PODMAN_BUILD` to another value in [.config](./configuration-settings.md#config), on the `make` command line (e.g., `make PODMAN_BUILD=0 all`) or in the environment (e.g., `export PODMAN_BUILD=0; make all`) will disable Podman.

First, a **base image** called `redox-base` will be constructed from the Containerfile, with all the necessary packages for the build system. The layer cache for it is pulled from upstream's registry (`PODMAN_CACHE_PATH` in `mk/podman.mk`), unless `PODMAN_CACHE_PULL` is set to 0. A "home" directory will also be created in `build/podman`. It is mounted as `/root` in the container, because the build runs as the container's `root` user: the Containerfile creates no other user. It will contain the Rust installation made by `podman/rustinstall.sh` (`rustup`, `sccache`, `just` and `cbindgen`). This takes some time, but is only done when necessary. The *tag* file [build/container.tag](./advanced-podman-build.md#buildcontainertag) is also created at this time to prevent unnecessary image builds.

Then, various `make` commands are executed in **containers** built from the **base image**. The files are constructed in your working directory tree, just as they would for a non-Podman build. In fact, if all necessary packages are installed on your host system, you can switch Podman on and off relatively seamlessly, although there is no benefit of doing so.

The build needs Podman 4.0 or later. With Podman 3.x and earlier, the user mapping of rootless containers often broke with [tar](https://github.com/containers/podman/issues/14655) and [buildah](https://github.com/containers/buildah/issues/1818), and the `--userns keep-id` option was needed as a workaround. `PODMAN_OPTIONS` in `mk/podman.mk` no longer has that option.

For Ubuntu 22.04 there's a temporary fix by [manually updating crun](https://github.com/microsoft/vscode-remote-release/issues/11042#issuecomment-3044713731).

The working directory is made available in the container by **mounting** it as a **volume**. The **Podman** options (`PODMAN_VOLUMES` in `mk/podman.mk`):

```sh
--volume $(ROOT):$(CONTAINER_WORKDIR)$(PODMAN_VOLUME_FLAG) --volume $(PODMAN_HOME):/root$(PODMAN_VOLUME_FLAG)
```

take the directory that `make` was started in (`ROOT`) as the host working directory, and **mount** it at the location `$CONTAINER_WORKDIR`, normally set to `/mnt/redox`. The `--workdir` option makes the same location the working directory in the container. The second option mounts `build/podman` (`PODMAN_HOME`) as `/root`. `PODMAN_VOLUME_FLAG` is `:Z`, unless `USE_SELINUX` is set to another value than 1. The `:Z` at the end of the name indicates that the mounted directory should not be shared between simultaneous container instances. It is optional on some Linux distros, and not optional on others.

The `PATH` environment variable is set as an option to `podman run` (`PODMAN_ENV`). This is to avoid the need for the `make` command in the container to run `.bashrc`, which would add extra complexity. The variables listed in [Build Environment](#build-environment) are passed in the environment to allow you to override the values in `mk/config.mk` or `.config`, e.g. by setting them on your `make` command line. A value in `.config` is overridden this way only if it is assigned with `?=`.

`PODMAN_BUILD=0` is also set in the container's environment, to ensure that the instance of `make` running in the container knows not to invoke **Podman**. This overrides `PODMAN_BUILD?=1` in `.config`, which is why that line needs the question mark.

The **Containerfile** uses a single `RUN` command, as **Podman** commits the image after each command, and no `ENTRYPOINT`, so that the `podman run` command can be given as a list of arguments, rather than just a string to be processed by the entrypoint shell.

Containers in the build process are run with `--rm` to ensure the container is discarded after each use. This prevents a proliferation of used containers. However, when you use `make container_clean`, you may notice multiple items being deleted. These are the partial images created as each `RUN` command is executed while building.

`PODMAN_OPTIONS` in `mk/podman.mk` also has `--interactive --tty`, so a Podman build is started from a terminal, as well as `--cap-add SYS_ADMIN`, `--device /dev/fuse`, `--network=host` and `--pids-limit=-1`.

Container images and container data is normally stored in the directory `$HOME/.local/share/containers/storage`. The following command removes that directory in its entirety. However, the contents of any **volume** are left alone:

```sh
podman system reset
```
