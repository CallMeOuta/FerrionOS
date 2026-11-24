# FerrionOS's core components move into this repository

Status: accepted; landed on 2026-11-24 (`5958f13f6..77c259efc` on `191befd3e`, 106 commits; drivers in `ferrion-hardware` `2802feb34..c222cfd8` on `20da44c`, 26 commits)

## Context

FerrionOS changed its core components (the kernel, relibc, base and RedoxFS) through stacks of patch files beside their recipes, applied at build time to sources fetched from upstream's repositories. By the freeze on 2026-11-23 master's relibc carried 30 such patches, base 31 plus a vendored `smoltcp` patch, the kernel 17 and RedoxFS 19. Every lane that touched a component edited the same patch list, a patch could not be refactored without regenerating it, simulation tests (ADR 0025) needed the source as ordinary files, and a change across two components could not be one commit. The maintainer decided on 2026-11-21, from Paradigm's "Effective Swarm Riding for Engineers" ("monorepo everything"), to bring these components into this repository.

## Decision

- **Layout.** The kernel, relibc, `base` (without its drivers, keeping `graphics-ipc` and `inputd`) and RedoxFS live under `components/{kernel,relibc,base,redoxfs}`. Base's drivers moved into `ferrion-hardware` under GPL-2.0-only ([ADR 0047](0047-drivers-in-ferrion-hardware.md)).
- **Squashed import.** Each tree was imported as one squashed commit at its pinned upstream revision (without upstream's full commit history), followed by one commit per former FerrionOS patch.
- **Record file.** `components/upstream.toml` records each tree's upstream URL, pinned revision, license file sha256 digests, submodules, archives, and base's split rules; `scripts/components-check.py` verifies every tree against it on rung 1.
- **The `path` source.** Each of the four recipes replaces its `git` + `patches` `[source]` with `path = "../../../components/<name>"`. The cookbook (`fetch_link_tree` and `fetch_tree_ident` in `src/cook/fetch.rs`) symlinks `source` directly to the repository tree, records the git tree hash (`HEAD:./`) as `source_identifier`, counts directory modification times (`src/cook/fs.rs`) so file deletions trigger a cook, and never fetches, patches, resets or deletes the tree.
- **Upstream merges.** Later upstream revisions are merged deliberately via `scripts/component-upstream.sh check|merge <name>`, updating `components/upstream.toml` and rebaselining imported lines via `scripts/components-check.py --rebaseline <name>`.

## Consequences

- A change to any of the four core components is an ordinary edit and commit; patch lists and their ordering conflicts disappear for these four components.
- The repository grows by these components' sources while keeping clone times and pack size bounded by squashed imports.
- Host tools (`mk/fstools.mk`) and local consumers (`ferrion-audiod`, `ferrion-wm-backend`, `sim/redoxfs`, `scripts/virtgpu-layout-check.py`, `recipes/wip/drivers/ferrion-hardware`) read directly from `components/`.

## Where

- `components/{kernel,relibc,base,redoxfs}` and `components/upstream.toml`
- `recipes/core/{kernel,relibc,base,redoxfs}/recipe.toml` and `recipes/core/initfs/recipe.toml`
- `src/cook/fetch.rs` (`fetch_link_tree`, `fetch_tree_ident`), `src/cook/fs.rs`, `src/recipe.rs`
- `scripts/components-check.py`, `scripts/component-upstream.sh`, `scripts/base-split.py`
- `docs/upstream.md` and [ADR 0047](0047-drivers-in-ferrion-hardware.md)
