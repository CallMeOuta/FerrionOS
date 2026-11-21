# FerrionOS

This repository is FerrionOS's build system, forked from an upstream project's: makefiles (`Makefile`, `mk/`), a Rust build tool called the cookbook (`src/`), package recipes (`recipes/`) and filesystem configs (`config/`). Together they fetch, cross-compile and package the operating system's components and assemble a bootable image. The components' own code (kernel, relibc, drivers) is fetched at build time and lives in other repositories.

# Task Guides

Read the matching guide before starting:

- Writing or changing a `recipe.toml` or its patches: [`agents/skills/recipes.md`](agents/skills/recipes.md)
- Changing what an image contains, `config/**/*.toml`: [`agents/skills/filesystem-configs.md`](agents/skills/filesystem-configs.md)
- Changing code inside a recipe's fetched `source/` (kernel, relibc, drivers, any package): [`agents/skills/component-sources.md`](agents/skills/component-sources.md)
- Changing the cookbook, the Rust code under `src/`: [`agents/skills/cookbook.md`](agents/skills/cookbook.md)
- Changing `Makefile`, `mk/*.mk`, the bootstrap scripts, `scripts/` or `podman/`: [`agents/skills/build-scripts.md`](agents/skills/build-scripts.md)
- Building an image or booting one in QEMU: [`agents/skills/images-and-qemu.md`](agents/skills/images-and-qemu.md)

# Reference

- Relying on something you remember about the system's components, or a port, test or boot failing in hardware, networking, terminals and PTYs, the filesystem or POSIX and relibc: [`docs/platform-state.md`](docs/platform-state.md)
- Choosing a check, or about to start a build: [`docs/testing.md`](docs/testing.md)
- How a `make` run becomes an image, where a setting comes from, what a target name means: [`docs/build-system.md`](docs/build-system.md)
- What still comes from upstream's servers and repositories: [`docs/upstream.md`](docs/upstream.md)
- Writing a commit message, or any text a user or reader sees, or reusing work linked from an issue: [`docs/code-policy.md`](docs/code-policy.md)
- Why the code is the way it is, before changing an area or reversing a choice: the decision records in [`docs/adr/`](docs/adr/README.md)
- Roadmaps, when `plans/` exists on this machine (it is kept out of git, so a clone has none): `plans/kernel-roadmap.md` before setting up hosting or forks or starting kernel, relibc or RedoxFS work; `plans/product-roadmap.md` before deciding what to build next for users; `plans/linux-driver-compat.md` before porting a Linux driver; `plans/performance-roadmap.md` before changing a build flag, a hot path or a hardening default; `plans/security-roadmap.md` before any change listed under Security. Never commit them.

Documentation is split by genre. `agents/skills/` holds task procedure and `docs/` holds reference on how the build system is shaped; both are for anyone working on the codebase. The roadmaps in `plans/` stay local to the maintainer's machine. User documentation is the FerrionOS Book in `book/` (mdBook, published at https://callmeouta.github.io/FerrionOS/), and it follows the same naming rule.

# Agent skills

## Issue tracker

Local Markdown files under `.scratch/`, kept out of git. See `docs/agents/issue-tracker.md`.

## Triage labels

The five default triage roles, each written as its own name on an issue's `Status:` line. See `docs/agents/triage-labels.md`.

## Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repository root. See `docs/agents/domain.md`.

# Rules

- The code in this repository is the source of truth. The book in `book/` lags behind it: where they disagree, follow the code.
- A failure that also happens without your change is pre-existing. Report it with its log and move on.
- Code brought in from another project keeps its license notice and names its source.
- Code carries no comments. A decision goes into an ADR in `docs/adr/`, and a hack or an unfinished state into an issue. What stays: license and copyright notices and the source notice of brought-in code, directives a tool reads (a shebang, `shellcheck`, `noqa`), the documentation of make targets, build variables and emulator options in `Makefile` and `mk/*.mk`, and the reason for a setuid, setgid or world-write `mode`; `scripts/comment-check.config.json` lists them. `python3 scripts/comment-check.py` enforces it against `scripts/comment-check.baseline.json`, whose counts only go down: after deleting comments, lower them with `--update`. Files of code brought in byte-exact (patches, `source/`) keep theirs.
- A single source of truth stays in code, and documents point at it instead of copying it: make targets in the comments of `Makefile` and `mk/*.mk`, build variables in `mk/config.mk`, emulator options in `mk/qemu.mk`, cookbook commands and `COOKBOOK_*` variables in `REPO_HELP_STR` (`src/bin/repo/main.rs`), recipe keys in `src/recipe.rs`. Update those when behavior changes.

# Security

FerrionOS is unhardened today. Before changing accounts, file modes, scheme lists, compiler or linker flags, a recipe's `[source]`, or a download in `mk/` or `podman/`, and before writing that FerrionOS protects against something, apply the rules below, and read `plans/security-roadmap.md` when it exists on this machine: its "FerrionOS today" table is the wording to use, and its items say where each change belongs.

- Claim a protection only with evidence that shows it on a built image, such as a boot test. Without it, write "planned" and name the roadmap item.
- The accounts in `config/dev-accounts.toml` (`root` with the password `password`, `user` with an empty password in group `sudo`) are dev credentials. `config/minimal.toml` and the configs that include `config/base.toml` directly include it; `config/base.toml` itself has no login accounts. An image that leaves the build machine carries its own accounts.
- A new input arrives pinned: a git recipe has `rev`, a tar recipe has `blake3`, and a download in a makefile or script is checked against a sha256 the way `ch_download` in `mk/ch.mk` does.
- Privilege comes from the `sudo` daemon. Files are installed without setuid or setgid bits, and a new `mode` with a setuid, setgid or world-write bit carries a comment saying why.
- A change that widens access is named in your report with its reason: a scheme added to `/etc/login_schemes.toml` or `/etc/contain.toml`, an account or group membership, a service started at boot, a hardening flag turned off for a recipe, an option added to `PODMAN_OPTIONS`.
- A hardening default ships with its toggle, its note and its check, as "How a hardening change ships" in the roadmap describes.
- A weakness found in a fetched component goes to the maintainer privately, with the file and commit, and stays out of public trackers.
- Personal information stays out of the repository and its history: no names, account handles, e-mail addresses, home-directory paths or details of anyone's machine, in files or in commit messages. Write "the maintainer" for the person who decides.

# Naming

- Write `FerrionOS` wherever text names the OS or project this repository builds: prose, comments, messages, package descriptions, labels a user sees.
- The name of the upstream project is never written in FerrionOS's text: prose, comments, messages, package descriptions, docs and links. Where the fact matters, write "upstream" ("the upstream project", "upstream's packages"). There are three exceptions:
  1. Legal notices: the copyright line in `LICENSE`, copyright and license headers in code and in `*.patch` files, and `authors` in `Cargo.toml` files.
  2. Identifiers and strings that other software matches, which would break if renamed: target triples such as `x86_64-unknown-redox`, crate, package, recipe, file and directory names (RedoxFS, relibc, `redox_syscall`, `redox.patch`), make and environment variables (`HOSTED_REDOX`), the `uname -s` value the kernel reports and code compares, `ID_LIKE` in os-release, and URLs that code or a recipe fetches. Prose may name a component by its identifier. Before renaming such a string, `rg` it across the repository, then rename every use together or keep it.
  3. Files applied byte-exact from upstream: `*.patch` files and anything under a recipe's `source/`, which are never edited.
- `README.md` and the book's introduction (`book/src/introduction.md`) each have one sentence saying FerrionOS began as a fork and is not affiliated with the upstream project; they are the only other places the name appears.

# Architectures

`ARCH` is exactly one of `x86_64`, `aarch64`, `riscv64gc`, and the directories under `config/` carry the same names. FerrionOS has only 64-bit build targets and userland. The derived names differ and are computed, never typed: the Rust target is `<arch>-unknown-redox`, the GNU triple for RISC-V is `riscv64-unknown-redox`, and QEMU's own names are in `mk/qemu.mk`.

# Display

FerrionOS has no X11: no X server, no Xwayland, no X11 client library or protocol headers in any image. Ports build with X11 disabled (meson `-Dx11=disabled`, cmake `-DWITH_X11=OFF`, configure `--without-x`, cargo features without `x11`) and use Wayland or Orbital, and a recipe never depends on an X11 library: `python3 scripts/no-x11-check.py` fails when one does. The display stack today is in [`docs/platform-state.md`](docs/platform-state.md).

# Running builds

- A cook or an image build is long and large. Start one when the task needs its result, and reach first for the cheapest check that can fail for your change: [`docs/testing.md`](docs/testing.md).
- Run make with `CI=1 COOKBOOK_LOGS=true` in the environment. Without `CI` the cookbook draws a terminal interface that needs a terminal and waits at a prompt when a recipe fails.
- Cook through `make`: it enters the container and puts the cross toolchain on `PATH`. Call `target/release/repo` directly only for its listing commands (`find`, `cook-list`).
- `source/` and `target/` under a recipe, and `build/`, `prefix/`, `repo/` and `target/` at the root, are fetched or generated. The cookbook resets and deletes `source/` trees: before editing one, read [`agents/skills/component-sources.md`](agents/skills/component-sources.md).
- `make install` and the `i.` targets extract packages into `DESTDIR`, which defaults to `/`. Pass `DESTDIR=<directory>` on any host that is not running FerrionOS.
- The bootstrap scripts clone upstream's build system and delete themselves unless called with `-d`.

# Style

- Markdown: one line per paragraph or list item, without hard wrapping.
- Indentation, line endings and final newlines follow `.editorconfig`: four spaces in Rust, shell and TOML, tabs in makefiles.
- Rust is formatted with `cargo fmt`.
