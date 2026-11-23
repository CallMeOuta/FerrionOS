# Components and Their Sources

The FerrionOS repository is a build system. It does not contain the code of the kernel, the drivers or the other system components: a recipe for each component fetches its source when the component is built. Most of these sources are upstream's repositories.

## Where a Component Comes From

Each component has a recipe directory, `recipes/<category>/<name>/`, with a `recipe.toml`. Its `[source]` section names the repository, and `rev` pins a revision where one is set. A recipe without `rev` follows the head of the repository's branch. After `make f.<name>`, the source lies in the recipe's `source/` directory.

| Component | Recipe | What it is |
|---|---|---|
| Kernel | `recipes/core/kernel` | The microkernel |
| base | `recipes/core/base` | Essential system services and drivers: `init`, the drivers, the network stack, the audio daemon and others |
| relibc | `recipes/core/relibc` | The C standard library, written in Rust |
| RedoxFS | `recipes/core/redoxfs` | The default filesystem |
| Bootloader | `recipes/core/bootloader` | The boot loader for BIOS and UEFI |
| Installer | `recipes/core/installer` | Builds a bootable image; the build system uses it to assemble `harddrive.img` |
| Ion | `recipes/core/ion` | A shell written for this system |
| userutils | `recipes/core/userutils` | `login`, `sudo`, `passwd`, `usermod` and the other tools for users and groups |
| coreutils | `recipes/core/coreutils` | Core utilities that are specific to this system, such as `free`, `ps`, `shutdown` and `which` |
| extrautils | `recipes/core/extrautils` | Further utilities, such as `dmesg` and `less` |
| binutils | `recipes/core/binutils` | Utilities for working with binary files |
| pkgutils | `recipes/core/pkgutils` | The package manager, `pkg` |
| pkgar | `recipes/core/pkgar` | The package format and its tools |
| Orbital | `recipes/gui/orbital` | The display server and window manager |
| orbdata, orbutils, orbterm | `recipes/gui/` | Orbital's resources, utilities and terminal emulator |
| liborbital | `recipes/libs/liborbital` | C bindings to the Orbital client library |
| Redoxer | `recipes/dev/redoxer` | Upstream's tool that builds and runs a program for the target in a virtual machine |

Third-party projects that the system relies on have recipes too. Examples are [uutils/coreutils](https://github.com/uutils/coreutils), a Rust rewrite of the GNU core utilities (`recipes/core/uutils`), and [smoltcp](https://github.com/smoltcp-rs/smoltcp), the TCP/IP stack that the network stack in `base` uses.

The repository's [`docs/upstream.md`](https://github.com/CallMeOuta/FerrionOS/blob/master/docs/upstream.md) lists everything that is still fetched from upstream's repositories and servers, at build time and inside a running system.

## FerrionOS's Changes

FerrionOS carries its changes to a component as patch files beside the recipe, named `ferrion-*.patch` and listed in `[source] patches`. The cookbook applies them to the fetched source. Today `base`, `bootloader`, `kernel` and `userutils` carry such patches, for the quiet boot and for the system's name.

`config/base.toml` sets these four recipes to the `source` rule. They are therefore built from source also when `REPO_BINARY=1` asks for prebuilt packages, because a prebuilt package is upstream's build and lacks the patches.

[Coding and Building](./coding-and-building.md) explains how to change a component's source in your own checkout.
