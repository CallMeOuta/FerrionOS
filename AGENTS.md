# FerrionOS

FerrionOS is a fork of the Redox OS build system: makefiles (`Makefile`, `mk/`), a Rust build tool called the cookbook (`src/`), package recipes (`recipes/`) and filesystem configs (`config/`). Together they fetch, cross-compile and package the operating system's components and assemble a bootable image. The components' own code (kernel, relibc, drivers) is fetched at build time and lives in other repositories.

# Task Guides

Read the matching guide before starting:

- Writing or changing a `recipe.toml` or its patches: [`agents/skills/recipes.md`](agents/skills/recipes.md)
- Changing what an image contains, `config/**/*.toml`: [`agents/skills/filesystem-configs.md`](agents/skills/filesystem-configs.md)
- Changing code inside a recipe's fetched `source/` (kernel, relibc, drivers, any package): [`agents/skills/component-sources.md`](agents/skills/component-sources.md)
- Changing the cookbook, the Rust code under `src/`: [`agents/skills/cookbook.md`](agents/skills/cookbook.md)
- Changing `Makefile`, `mk/*.mk`, the bootstrap scripts, `scripts/` or `podman/`: [`agents/skills/build-scripts.md`](agents/skills/build-scripts.md)
- Building an image or booting one in QEMU: [`agents/skills/images-and-qemu.md`](agents/skills/images-and-qemu.md)

# Reference

- Relying on something you remember about Redox, or a port, test or boot failing in hardware, networking, terminals and PTYs, the filesystem or POSIX and relibc: [`docs/redox-state.md`](docs/redox-state.md)
- Choosing a check, or about to start a build: [`docs/testing.md`](docs/testing.md)
- How a `make` run becomes an image, where a setting comes from, what a target name means: [`docs/build-system.md`](docs/build-system.md)
- What still comes from Redox's servers and repositories: [`docs/upstream.md`](docs/upstream.md)
- Roadmaps, when `plans/` exists on this machine (it is kept out of git, so a clone has none): `plans/kernel-roadmap.md` before setting up hosting or forks or starting kernel, relibc or RedoxFS work; `plans/product-roadmap.md` before deciding what to build next for users; `plans/linux-driver-compat.md` before porting a Linux driver; `plans/performance-roadmap.md` before changing a build flag, a hot path or a hardening default; `plans/security-roadmap.md` before any change listed under Security. Never commit them.

Documentation is split by genre. `agents/skills/` holds task procedure and `docs/` holds reference on how the build system is shaped; both are for anyone working on the codebase. The roadmaps in `plans/` stay local to the maintainer's machine. User documentation is upstream's [Redox Book](https://doc.redox-os.org/book/).

# Rules

- The code in this repository is the source of truth. The Redox Book and FAQ describe Redox and lag behind it: where they disagree with the code, follow the code.
- A failure that also happens without your change is pre-existing. Report it with its log and move on.
- Code brought in from another project keeps its licence notice and names its source. Upstream relibc has an open issue over code copied without one ([`docs/redox-state.md`](docs/redox-state.md)).
- A single source of truth stays in code, and documents point at it instead of copying it: make targets in the comments of `Makefile` and `mk/*.mk`, build variables in `mk/config.mk`, emulator options in `mk/qemu.mk`, cookbook commands and `COOKBOOK_*` variables in `REPO_HELP_STR` (`src/bin/repo/main.rs`), recipe keys in `src/recipe.rs`. Update those when behaviour changes.

# Security

FerrionOS is unhardened today. Before changing accounts, file modes, scheme lists, compiler or linker flags, a recipe's `[source]`, or a download in `mk/` or `podman/`, and before writing that FerrionOS protects against something, apply the rules below, and read `plans/security-roadmap.md` when it exists on this machine: its "FerrionOS today" table is the wording to use, and its items say where each change belongs.

- Claim a protection only with evidence that shows it on a built image, such as a boot test. Without it, write "planned" and name the roadmap item.
- The accounts in `config/base.toml` (`root` with the password `password`, `user` with an empty password in group `sudo`) are dev credentials. An image that leaves the build machine carries its own accounts.
- A new input arrives pinned: a git recipe has `rev`, a tar recipe has `blake3`, and a download in a makefile or script is checked against a sha256 the way `ch_download` in `mk/ch.mk` does.
- Privilege comes from the `sudo` daemon. Files are installed without setuid or setgid bits, and a new `mode` with a setuid, setgid or world-write bit carries a comment saying why.
- A change that widens access is named in your report with its reason: a scheme added to `/etc/login_schemes.toml` or `/etc/contain.toml`, an account or group membership, a service started at boot, a hardening flag turned off for a recipe, an option added to `PODMAN_OPTIONS`.
- A hardening default ships with its toggle, its note and its check, as "How a hardening change ships" in the roadmap describes.
- A weakness found in a fetched component goes to the maintainer privately, with the file and commit, and stays out of public trackers.
- Personal information stays out of the repository and its history: no names, account handles, e-mail addresses, home-directory paths or details of anyone's machine, in files or in commit messages. Write "the maintainer" for the person who decides.

# Naming

- Write `FerrionOS` wherever text names the OS or project this repository builds: prose, comments, messages, package descriptions, labels a user sees.
- `Redox` stays for what upstream owns (the Redox OS project, its team, nonprofit, trademark, website, Book, GitLab and chat, published titles, statements about upstream) and inside identifiers and compound names: RedoxFS, Redoxer, relibc, libredox, `redox_syscall`, target triples such as `x86_64-unknown-redox`, recipe, file and directory names, URLs, any lowercase `redox`.
- `Redox` also stays in a string that something outside this repository matches: the kernel answers `uname -s` with `Redox`, and third-party build scripts list it as an OS name. Before renaming such a string, `rg` it across the repository, then rename every use together or keep it.
- `*.patch` files, `LICENSE` and `TRADEMARK.md` keep their text.

# Architectures

`ARCH` is exactly one of `x86_64`, `i586`, `aarch64`, `riscv64gc`, and the directories under `config/` carry the same names. `i686` on the make command line is rejected. The derived names differ and are computed, never typed: the Rust target is `<arch>-unknown-redox`, the GNU triple for RISC-V is `riscv64-unknown-redox`, and QEMU's own names are in `mk/qemu.mk`.

# Running builds

- A cook or an image build is long and large. Start one when the task needs its result, and reach first for the cheapest check that can fail for your change: [`docs/testing.md`](docs/testing.md).
- Run make with `CI=1 COOKBOOK_LOGS=true` in the environment. Without `CI` the cookbook draws a terminal interface that needs a terminal and waits at a prompt when a recipe fails.
- Cook through `make`: it enters the container and puts the cross toolchain on `PATH`. Call `target/release/repo` directly only for its listing commands (`find`, `cook-list`).
- `source/` and `target/` under a recipe, and `build/`, `prefix/`, `repo/` and `target/` at the root, are fetched or generated. The cookbook resets and deletes `source/` trees: before editing one, read [`agents/skills/component-sources.md`](agents/skills/component-sources.md).
- `make install` and the `i.` targets extract packages into `DESTDIR`, which defaults to `/`. Pass `DESTDIR=<directory>` on any host that is not running FerrionOS.
- The bootstrap scripts clone upstream Redox and delete themselves unless called with `-d`.

# Style

- Markdown: one line per paragraph or list item, without hard wrapping.
- Indentation, line endings and final newlines follow `.editorconfig`: four spaces in Rust, shell and TOML, tabs in makefiles.
- Rust is formatted with `cargo fmt`.
