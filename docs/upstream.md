# What FerrionOS still takes from Redox

FerrionOS forks one repository of the Redox OS project: the build system. Everything in the table is still fetched from Redox's repositories and servers, at build time or inside a running system. Use it to judge what a change in this repository can reach, and to recognise the upstream-owned things whose names stay `Redox` (naming rules: [`AGENTS.md`](../AGENTS.md)).

| What | Comes from | Set in |
|---|---|---|
| Source of the system components: kernel, `base` (essential system components and drivers), relibc, RedoxFS, bootloader, installer, Orbital and more | Git repositories under `gitlab.redox-os.org/redox-os/`, followed at their branch head unless the recipe sets `rev` | `[source] git` in the recipe's `recipe.toml` |
| Crates the cookbook links: `pkgar`, `redox-pkg`, `redox_installer`, `redoxer` | The same GitLab, at the revisions in `Cargo.lock` | `Cargo.toml` |
| Prebuilt cross toolchain (`PREFIX_BINARY=1`) | `https://static.redox-os.org/toolchain/` | `mk/prefix.mk` |
| Prebuilt packages (`REPO_BINARY=1`, rule `binary`) | `https://static.redox-os.org/pkg` | `REMOTE_PKG_SOURCE` in `src/lib.rs` |
| Package source and project links written into every image | `https://static.redox-os.org/pkg` in `/etc/pkg.d/50_redox`, redox-os.org addresses in `/usr/lib/os-release` | `config/base.toml` |
| Layer cache for the Podman image | `docker.io/redoxos/redox-base-<host arch>` | `PODMAN_CACHE_PATH` in `mk/podman.mk` |
| `config.sub` for autotools ports | `gitlab.redox-os.org/redox-os/gnu-config` | `GNU_CONFIG_GET` in `src/cook/script.rs` |
| U-Boot images for ARM boards | `gitlab.redox-os.org/Ivan/redox_firmware` | `mk/qemu.mk` |
| The checkout the bootstrap scripts clone | `gitlab.redox-os.org/redox-os/redox.git` | `boot()` in `native_bootstrap.sh` and `podman_bootstrap.sh` |
| The `cookbook` package inside an image | The same repository | `recipes/other/cookbook/recipe.toml` |
| Source links on generated package pages | `https://gitlab.redox-os.org/redox-os/redox` | `this_repo` in `src/web.rs` |
| Documentation | The Redox Book, `https://doc.redox-os.org/book/` | Links in `README.md` and `CONTRIBUTING.md` |

List the recipes that fetch from Redox's GitLab with `rg -l 'gitlab\.redox-os\.org' recipes -g recipe.toml`.

## What follows from it

- An image built here is Redox's components with this repository's recipes, configs and names. A change to a component's code takes the route in [`agents/skills/component-sources.md`](../agents/skills/component-sources.md).
- `REPO_BINARY=1` installs packages that Redox's build server compiled from Redox's sources. It tests a config and the image assembly; it cannot test a change to a recipe or to a component.
- Recipes without `rev` move with upstream, so two cooks on different days can build different code. `make repo-lock` freezes the revisions now checked out, and `REPO_OFFLINE=1` stops fetching.
- The bootstrap scripts clone upstream Redox into a new `redox/` directory. In this checkout run them with `-d`, which installs the dependencies and stops there.
- When gitlab.redox-os.org is unreachable, the cookbook cannot be built from an empty Cargo cache and the component recipes cannot be fetched. On 2026-11-20 it served git over HTTPS (`git ls-remote https://gitlab.redox-os.org/redox-os/base.git HEAD` answered `20f7573b`) while web page requests got a Cloudflare challenge, so builds fetch normally; browse its pages in a browser.
