# A cooked package is staged, then renamed into place

Status: accepted

## Context

A cook unpacks build dependencies into a sysroot, runs the build script, stages the result, works out the package's runtime dependencies and packages it with pkgar; `repo_builder` then publishes the packages into `repo/<target>/`. A cook can be interrupted at any point, and `clean_target` deletes most of what a cook leaves behind.

## Decision

### Atomic directories

The sysroot is built in `sysroot.tmp` and the stage in `stage.tmp`, and each is renamed into place only when complete, so a directory with the final name is always whole.

### Dependency caches

- Both the sysroot cache and the stage cache are invalid without their init tag; `build` rebuilds them when the tag is missing.
- When `clean_target` is on, a tag file keeps the dependency directory from being removed until the build has used it; `clean_deps_dir` removes the tag after the build. The tags themselves are kept for the next check.
- `build_deps_dir` returns a signature key even for an empty dependency list; the key is not read in that case.

### Stages and runtime dependencies

- The stage stays on disk until the dependency analysis has read it.
- `get_stage_dirs` lists the mandatory package's stage last, because it holds what is left over after each optional package took its files.
- `auto_deps_from_dynamic_linking` treats relibc and the libraries installed with it as always present, so they are never added as dependencies.
- `publish_packages` publishes both the `[package] dependencies` of a recipe and the dynamically linked packages `auto_deps` found.
- `package` gives a meta-package only its metadata: it has no stage and no optional packages.
- pkgar does not record empty directories, so `walk_file_tree` leaves them out of a package's file tree.

### Installing into a directory

`package_handle_push` installs a package with the remote name `local`, the name the installer hard-codes for local packages, and installs bare: no package-state metadata, no cache.

### Package trees

`walk_tree_entry` calls its operation on a root before its dependencies (unlike the recursion in `CookRecipe`), hides host packages from target trees, and reports a recipe with no data as omitted without expanding it. A duplicate entry is listed without its dependencies again. `TreeData` sums package sizes only for push trees and counts missing recipes apart from unbuilt ones.

### Build identity

`IdentifierConfig::new` takes the build timestamp from the system `date` command rather than a date library.

## Consequences

- Writing a stage or sysroot in place, without the `.tmp` rename, lets an interrupted cook leave a directory the next cook trusts.
- Adding relibc's libraries to runtime dependencies would make every package depend on the C library package.

## Where

- `src/cook/cook_build.rs`: `build`, `build_deps_dir`, `clean_deps_dir`, `get_stage_dirs`, `auto_deps_from_dynamic_linking`
- `src/cook/package.rs`: `package`, `package_handle_push`
- `src/cook/tree.rs`: `TreeData`, `build_dependents`, `walk_tree_entry`, `walk_file_tree`
- `src/cook/ident.rs`: `IdentifierConfig::new`
- `src/bin/repo_builder.rs`: `publish_packages`
