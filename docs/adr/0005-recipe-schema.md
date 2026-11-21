# What the recipe keys mean beyond their names

Status: accepted

## Context

`src/recipe.rs` is the schema of every `recipe.toml` ([`agents/skills/recipes.md`](../../agents/skills/recipes.md)). Several keys and the loading code around them carry rules that the type of the key does not show. This record holds them.

## Decision

### Sources

- `same_as` reuses another recipe's `source/`: one repository that holds several projects gets one recipe per project and one checkout. Its value is a path relative to the recipe's own directory. `canon_recipe_name` names the recipe that owns the source, and the cookbook uses that name wherever it means the source.
- A git source should give `branch` as well as `rev`: `rev` makes the build reproducible, and `branch` says where the next `rev` comes from.
- A tar source should give `blake3`, which makes a reproducible build checkable.
- `pinned` records whether the recipe's revision comes from `cookbook.lock` (made by `make repo-lock`), so the cookbook knows not to move it.

### Builds

- A cargo recipe's `cargopath` is relative to the source directory.
- Binaries built from `cargopackages` (the members of a workspace) keep their names; binaries built from `cargoexamples` are prefixed with the recipe's name, because example names such as `hello` collide across recipes.
- `clearlocked` drops the default `--locked`, which is common for libraries, whose repositories often carry no lock file.
- A python recipe builds and installs with pip. `setup.py` runs only when the recipe sets `legacysetup`: some projects need it and others refuse it, and an error such as `invalid command 'dist_info'` is the sign that a recipe needs it.

### Packages and dependencies

- A meta-package (a recipe with no build) has an empty version string, and `set_as_remote` leaves meta-packages alone: a `remote` build cannot fetch one.
- `get_packages_list` lists the optional packages first and the mandatory package last, because `cook_build` stages them in that order.
- A host recipe (`host:` prefix) has no package dependencies, which describe a package installed into the image.
- `get_package_deps_recursive` follows build dependencies too, because a library a recipe builds against can bring runtime files of its own.
- `new_recursive_nonstop` returns the packages that loaded and a map of failures; a package can be in both when it loaded but a dependency did not.

### Filesystem rules

- `from_path` skips reading `recipe.toml` for `clean` and `unfetch`, which do not need it.
- `reload_recipe` reads `recipe.toml` again but keeps the effect of the recipe's rule (`source`, `binary`, `local`, `ignore`), and the `local` rule turns off fetching so local changes in `source/` survive.
- `validate_filesystem_rules` stops before building when a recipe whose rule is `ignore` is still a build dependency of a recipe that will be built from source. A recipe that is not built from source needs no build dependencies, so it is not checked.

### Stage paths

`stage_paths` returns the stage directory, the package file and its TOML in that order, and `target_dir` follows the `host:` prefix: a host recipe stages for the build machine.

## Consequences

- A change to one of these rules changes every recipe that relies on it; `rg` the key across `recipes/` in the same change, as the cookbook guide asks.
- Reversing the package order breaks `cook_build`'s staging of optional packages.

## Where

- `src/recipe.rs`: `SourceRecipe` (`same_as`, `branch`, `rev`, `blake3`), `BuildKind::Cargo` (`cargopath`, `cargopackages`, `cargoexamples`, `clearlocked`), `BuildKind::Python` (`legacysetup`), `Recipe::pinned`, `set_as_remote`, `get_packages_list`, `CookRecipe::new`, `from_path`, `get_package_deps_recursive`, `reload_recipe`, `stage_paths`, `target_dir`, `apply_filesystem_config`, `guess_version`, `canon_recipe_name`, `validate_filesystem_rules`
- `src/staged_pkg.rs`: `new_recursive_nonstop`
