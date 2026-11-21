# Package Manager

> 📝 **Note:** FerrionOS inherits what this page describes from upstream and has not verified it yet.

`pkg` (recipe `recipes/core/pkgutils`) is the package manager of FerrionOS: it installs binary packages to a running system. If you want to build packages, or include binary packages during the build, please see the [Including Programs in FerrionOS](./including-programs.md) page.

An image's package source is upstream's package server (`/etc/pkg.d/50_redox`, written by `config/base.toml`), so `pkg` installs upstream's builds; FerrionOS has no package server.

Due to limited device support, you may get better results in an virtual machine than on real hardware.

The most commonly used `pkg` commands are shown below:

- Install a package:

  ```sh
  sudo pkg install <package-name>
  ```

- Remove a package:

  ```sh
  sudo pkg remove <package-name>
  ```

- Update all installed packages:

  ```sh
  sudo pkg update
  ```

- Search for a package:

  ```sh
  pkg search <package-name>
  ```

- Show information about a package:

  ```sh
  pkg info <package-name>
  ```

- List the installed packages:

  ```sh
  pkg list
  ```

- Get detailed information about one of the above options:

  ```sh
  pkg help <pkg-command>
  ```

> 📝 **Note:** Some `pkg` commands must be run with `sudo` because they change system folders, such as `/usr/bin`, and the records of the installed packages.
