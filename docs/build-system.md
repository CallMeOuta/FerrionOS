# Build system

How a `make` run turns recipes and a filesystem config into a bootable FerrionOS image: the stages, what each leaves on disk, where settings come from, and how recipe targets are named. Task procedure lives in [`agents/skills/`](../agents/skills/). The variables and targets themselves are documented where they are defined: `mk/config.mk`, and the comment above each target in `Makefile` and `mk/*.mk`.

## Stages

`make all` runs these in order the first time. A stage is skipped once its product exists.

| Stage | Product | Made by |
|---|---|---|
| Container (Podman builds only) | `build/container.tag`, image `redox-base` | `mk/podman.mk`, from `podman/redox-base-containerfile` and `podman/rustinstall.sh` |
| Cookbook | `build/fstools.tag`, `target/release/repo` and its sibling binaries | `cargo build --release` of this repository (`mk/fstools.mk`) |
| Cross toolchain, the "prefix" | `prefix/<target>/sysroot` | `mk/prefix.mk`: downloaded when `PREFIX_BINARY=1`, built from source otherwise |
| Host tools | `build/fstools/bin/redox_installer`, `redoxfs`, `redoxfs-mkfs` | `mk/fstools.mk`, compiled for the host from `recipes/core/installer/source` and `recipes/core/redoxfs/source` |
| Cook | `build/<arch>/<config>/repo.tag`, packages in `repo/<target>/` | `repo cook --filesystem=<config> --with-package-deps` (`mk/repo.mk`) |
| Image | `build/<arch>/<config>/harddrive.img` | `redox_installer -c <config>` (`mk/disk.mk`) |

`<target>` is the Rust target triple, such as `x86_64-unknown-redox`.

After the first build, `make all` and `make qemu` reuse what exists: they cook again only when the file named by `FILESYSTEM_CONFIG` or the prefix is newer than `repo.tag`. Configs pulled in through `include` are not tracked. `make rebuild` runs the cook again (unchanged recipes report `cached`) and makes a new image. `make image` makes a new image from the packages already in `repo/`.

## What a cook leaves in a recipe directory

`recipes/<category>/<name>/` holds `recipe.toml` and the files it names. The cookbook produces everything else there:

- `source/`: the fetched source (a git clone, an unpacked `source.tar`, or a symlink for `same_as`) with the recipe's patches applied.
- `source.tar`: the downloaded tarball of a `tar` source.
- `target/<target>/build/`: the build directory, where the build script runs.
- `target/<target>/sysroot/` and `toolchain/`: the recipe's build dependencies unpacked from their packages. `toolchain/` holds the `host:` ones.
- `target/<target>/stage/`: the files the package installs. `stage.pkgar` is the package and `stage.toml` its metadata. An optional package `<x>` gets `stage.<x>/`, `stage.<x>.pkgar` and `stage.<x>.toml`.
- `target/<target>/auto_deps.toml` and `source_info.toml`: the detected runtime dependencies, and the source revision that was built.

A recipe is cooked again when its `source/` or `recipe.toml` is newer than `stage.pkgar`, or when the package of a build dependency changed. Otherwise it reports `cached`. A change in the cookbook's own build scripts (`src/cook/script.rs`) or in the environment does not count: run `make c.<recipe>` first to see it.

## Where settings come from

- `mk/config.mk` defines every build variable with its default and a comment. `ARCH` defaults to the host's `uname -m`, `CONFIG_NAME` to `desktop`.
- `.config` in the repository root is included first by `mk/config.mk` and uses make syntax (`ARCH?=x86_64`). The repository is mounted into the container, so `.config` applies on the host and inside Podman alike.
- A variable given on the command line or in the environment applies on the host. It reaches the container only when `PODMAN_CONFIG` in `mk/podman.mk` forwards it.
- `cookbook.toml` in the root configures the cookbook: a `[cook]` table whose keys are the fields of `CookConfigOpt` in `src/config.rs`, and `[mirrors]`, URL prefix replacements for clones, tarball downloads and the package server. A `[cook]` key wins over its `COOKBOOK_*` environment variable; `REPO_HELP_STR` in `src/bin/repo/main.rs` lists those variables.
- `cookbook.lock` is written by the cookbook (rules and revision locks, below) and overrides recipes and configs.

## Which config is built

`FILESYSTEM_CONFIG` is `config/<ARCH>/<CONFIG_NAME>.toml` when that file exists and `config/<CONFIG_NAME>.toml` otherwise. With `BOARD` set it is `config/<ARCH>/<BOARD>/<CONFIG_NAME>.toml`. Products land in `build/<ARCH>/<CONFIG_NAME>/`, so several configs and architectures build side by side.

`build.sh` sets `FILESYSTEM_CONFIG=config/<ARCH>/<CONFIG_NAME>.toml` without that fallback, so it fails for the configs that exist only at the top level (`desktop`, `server`, `minimal`). Pass `ARCH=` and `CONFIG_NAME=` to `make` instead.

## Recipe rules

Every recipe is cooked under one rule:

| Rule | Meaning |
|---|---|
| `source` | Fetch the source and build it. The default. |
| `binary` | Download the prebuilt package from the package server. The default with `REPO_BINARY=1`; a recipe the server lacks falls back to `source`. |
| `local` | Build what is in `source/`, without fetching or patching it. |
| `ignore` | Neither build nor install it. The cook fails when a recipe being built needs it as a build dependency. |

In rising precedence a rule comes from the default, from a string value in a filesystem config (`orbterm = "ignore"`), and from `cookbook.lock`, which `make sc.<recipe>`, `bc.`, `lc.` and `nc.` write and `make cc.<recipe>` clears. `make repo-lock` records the checked-out revision of every git recipe in the same file; `make repo-unlock` removes those locks.

## Recipe target names

Targets in `mk/repo.mk` that act on recipes are `<letters>.<recipe>`. Each letter is one cookbook action, read left to right:

| Letters | Action |
|---|---|
| `f` | Fetch the source. |
| `r` | Cook: fetch, build and package. |
| `c` | Clean: delete the recipe's `target/`. |
| `u` | Unfetch: delete `source/` and `target/`. |
| `p` | Push the package into the image under `build/<arch>/<config>/`. |
| `i` | Install the package into `DESTDIR`, which defaults to `/`. Meant for a FerrionOS host: pass `DESTDIR=<directory>` anywhere else. |
| `b`, `s`, `l`, `n` before `c` | Set the rule to `binary`, `source`, `local` or `ignore`, then clean. |
| `cc` | Clear the rule. |
| `rt`, `pt` | Print what a cook or a push would cover, without doing it. |
| `find` | Print the recipe's directory. |

Only the combinations defined in `mk/repo.mk` exist (`cr`, `ucr`, `rp`, `crp`, `lcr` and so on). Several recipes are separated by commas (`make r.kernel,relibc`), and cookbook flags ride in the same list (`make r.foo,--with-package-deps`, `make c.--all`). The targets without a recipe (`make fetch`, `repo`, `push`, `repo-tree`, `image-tree`) cover the whole filesystem config.

## Hosts

The build runs on Linux, macOS, FreeBSD and on FerrionOS itself. The `UNAME` block of `mk/config.mk` holds the per-host differences.

- Linux with Podman is the default (`PODMAN_BUILD=1`). The container cooks. The host still compiles the installer and RedoxFS, mounts the image through FUSE and runs the emulator, unless `FSTOOLS_IN_PODMAN=1`.
- `PODMAN_BUILD=0` builds natively with the tools `native_bootstrap.sh` installs.
- On a FerrionOS host the kernel reports `Redox` to `uname`. Make then sets `HOSTED_REDOX=1` and turns Podman off.
