# Self-hosted Development

> 📝 **Note:** FerrionOS inherits what this page describes from upstream and has not verified it yet.

This page explains how to configure the build system or FerrionOS for development on FerrionOS itself.

> 📝 **Note:** This page is incomplete: the toolchain still has open issues when it runs on FerrionOS itself, see [Caveats](#caveats).

## Bootstrapping with QEMU

This method is used if you are doing self-hosted development in QEMU from an existing build system outside of FerrionOS (e.g., Linux).

The build system is meant to come into the image through a recipe called `cookbook` (`recipes/other/cookbook/recipe.toml`). This recipe fetches upstream's build system, not this repository: its `[source]` section names upstream's repository, and the package itself holds only upstream's `native_bootstrap.sh`, installed as `/usr/bin/redox_bootstrap`. The `cookbook` directory that the steps below use is therefore not installed by the recipe as it is today. The package dependencies of the recipe are similar to what is shown in [Native build](./building-redox.md).

Add the `cookbook` recipe to your FerrionOS image and start the image in QEMU:

```sh
make rp.cookbook qemu
```

### Inside FerrionOS in QEMU

Inside QEMU, open a terminal, then go to the cookbook directory:

```sh
cd cookbook
```

Build the toolchain. On a FerrionOS host, `make prefix` downloads the `libgcc` and `libstdcxx` packages from upstream's package server and cooks relibc (`mk/prefix.mk`):

```sh
make prefix
```

Build the core packages from source:

```sh
make r.sys
```

Build and install your system changes:

```sh
make r.recipe-name
```

```sh
sudo make i.recipe-name
```


Or (if you changed multiple recipes)

```sh
make r.sys
```

```sh
sudo make i.sys
```

Or install all packages defined from config:

```sh
sudo make install
```

If the process is successful, you can reboot the system to load your changes:

```sh
sudo shutdown -r
```

## Bootstrapping from Real Hardware

You can follow the [Native Build](./building-redox.md) instructions to start a self-hosted build system.

## Caveats

In this section, we explain existing problems of using self-hosted development and a possible workaround for it.

### No recovery point

FerrionOS currently has no mechanism for recovering itself if a build broke the system. Keep a copy of a working image before you start, because FerrionOS publishes no images to download.

Having a secondary Linux OS in the same system can ease recovering the system from an unbootable position. See [RedoxFS: Working with Linux Partition](./redoxfs.md#working-with-linux-partition) for more details.

### No GCC "Cross compiler" mode

GCC that is built for Linux is configured as "Cross compiler" mode, whereas GCC that is built to run on FerrionOS is configured as "Hosted compiler". The difference between these two modes affects which `relibc` variant is used during compilation, and in this case the GCC uses the `relibc` from the system installed at `/usr` to do the compilation, instead of the expected place in the `prefix` directory.

There is no way to change the mode to "Cross compiler" without rebuilding, so the easiest way to fix it is by using Clang as the default compiler. Add this config to `.config` to change that:

```sh
export REDOXER_USE_CLANG=1
```

