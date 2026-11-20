# Build scripts

Read this before changing `Makefile`, `mk/*.mk`, the bootstrap scripts, `scripts/` or `podman/`.

These files run on Linux, macOS, FreeBSD and on FerrionOS itself, inside the Podman container and outside it. The rules below are what the tree already does; the ones marked new apply to code you add.

## Shell

- A script starts with `#!/usr/bin/env bash`. Bash is not at `/bin/bash` on every host.
- Indent with four spaces, or with the file's own width where it differs.
- New: a script says what it does in a comment under the shebang, and prints its usage when called without the arguments it needs. Most of `scripts/` does, and `scripts/patch-tar.sh` is the model.
- The scripts in `scripts/` run from the repository root and find a recipe through `make find.<recipe>` or `target/release/repo find`.
- A message names the OS FerrionOS. A string compared with something outside this repository keeps its value: `is_os_redox` in `native_bootstrap.sh` compares `uname -s` with `Redox`, because that is what the kernel reports.
- Check an edit with `bash -n <script>`.

## Makefiles

- Recipe lines are indented with a tab.
- A target that needs the build tools has two bodies. Under `ifeq ($(PODMAN_BUILD),1)` it runs itself again in the container with `$(PODMAN_RUN) make $@`, and the `else` branch does the work. Copy the shape from a neighbour in `mk/repo.mk`.
- A variable that must reach the cookbook inside the container is added to `PODMAN_CONFIG` in `mk/podman.mk`.
- New: a build variable gets its default and a `##` comment in `mk/config.mk`, where users look for it.
- Host differences live in the `UNAME` block of `mk/config.mk` (`FUMOUNT`, `FIND`, `VBM` and others). Add to that block and use the variable.
- A download goes to `$@.partial` and is renamed when complete, and a pinned one checks its sha256: `ch_download` in `mk/ch.mk`.
- Check an edit with `make -n <target>`, minding the caveat in [`docs/testing.md`](../../docs/testing.md).

## Host dependencies

A tool the build needs on the host is declared once per package manager. Adding or renaming one touches all of these:

- `native_bootstrap.sh`: every distribution function (`osx_macports`, `osx_homebrew`, `freebsd`, `archLinux`, `ubuntu`, `fedora`, `suse`, `gentoo`, `solus`, `redox`).
- `podman/redox-base-containerfile`, the image Podman builds run in, and `podman/redox-toolchain-containerfile` when the toolchain build needs the tool too.
- `flake.nix`, in the `native` shell.
- `mk/depends.mk`, when make should refuse to start without it.

`podman_bootstrap.sh` installs only what the host needs around the container: Podman, FUSE, git, make and an emulator. The versions of `just` and `cbindgen` are pinned in both `native_bootstrap.sh` (`cargoInstall`) and `podman/rustinstall.sh`: change them together.

## Bootstrap scripts in this checkout

Without `-d`, both bootstrap scripts clone upstream Redox into a new `redox/` directory and then delete themselves. Run them here as `./native_bootstrap.sh -d` or `./podman_bootstrap.sh -d`, which installs the dependencies and stops.
