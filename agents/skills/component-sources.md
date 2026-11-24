# Component sources

Read this before changing code in `components/` (the kernel, relibc, `base` without drivers, RedoxFS), in `ferrion-hardware` drivers, or inside any other recipe's fetched `source/` directory.

## The four trees in this repository

The kernel, relibc, `base` (system services plus `graphics-ipc` and `inputd`) and RedoxFS live under `components/<name>/` ([ADR 0027](../../docs/adr/0027-monorepo.md)). Their recipes link `recipes/core/<name>/source` to `../../../components/<name>` via `[source] path`, and the cookbook never fetches, patches, resets or deletes them.

- A change is an ordinary edit and commit under the component's category (`kernel:`, `relibc:`, `base:`, `redoxfs:`). No patch files are added under `recipes/core/<name>/`.
- Cook with `CI=1 COOKBOOK_LOGS=true make r.<name>` (and `make p.<name>` or `make rp.<name>` to push into an existing image).
- Cook dependents when shared interfaces or boot images change: `make r.redoxfs,bootloader,base,initfs` after RedoxFS changes (`make fstools` also rebuilds host tools from `components/redoxfs`); `make r.relibc,base,drivers,initfs` after `components/relibc/redox-ioctl` changes; `make c.relibc` when the cross toolchain's copy must follow.
- `components/kernel/config.toml` is generated during kernel cooks and git-ignored; leave it out of commits.
- Root `.gitignore` ignores directories named `source` or `target`; if a new file under `components/` sits inside a path with that name, stage it with `git add -f`.
- New code carries no comments (imported comments stay under `scripts/comment-check.baseline.json`), agents write no unit tests ([ADR 0025](../../docs/adr/0025-tests-without-unit-tests.md)), and upstream license files stay untouched (`python3 scripts/components-check.py` checks their sha256 digests).
- Bring in later upstream revisions only through `scripts/component-upstream.sh check|merge <name>`, which updates `components/upstream.toml` and rebaselines imported lines.

## The drivers

FerrionOS's own drivers live in `ferrion-hardware` (`github.com/CallMeOuta/ferrion-hardware`, under `drivers/`, GPL-2.0-only beside upstream's MIT notice; [ADR 0047](../../docs/adr/0047-drivers-in-ferrion-hardware.md)). This repository pins `ferrion-hardware` by `rev` in `recipes/core/drivers/recipe.toml` and compiles its drivers against `components/` via `scripts/drivers-lock.sh` and `recipes/core/drivers/Cargo.lock`.

- To iterate locally inside a FerrionOS checkout: `make f.drivers && make lc.drivers`, edit inside `recipes/core/drivers/source/drivers/`, and cook with `CI=1 COOKBOOK_LOGS=true make r.drivers,initfs`.
- Commit driver changes in `ferrion-hardware` (every new file starts with `SPDX-License-Identifier: GPL-2.0-only`, verified by `scripts/license-check.py`), then move `rev` in `recipes/core/drivers/recipe.toml`, run `make f.drivers && scripts/drivers-lock.sh write`, clear `make cc.drivers`, and prove with `make r.drivers,initfs` and the driver's checks in `scripts/vm-test.py`.
- A change that spans FerrionOS's crates and a driver lands as FerrionOS's compatible half first, then the driver commit in `ferrion-hardware`, then the pin move in `recipes/core/drivers/recipe.toml`.

## Every other fetched component

Every other component's code (bootloader, installer, Orbital, ports) is not in this repository. A recipe's `[source]` says where it comes from, and the cookbook clones or unpacks it into `recipes/<category>/<name>/source/`; `make find.<name>` prints the recipe directory ([`docs/upstream.md`](../../docs/upstream.md)).

### Keep your edits

The cookbook owns `source/` and puts it back into the recipe's state:

- A cook fetches first. A fetch that finds anything to update checks a git source out at upstream's head again, and for a recipe with `patches` or a `script` it runs `git reset --hard` before that.
- `make u.<name>` and every target with `u` among its letters (`uc.`, `ucr.`, `ucf.`), `make unfetch` and `make distclean` delete `source/`.

So set the recipe's rule to `local` before the first edit: `make f.<name>` fetches, then `make lc.<name>` sets the rule. Under `local` the cookbook builds what is in `source/` and neither fetches nor patches it. `make cc.<name>` puts the default rule back. `REPO_OFFLINE=1` gives every cook the same protection at once. Neither protects from the `u` targets.

### Build, install, boot

1. `CI=1 COOKBOOK_LOGS=true make r.<name>` cooks the component from your tree. Done when the output ends with `cook <name> - successful`.
2. `make p.<name>` pushes the package into the existing image, with QEMU stopped. `make rp.<name>` does steps 1 and 2. Without an image yet, `make image` builds one from every cooked package.
3. Boot and check: [`images-and-qemu.md`](images-and-qemu.md).

### Make it last

An edit under `source/` exists only in this checkout, inside another project's repository. To survive a fresh fetch, the recipe has to carry it:

- as a patch file beside `recipe.toml`, listed in `[source] patches`. Git sources take patches too, as `recipes/net/curl` shows, and `git -C <source directory> diff > <recipe directory>/<file>.patch` writes one that `patch --strip=1` accepts;
- or by pointing `[source] git` at a repository that contains the change.

Which of the two a component gets is the maintainer's decision. Ask when the task does not say. Once the recipe carries the change, clear the rule with `make cc.<name>` and prove it from a clean fetch with `make ucr.<name>`, which deletes `source/` first.

Code taken from another project keeps its license notice and names where it came from.
