# How the repo command selects recipes, logs and reports

Status: accepted

## Context

`repo` is called by every recipe target in `mk/repo.mk` and by scripts that parse its output ([`agents/skills/cookbook.md`](../../agents/skills/cookbook.md)). It runs either with a terminal interface or plain (`CI=1`), and its options come from the command line, from `cookbook.toml` and from `COOKBOOK_*` variables. The commands and variables a user sees are listed in `REPO_HELP_STR`; this record holds the reasons behind behavior that list does not explain.

## Decision

### Names callers depend on

- `rebuild` is an alias of `cook` for `repo_editor`, and `find` is an alias of `repo-list` for the scripts in `scripts/`. Both stay because callers outside `src/` use them.
- `--category-<path>`, with dots for slashes, selects a recipe category in one word, because make cannot pass the separate option and value through its pattern targets.
- Informational commands print nothing else to standard output, so callers can parse it (`--display=csv`).

### Choosing recipes and dependencies

- `clean` acts on the named recipes only and loads no dependencies.
- Rules from `cookbook.lock` are not expanded again: the lock file already lists every recipe.
- `parse_args` splits the recipe list by rule, because a recipe built from source needs its build dependencies while a `binary` recipe or a push does not; development dependencies load only for source builds. After the dependencies are added, the rules apply again to them.
- A host recipe always builds from source, so `handle_change_rule` does not change its rule. Before choosing revisions to roll back to, it fetches, because the git tracking may have changed.
- `unfetch` leaves a recipe's target directory, which may hold downloaded binaries.
- With `nonstop`, `handle_nonstop_fail` deletes a failed recipe's stage artifacts, which is how `repo_builder` learns not to publish it.

### Logs and output

- Logs are written to `build/logs/` by default when the terminal interface runs (`logs`). A successful cached cook is not logged, and when a recipe is retried the full log of the earlier real work is kept. A retry reads `recipe.toml` again first.
- Without `verbose`, progress output is hidden; cached cooks are always hidden and a failure is always printed.
- `verbose_cmd` asks build commands for verbose output; each recipe may or may not honor it.
- `jobs` overrides the job count, which otherwise comes from the number of processors. `clean_build` empties the build directory before building.
- `drain_buffer_to_lines` hands only complete lines to the log view and trims carriage returns. `kill_everything` stops every process under a given PID, or under the cookbook itself; cancelling in the terminal interface uses it because compilers keep running otherwise.

### Code conventions

- The module that cooks a recipe is `cook_build`, not `build`, to avoid confusion with Cargo's `build.rs`.
- An I/O error's context names the operation as a gerund ("Reading file"); `Display` appends the path, so the context never says "Failed at".
- `try_get_config` exists because the configuration is not loaded in unit tests; every test sets the same configuration, so setting it again is a no-op.
- `WALK_DEPTH` bounds how many levels of dependencies a walk descends by default.
- `cookbook_redoxer` forwards every flag it receives to cargo.

### The web pages

`generate_web` links to the package repository by absolute URL rather than to `/repo`, and restores the package name it selected after loading the recipe, because `CookRecipe::from_path` drops the optional-package suffix. The search index is written as JSON, which the page's JavaScript reads natively.

## Consequences

- Renaming an alias or the `--category-` form breaks `mk/repo.mk`, `repo_editor` or a script; `rg 'REPO_BIN|target/release/repo' Makefile mk scripts` lists the callers.
- Printing progress to standard output in an informational command breaks every caller that parses it.

## Where

- `src/bin/repo/main.rs`: `CliCommand::from_str`, `parse_args`, `repo_inner`, `handle_nonstop_fail`, `handle_clean`, `handle_change_rule`, `REPO_HELP_STR`
- `src/bin/repo/app.rs`: `run_tui_cook`, `handle_main_event`
- `src/config.rs`: `CookConfigOpt` (`jobs`, `logs`, `verbose`, `verbose_cmd`, `clean_build`), `try_get_config`
- `src/cook/tui.rs`: `drain_buffer_to_lines`, `kill_everything`
- `src/cook.rs`: the `cook_build` module
- `src/lib.rs`: `WALK_DEPTH`, `Error`
- `src/bin/cookbook_redoxer.rs`
- `src/web.rs`: `CliWebConfig`, `generate_web`; `src/web/search.rs`: `write`
