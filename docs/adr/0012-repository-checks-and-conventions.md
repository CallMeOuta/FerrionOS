# What the repository's own checks enforce, and how files are kept

Status: accepted

## Context

FerrionOS keeps its rules in `AGENTS.md` and `docs/code-policy.md`, and enforces some of them with standard-library Python scripts under `scripts/` (rung 1 of [`docs/testing.md`](../testing.md)). Editors follow `.editorconfig`. The book is published from this repository with GitHub Pages.

## Decision

### Checks

- `scripts/naming-check.py` lists every tracked line that names the upstream project and classifies it by the exceptions of AGENTS.md "Naming". Its rules apply in a fixed order and the first match gives the class: files applied byte-exact from upstream (exception 3), by path; the fork sentence, allowed once in `README.md` and once in `book/src/introduction.md`; identifiers and machine-matched strings (exception 2), case-sensitive, where every occurrence on the line must sit inside a match for the line to count. `make qemu`'s `share=redox` value is one such identifier: it shares the build tree under the tag `redox`. The checker and its baseline quote the name as data and are not scanned.
- `scripts/no-x11-check.py` fails when a tracked recipe lists an X11 package in its build, dev, package or optional-package dependencies, when a filesystem config installs one, or when a recipe of that name exists (AGENTS.md "Display"). It prints each finding and exits 1.
- `scripts/lint-commit-message.py` checks a commit message against `docs/code-policy.md`: a `Category: Description` subject in the imperative, an empty second line, lines of at most 72 characters except a lone URL, no merge commits and no `Signed-off-by`. It runs as a `commit-msg` hook on a file, or with `--range` on existing commits.
- `scripts/comment-check.py` keeps code free of comments, as [0001](0001-no-code-comments.md) records.

### Helper scripts

- `scripts/open-recipe.sh` runs on the host, outside Podman; `scripts/pkg-size.sh`, which prints the size of recipes' `stage.pkgar`, runs inside the container (`make env`).
- `scripts/recipe-category.sh` writes a category's slashes as dots, which the cookbook reads back as slashes (`--category-<path>`).
- `scripts/test-vm-graphics.py` bounds the whole screen capture by time, because a server can send Bell messages forever, each well inside the per-read timeout.

### Files

- Makefiles indent with tabs: make requires tabs in recipe lines.
- Markdown keeps trailing whitespace, which can be a line break.
- `*.patch` files are exempt from every `.editorconfig` rule: patches against third-party sources are byte-exact, and their context lines keep their trailing whitespace, tabs and line endings.
- `/plans/` is ignored: the roadmaps stay on the maintainer's machine and are never committed.

### The book

`.github/workflows/book.yml` builds the FerrionOS Book (`book/`, mdBook) and publishes it with GitHub Pages, whose source must be set to "GitHub Actions" in the repository's settings. One deployment runs at a time: a running one is left to finish, so a deployment is never cut off halfway, and of the runs that wait only the newest is kept.

## Consequences

- A new exception to the naming rule belongs in `naming-check.py`'s rules and in AGENTS.md together.
- Running `.editorconfig`-aware formatting on a patch breaks it.

## Where

- `scripts/naming-check.py`, `scripts/no-x11-check.py`, `scripts/lint-commit-message.py`, `scripts/comment-check.py`
- `scripts/open-recipe.sh`, `scripts/pkg-size.sh`, `scripts/recipe-category.sh`, `scripts/test-vm-graphics.py`
- `.editorconfig`, `.gitignore`
- `.github/workflows/book.yml`: `concurrency`
