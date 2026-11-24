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

- `components/{kernel,relibc,base,redoxfs}`, additional trees under `components/`, `components/upstream.toml`, and `components/<tree>.unsafe.md`
- `recipes/core/{kernel,relibc,base,redoxfs}/recipe.toml` and `recipes/core/initfs/recipe.toml`
- `src/cook/fetch.rs` (`fetch_link_tree`, `fetch_tree_ident`), `src/cook/fs.rs`, `src/recipe.rs`
- `scripts/components-check.py`, `scripts/component-upstream.sh`, `scripts/base-split.py`
- `docs/upstream.md` and [ADR 0047](0047-drivers-in-ferrion-hardware.md)

## Amendment (2026-11-24): Additional trees under `components/` and the `subset` kind

### Common rules for trees beyond the four core components

- **Additional trees and `kind`.** Trees under `components/` beyond `kernel`, `relibc`, `base` and `redoxfs` are allowed. Each tree has an entry in `components/upstream.toml` with a `kind` field (default `"component"` for the four core trees) and is verified by `scripts/components-check.py`; an unknown `kind` fails the check.
- **Baselines without whole-tree skips.** No tree under `components/` gets a whole-tree comment-check skip. Upstream's comments stay as imported and are recorded once in the import commit with `python3 scripts/components-check.py --rebaseline <name>` (covering brought-in comments and naming lines), and rebaselined again only at an upstream merge or a version bump. FerrionOS adds no comments of any kind (the existing repository rule), and `scripts/comment-check.py` enforces it.
- **FerrionOS unsafe-block justifications.** FerrionOS's unsafe blocks carry a smallest-scope `#[allow(clippy::undocumented_unsafe_blocks)]` (one `unsafe {` block per allowed helper), assert their invariant where possible, and are explained one line per block in the tracked file `components/<tree>.unsafe.md` in the form `path | function | invariant`, with `path` relative to `components/<tree>/`, no commit hashes and no line numbers.
- **Changes inside additional trees.** Trees beyond the four are upstream code with FerrionOS's commits on top. FerrionOS's changes to such a tree are commits in it, and small programs that prove those changes may sit inside it, listed in `added`.
- **Offline check.** `scripts/components-check.py` with no options runs offline and checks format and presence: the record's fields, object ids at the import commit, tracked stray paths, license digests, and the per-file `components/<tree>.unsafe.md` counts (requiring every allowed helper to contain exactly one `unsafe {` block and failing when any file's count differs or names a file with zero FerrionOS blocks).

### The `subset` kind

- A `kind = "subset"` entry in `components/upstream.toml` records a curated subset of an upstream repository by its `https://` `url` and full 40-hex commit `rev`, without requiring a `recipes/core/<name>` recipe.
- `[<name>.include]` maps each imported relative path (file or directory) to its 40-hex git object id at `rev`. `scripts/components-check.py` finds the import commit that first added the tree's anchor license file (`git log --diff-filter=A --reverse`) and verifies every `include` path's git object id at that commit against the recorded id. A shallow clone that lacks the import commit fails with `history is shallow; the subset proof needs the import commit`.
- `added` lists relative paths of FerrionOS-added files or directories inside `components/<name>/`. The stray-path check inspects tracked files (`git ls-files`) only and fails when any tracked file under `components/<name>/` falls outside `include` and `added`; untracked local build directories such as `target/` are ignored by the stray check and excluded via `.gitignore`.
- `[<name>.licenses]` maps each license and notice file relative to `components/<name>/` to its sha256 digest and is checked on every run.
- `scripts/components-check.py --rebaseline <name>` for `kind = "subset"` rebaselines only paths covered by `include`, never paths in `added`, so FerrionOS's own added files remain unbaselined at zero comments.

### The `archives` kind

- A `kind = "archives"` entry in `components/upstream.toml` records vendored upstream archives (such as crates.io `.crate` tarballs under `components/wgpu/`, consumed via `[patch.crates-io]` path entries), with no `url`, `rev`, `submodules`, `split`, or `recipes/core/<name>` recipe.
- Each unpacked archive under `[<name>.archives.<archive>]` records its relative `path`, `https://` archive `url`, 64-hex `sha256` (checked against `Cargo.lock` at import for crates.io archives), and `[<name>.archives.<archive>.licenses]` mapping each license file inside that archive to its sha256 digest.
- Plain offline `scripts/components-check.py` checks format and presence only for `kind = "archives"` entries (`https://` URLs, 64-hex `sha256` syntax, `dropped` paths, per-archive license file presence and sha256 digests, no unrecorded subdirectories under `components/<name>/`, no git submodule links, and `components/<name>.unsafe.md` counts).
- Online `scripts/components-check.py --verify-archives [name]` downloads each recorded archive with bounded size and timeout, verifies its sha256 against `components/upstream.toml`, unpacks it to a scratch directory with bounded file count and total unpacked bytes (refusing absolute paths, `..`, symlinks, and hard links), locates the import commit from `components/upstream.toml`'s git history, and compares every file byte-for-byte against that import commit so later FerrionOS commits do not break verification.
- Updates happen deliberately at a version bump: verify each new archive's sha256, unpack plain files into `components/<name>/<path>/`, review license changes and update their sha256 digests in `components/upstream.toml`, and run `scripts/components-check.py --rebaseline <name>` once in that bump commit.
