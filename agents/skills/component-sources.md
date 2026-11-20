# Component sources

Read this before changing code inside a recipe's `source/` directory: the kernel, relibc, the drivers in `base`, or any other fetched package.

The components' code is not in this repository. A recipe's `[source]` says where it comes from, and the cookbook clones or unpacks it into `recipes/<category>/<name>/source/`; `make find.<name>` prints the recipe directory. Almost every system component comes from Redox's GitLab ([`docs/upstream.md`](../../docs/upstream.md)).

## Keep your edits

The cookbook owns `source/` and puts it back into the recipe's state:

- A cook fetches first. A fetch that finds anything to update checks a git source out at upstream's head again, and for a recipe with `patches` or a `script` it runs `git reset --hard` before that.
- `make u.<name>` and every target with `u` among its letters (`uc.`, `ucr.`, `ucf.`), `make unfetch` and `make distclean` delete `source/`.

So set the recipe's rule to `local` before the first edit: `make f.<name>` fetches, then `make lc.<name>` sets the rule. Under `local` the cookbook builds what is in `source/` and neither fetches nor patches it. `make cc.<name>` puts the default rule back. `REPO_OFFLINE=1` gives every cook the same protection at once. Neither protects from the `u` targets.

## Build, install, boot

1. `CI=1 COOKBOOK_LOGS=true make r.<name>` cooks the component from your tree. Done when the output ends with `cook <name> - successful`.
2. `make p.<name>` pushes the package into the existing image, with QEMU stopped. `make rp.<name>` does steps 1 and 2. Without an image yet, `make image` builds one from every cooked package.
3. Boot and check: [`images-and-qemu.md`](images-and-qemu.md).

RedoxFS is also built into the initfs inside the `base` package (`recipes/core/base/recipe.toml` lists it as a build dependency). After changing RedoxFS cook `base` as well: `make rp.redoxfs,base`, per the Redox Book's [How to update RedoxFS](https://doc.redox-os.org/book/coding-and-building.html#how-to-update-redoxfs).

## Make it last

An edit under `source/` exists only in this checkout, inside another project's repository. To survive a fresh fetch, the recipe has to carry it:

- as a patch file beside `recipe.toml`, listed in `[source] patches`. Git sources take patches too, as `recipes/net/curl` shows, and `git -C <source directory> diff > <recipe directory>/<file>.patch` writes one that `patch --strip=1` accepts;
- or by pointing `[source] git` at a repository that contains the change.

Which of the two a component gets is the maintainer's decision. Ask when the task does not say. Once the recipe carries the change, clear the rule with `make cc.<name>` and prove it from a clean fetch with `make ucr.<name>`, which deletes `source/` first.

Code taken from another project keeps its licence notice and names where it came from.
