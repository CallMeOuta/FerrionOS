# Filesystem configs

Read this before changing what an image contains: any `config/**/*.toml`.

A filesystem config names the packages, files, users and size of one image. `make` picks it from `ARCH` and `CONFIG_NAME`: `config/<arch>/<name>.toml` when that file exists, `config/<name>.toml` otherwise ([`docs/build-system.md`](../../docs/build-system.md)).

## The format

The schema is `redox_installer::Config`, in upstream's installer crate and not in this repository: `src/config/` of `gitlab.redox-os.org/redox-os/installer` at the revision `Cargo.lock` pins, and after a first build also `recipes/core/installer/source/src/config/`. How it merges:

- `include = ["base.toml"]` takes paths relative to the including file. Included configs merge in list order, and the including file merges last.
- `[general]` merges key by key, the later file winning: `filesystem_size` in MiB, `efi_partition_size`, `prompt` and others.
- `[packages]` merges by name. `name = {}` installs the package with its package dependencies. `name = "<rule>"` also sets the recipe's rule (`source`, `binary`, `local`, `ignore`). `"ignore"` is how a config drops a package that an included config or a meta-package brings: `orbterm = "ignore"` in `config/desktop.toml`. An optional package is written `"<recipe>.<part>" = {}`.
- `[[files]]` entries are appended, never replaced. To override a file from an included config, add an entry with the same `path` later in the order, as `config/desktop-minimal.toml` does to empty `30_console_switch`. An include brought in twice puts its entries in the order again at the second place: `config/desktop.toml` lists `server.toml` before `desktop-minimal.toml` because both include `minimal.toml`, and the other order let `minimal.toml`'s `30_console_switch` win again. Keys: `path`, `data`, and optionally `symlink` (then `data` is the link target), `directory`, `mode`, `uid`, `gid`, `recursive_chown`, `postinstall`, `append`.
- `[users.<name>]` and `[groups.<name>]` merge by name, a later table replacing the earlier one whole.

## What a change costs

- Only the file named by `FILESYSTEM_CONFIG` is a make prerequisite. After editing a config that others include (`base.toml`, `dev-accounts.toml`, `minimal.toml`, `server.toml`, `desktop.toml`), `make all` sees nothing new. Run `make rebuild`, or `make image` when only files, users or sizes changed.
- A config that others include changes every image built from them. `rg -l '<file>\.toml"' config` lists its direct includers.
- The architecture directories are `config/x86_64`, `config/aarch64` and `config/riscv64gc`. `config/<arch>/ci.toml` is the package list the build server cooks with `make ci-pkg`; its header asks to comment a broken package out instead of deleting its line.

## Check it

1. The TOML parses (rung 1 of [`docs/testing.md`](../../docs/testing.md)).
2. `make repo-tree CONFIG_NAME=<name> ARCH=<arch>` resolves every package to a recipe. Done when it prints the tree and its build summary without an error, for each architecture whose configs include the file.
3. After a cook, `make image-tree` with the same variables prints the estimated image size. Done when it is below `filesystem_size`.
4. A changed file or init script is proven by booting: [`images-and-qemu.md`](images-and-qemu.md).
