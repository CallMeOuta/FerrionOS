# FerrionOS

This repository is the build system for FerrionOS. FerrionOS began as a fork of Redox OS and is developed independently; it is not affiliated with or endorsed by the Redox OS project.

FerrionOS is an open-source operating system written in Rust, a language with focus on safety, efficiency and high performance. FerrionOS uses a microkernel architecture, and aims to be reliable, secure, usable, correct, and free. Its kernel and system design draw on seL4, MINIX, Plan 9, Linux and BSD; its everyday experience draws on macOS and Windows.

FerrionOS _is not_ just a kernel: it is a **full-featured operating system**, providing components (file system, display server, core utilities, etc.) that together make up a functional and convenient operating system. It provides source code compatibility with many Rust, Linux and BSD programs. The desktop currently ships the COSMIC terminal, file manager and text editor; which desktop apps FerrionOS ships is not settled and may change.

The build system is makefiles (`Makefile`, `mk/`), a Rust build tool called the cookbook (`src/`), package recipes (`recipes/`) and filesystem configs (`config/`). They fetch, cross-compile and package the system's components and assemble a bootable image.

## Quick start

On a Linux host:

```sh
./podman_bootstrap.sh -d   # install Podman, FUSE, git, make and QEMU
make all                   # build the desktop image for the host's architecture
make qemu                  # boot it in QEMU
```

`./native_bootstrap.sh -d` installs the tools for a build without Podman instead; set `PODMAN_BUILD=0` in `.config` to use it. `CONFIG_NAME=minimal` builds a small image without a GUI. Always pass `-d` in this checkout: without it, the bootstrap scripts clone the upstream build system into a new directory and delete themselves.

A first `make all` builds the container, the toolchain and every package of the image, and needs 50 GB of storage or more. The dev images log in as `user` with an empty password, or as `root` with the password `password`; use them only on a test machine.

## Documentation

- [The FerrionOS Book](book/src/SUMMARY.md), the user documentation
- [Contributing](CONTRIBUTING.md)
- [Working on this repository](AGENTS.md): task guides in `agents/skills/`, reference in `docs/`
- [How the build works](docs/build-system.md) and [how to check a change](docs/testing.md)
- [What FerrionOS boots on today](docs/vm-baseline.md)

## Licence

FerrionOS's build system is released under the MIT licence: see [LICENSE](LICENSE). Components fetched at build time carry their own licences.
