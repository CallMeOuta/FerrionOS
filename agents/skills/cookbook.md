# The cookbook

Read this before changing the Rust code under `src/`.

`src/` is one crate, `redox_cookbook`: a library and four binaries, built side by side into `target/release/`.

- `repo` is the command every recipe target in `mk/repo.mk` calls (`src/bin/repo/`).
- `repo_builder` publishes cooked packages into `repo/<target>/`. `repo cook` runs it at the end.
- `cookbook_redoxer` wraps upstream's `redoxer` crate and provides the cross-compilation environment every build script runs in.
- `repo_editor` is an interactive terminal front end, started by `make repo_editor`.

`repo` looks for `repo_builder` and `cookbook_redoxer` beside its own executable, so build them together.

## Build and check

- `make` does not notice an edit in `src/`: it rebuilds the cookbook only when `build/fstools.tag` is missing. Run `make cookbook` after a change.
- `cargo fmt -- --check` and `cargo test --locked` are the checks, rung 2 of [`docs/testing.md`](../../docs/testing.md). Upstream's CI ran the tests with `RUSTFLAGS="-Dwarnings"`.
- To watch a change act, cook one small recipe: `make c.<recipe>`, then `CI=1 make r.<recipe>`. A cooked recipe stays `cached` across cookbook changes, including changes to the bash in `src/cook/script.rs`.

## What depends on this code

- Every `recipe.toml`. `src/recipe.rs` is their schema. Unknown keys are ignored, so a new key breaks nothing and a renamed key is silently dropped everywhere: `rg` the old key across `recipes/` in the same change.
- The callers of `repo`. `mk/*.mk` and `scripts/` call it with fixed commands and flags and parse its output (`find`, `--display=csv`). `rg 'REPO_BIN|target/release/repo' Makefile mk scripts` lists them. `REPO_HELP_STR` in `src/bin/repo/main.rs` is the list users see: keep it in step.
- The container. An environment variable the cookbook reads reaches a Podman build only when `PODMAN_CONFIG` in `mk/podman.mk` forwards it.
- Every host. The cookbook, and the bash in `src/cook/script.rs`, run on Linux, macOS, FreeBSD and on FerrionOS itself (`is_redox()` in `src/lib.rs`). Give an external command only options all of them accept; `fetch_extract_tar` in `src/cook/fetch.rs` shows the pattern.
- Upstream's crates. `PackageName`, `Package` and the filesystem-config types come from `redox-pkg`, `pkgar` and `redox_installer`, git dependencies in `Cargo.toml`. Their code is not in this repository.

## Conventions in the code

- Errors are `cookbook::Error`, built with `wrap_io_err!`, `wrap_other_err!` and `bail_other_err!` from `src/lib.rs`. An I/O context is a gerund phrase such as `"Reading file"`, because `Display` appends `failed at "<path>"` itself.
- Preserved upstream tests sit in `#[cfg(test)] mod tests`. Under `cfg(test)`, `init_config` in `src/config.rs` uses a fixed mirror table and reads neither `cookbook.toml` nor the environment.

## Regression proof

Follow [ADR 0025](../../docs/adr/0025-tests-without-unit-tests.md): prove cookbook changes through a real recipe/CLI differential comparison or a deterministic filesystem/process simulation, red before the fix and green after it. Preserve imported upstream tests; do not extend them with agent-written unit tests. Record an uncovered contract in the local issue tracker.
