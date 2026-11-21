# Recipes

Read this before writing or changing a `recipe.toml` or its patches under `recipes/`.

A recipe is the directory `recipes/<category>/<name>/`: a `recipe.toml` and the files it names. The directory name is the package name. The cookbook finds recipes by that name alone (`src/staged_pkg.rs`), so a name is unique across every category, `wip` included. It contains no dot: a dot separates an optional package from its recipe (`gcc13.cxx`).

## Find it first

Search the whole tree before creating anything: `rg --files recipes -g recipe.toml | rg '/<name>/'`, then `rg -l '<project name or URL>' recipes -g recipe.toml`. Many programs already have a recipe or a `wip` stub. Continue that one.

## Where it lives

- A new or unfinished port lives in `recipes/wip/<category>/<name>/`. Its `recipe.toml` opens with a `#TODO` line that says what is missing, in the tree's vocabulary: `not compiled or tested`, `compiled but not tested`, a named error (`#TODO openssl-sys crate error`), or `promote` once it works. Update the line whenever the state changes.
- A recipe that works inside the OS lives in `recipes/<category>/<name>/`. Promoting it means moving the directory, removing the `#TODO` and giving a `tar` source its `blake3`.
- Prove a recipe on x86_64 first. It is the architecture with by far the most working packages ([`docs/platform-state.md`](../../docs/platform-state.md)).

## The format

`src/recipe.rs` is the schema: `SourceRecipe` for `[source]`, `BuildKind` and `BuildRecipe` for `[build]`, `PackageRecipe` for `[package]`, `OptionalPackageRecipe` for `[[optional-packages]]`. Read those types for the keys. What they do not tell you:

- An unknown key is ignored without an error, so a misspelled key silently does nothing. Check each key against the types.
- `template` takes exactly the `BuildKind` names. `wip` holds stubs with `template = "go"`, which the cookbook does not implement.
- `[source]` is one of `git`, `tar`, `same_as` (another recipe's `source/`, through a symlink) or `path`. Without a `[source]` section the cookbook fetches nothing and the script works from files in the recipe directory, as `recipes/tests/hello-redox` does.
- A `git` source without `rev` follows the branch head and changes under you. `rev` takes a commit hash or a tag.
- A `tar` source outside `wip` carries `blake3`; a few old recipes still lack it. Leave it out for the first fetch and copy the hash from the cookbook's line `WARNING: set blake3 for ... to ...`.
- `patches` are applied in order with `patch --strip=1`, then `[source] script` runs inside `source/`. A patch file is a byte-exact diff against third-party code: change it by regenerating it. For a `tar` recipe, `scripts/patch-tar.sh <name>` rewrites `redox.patch` from your edits in `source/`.
- `[build] dependencies` are unpacked into the sysroot the build sees. Runtime dependencies are derived from them: the shared libraries the binaries need, and the package dependencies of what was linked statically. `[package] dependencies` therefore lists only what the build cannot reveal, such as data or programs called at run time.
- `dev-dependencies` are needed for the build alone. A `host:` prefix (`"host:redoxer"`) builds the dependency for the build machine.

## The build script

Every template expands to a bash script, and `template = "custom"` supplies its own in `script`. `src/cook/script.rs` holds the helpers (`cookbook_cargo`, `cookbook_configure`, `cookbook_cmake`, `cookbook_meson`, `DYNAMIC_INIT` and others), and `build()` in `src/cook/cook_build.rs` sets the `COOKBOOK_*` variables. Before writing one:

- The script starts in the build directory, `${COOKBOOK_BUILD}`. The source is `${COOKBOOK_SOURCE}`, the recipe directory `${COOKBOOK_RECIPE}`, and the dependencies are under `${COOKBOOK_SYSROOT}`.
- Whatever ends up under `${COOKBOOK_STAGE}` is the package. Install under `usr/` (`${COOKBOOK_STAGE}/usr/bin`): in the image `/bin` and `/lib` are symlinks into `/usr`.
- Linking is static unless the script calls `DYNAMIC_INIT` first. The templates other than `custom` call it for you.
- The same script runs for `x86_64`, `aarch64` and `riscv64gc`. Take the triple from `${TARGET}` and `${GNU_TARGET}` and keep architecture names out of it.
- It runs under `bash -e` with an error trap, so the first failing command ends the cook.
- Afterwards the cookbook strips the executables and libraries in the stage and removes `*.la` files.

## Check it

Climb the ladder in [`docs/testing.md`](../../docs/testing.md) as far as the change needs:

1. The TOML parses (rung 1).
2. `make rt.<name>` prints the recipe's build tree. Done when it lists the dependencies without an error.
3. `CI=1 COOKBOOK_LOGS=true make r.<name>` cooks it. Done when the output ends with `cook <name> - successful` and `target/<target>/stage.pkgar` exists in the recipe directory. On failure read `build/logs/<target>/<name>.log`.
4. `find recipes/<category>/<name>/target/<target>/stage -type f` shows the files where the OS expects them.
5. Running the program needs an image: [`images-and-qemu.md`](images-and-qemu.md). Until it has run inside the OS, a `wip` recipe says `#TODO compiled but not tested`.

A recipe reaches an image through a filesystem config, or as a dependency of a meta-package in `recipes/groups/`: [`filesystem-configs.md`](filesystem-configs.md).
