# What FerrionOS still takes from upstream

FerrionOS's four core trees (`components/kernel`, `components/relibc`, `components/base` without its drivers, and `components/redoxfs`) live in this repository ([ADR 0027](adr/0027-monorepo.md)), and its drivers live in `ferrion-hardware` ([ADR 0047](adr/0047-drivers-in-ferrion-hardware.md)). Everything in the table below is still fetched from upstream's or FerrionOS's other repositories and servers, at build time or during upstream merges. Use it to judge what a change in this repository can reach, and to recognise the URLs and identifiers that keep their upstream names (naming rules: [`AGENTS.md`](../AGENTS.md)).

| What | Comes from | Set in |
|---|---|---|
| Source of fetched system components: bootloader, installer, Orbital and other ports | Git repositories under `gitlab.redox-os.org/redox-os/`, followed at their branch head unless the recipe sets `rev` | `[source] git` in the recipe's `recipe.toml` |
| Later upstream revisions of the four core trees (`kernel`, `relibc`, `base`, `redoxfs`) | Upstream GitLab repositories merged deliberately via `scripts/component-upstream.sh` | `components/upstream.toml` |
| FerrionOS's drivers and Linux driver host (`ferrion-hardware`) | `https://github.com/CallMeOuta/ferrion-hardware.git` (FerrionOS's own GPL-2.0-only repository), fetched at a pinned commit | `recipes/core/drivers/recipe.toml` (and `same_as` recipes) |
| Crates the four trees and the drivers still fetch by git URL at cook time | Upstream GitLab (`redox-rings`, `redox-buffer-pool`, `object`, `orbclient`, `redox-log`, `redox-scheme`, `acpi`, `rehid`) and GitHub (`fdt`, `range-alloc`) | `Cargo.toml` / `Cargo.lock` inside `components/` and `ferrion-hardware` |
| Crates the cookbook links: `pkgar`, `redox-pkg`, `redox_installer`, `redoxer` | Upstream GitLab, at the revisions in `Cargo.lock` | `Cargo.toml` |
| Prebuilt cross toolchain (`PREFIX_BINARY=1`) | `https://static.redox-os.org/toolchain/` | `mk/prefix.mk` |
| Prebuilt packages (`REPO_BINARY=1`, rule `binary`) | `https://static.redox-os.org/pkg` | `REMOTE_PKG_SOURCE` in `src/lib.rs` |
| Layer cache for the Podman image | `docker.io/redoxos/redox-base-<host arch>` | `PODMAN_CACHE_PATH` in `mk/podman.mk` |
| `config.sub` for autotools ports | `gitlab.redox-os.org/redox-os/gnu-config` | `GNU_CONFIG_GET` in `src/cook/script.rs` |
| U-Boot images for ARM boards | `gitlab.redox-os.org/Ivan/redox_firmware` | `mk/qemu.mk` |
| The checkout the bootstrap scripts clone | `gitlab.redox-os.org/redox-os/redox.git` | `boot()` in `native_bootstrap.sh` and `podman_bootstrap.sh` |
| The `cookbook` package inside an image | The same repository | `recipes/other/cookbook/recipe.toml` |

List the recipes that fetch from upstream's GitLab with `rg -l 'gitlab\.redox-os\.org' recipes -g recipe.toml`.

## Upstream merges

- Check upstream carry cost for one of the four trees: `scripts/component-upstream.sh check <name>`.
- Merge a new upstream revision into `components/<name>`: `scripts/component-upstream.sh merge <name> [<rev>]`, resolve any conflicts, review license diffs, run `python3 scripts/components-check.py --rebaseline <name>` if imported comments or upstream prose changed, and prove with the component's VM checks.
- For `base`, upstream revisions are merged in both repositories from the same upstream commit: `scripts/component-upstream.sh merge base <rev>` here and `scripts/drivers-upstream.sh merge <rev>` in `ferrion-hardware`.

## What follows from it

- Changes to `kernel`, `relibc`, `base` and `redoxfs` happen directly in `components/` and build offline from this repository. Changes to drivers happen in `ferrion-hardware` and reach images through `recipes/core/drivers/recipe.toml`. Other fetched components follow [`agents/skills/component-sources.md`](../agents/skills/component-sources.md).
- `REPO_BINARY=1` installs packages that upstream's build server compiled from upstream's sources. It tests a config and the image assembly; it cannot test a change to a recipe or to a component.
- Recipes without `rev` move with upstream, so two cooks on different days can build different code. `make repo-lock` freezes the revisions now checked out, and `REPO_OFFLINE=1` stops fetching.
- The bootstrap scripts clone upstream's build system into a new `redox/` directory. In this checkout run them with `-d`, which installs the dependencies and stops there.
- When upstream's GitLab is unreachable, the cookbook cannot be built from an empty Cargo cache and fetched component recipes cannot be cloned, though the four core trees under `components/` need no network fetch.
