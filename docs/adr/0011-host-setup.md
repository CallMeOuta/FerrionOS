# How the bootstrap scripts, the containers and the Nix shell set up a host

Status: accepted

## Context

A host gets its build tools from `podman_bootstrap.sh` or `native_bootstrap.sh` (one function per package manager), from the Podman containers in `podman/`, or from the Nix flake ([`agents/skills/build-scripts.md`](../../agents/skills/build-scripts.md), "Host dependencies"). Package managers differ in how they report a missing package and in what they upgrade.

## Decision

### Bootstrap scripts

- The Arch function installs with `pacman -S --needed` and never upgrades the system: a script that only installs a few packages must not cause a system update. If the install fails, the user resolves it and runs the script again.
- The Debian and Ubuntu function tries the RISC-V QEMU package under its current name (Debian 13, Ubuntu 26) and then under the older one (Debian 12, Ubuntu 24).
- The openSUSE function checks every package with its own `zypper install --dry-run` first, because one `zypper install` of several packages succeeds when any one of them installs; the real install is then one command, so the user confirms once. The dry runs' output is hidden unless one fails, because zypper prints its confirmation prompt and a `y` even with `--non-interactive`, which reads as if the package had been installed.
- The Solus function calls `eopkg` without "already installed" checks, because eopkg does nothing for a package that is current.

### Containers

- `podman/redox-toolchain-containerfile` builds the Linux toolchain against an older glibc, so the binaries it produces run on older hosts.
- `podman/redox-toolchain.toml` lists packages that help a build server compile `host:` recipes found in many recipes' dev-dependencies and distribute them, such as Go or Zig. It does not replace the cross compilers of the prebuilt prefix: GCC, Rust, LLVM and Clang.

### The Nix flake

- The Podman shell includes `fuse-overlayfs`, a copy-on-write storage driver that is much faster than Podman's default `vfs`.
- The QEMU set includes the host architecture's QEMU beside the general package.
- The native shell's extra packages each serve a component: three for relibc, one each for fuser, netsurf, orbutils and prboom. Removing one breaks that component's native build.

## Consequences

Adding a tool means adding it to every package manager's function, the containers and the flake in one change, as the build-scripts guide lists.

## Where

- `podman_bootstrap.sh`: `archLinux`, `ubuntu`, `suse`, `solus`
- `podman/redox-toolchain-containerfile`, `podman/redox-toolchain.toml`
- `flake.nix`: the `podman` and `native` shells
