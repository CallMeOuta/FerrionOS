# Checks

What can be checked in this repository, cheapest first: what each check proves and what it needs. Climb only as far as the change requires, and name in your report the highest rung you reached and what you could not run.

## The ladder

| Rung | Command | Proves | Needs |
|---|---|---|---|
| 1. Syntax | `bash -n <script>` for shell, `python3 -c 'import sys,tomllib; [tomllib.load(open(f,"rb")) for f in sys.argv[1:]]' <file.toml>` for TOML, `make -n <target>` for makefiles | The file parses; for make, which commands a target would run. | Nothing. |
| 2. Cookbook | `cargo fmt -- --check`, `cargo test --locked` | Formatting and the unit tests in `src/`. | The Rust toolchain pinned in `rust-toolchain.toml`, and network on the first run: `Cargo.toml` takes four git repositories from gitlab.redox-os.org. |
| 3. Graph | `make rt.<recipe>`, `make repo-tree`, `make image-tree` | The recipe matches the schema, and every dependency and every package of the config names an existing recipe. | The cookbook built (`build/fstools.tag`). |
| 4. Fetch | `make f.<recipe>` | The source URL answers, the `blake3` matches and the patches apply. | Network. |
| 5. Cook | `make r.<recipe>` | The recipe builds and packages for one architecture. | The container or the native tools, the prefix, and its build dependencies cooked. |
| 6. Image | `make image`, `make qemu` | The config assembles into an image, and the image boots. | Every package of the config cooked. |
| 7. In the OS | A config whose init script runs a suite (below) | Behaviour inside FerrionOS. | Rung 6. |

`make -n` still parses every makefile. With `PODMAN_BUILD=0`, which image targets also pass to a nested make, that parse calls `rustc`, and rustup then installs the pinned toolchain when it is missing.

## Spending a run well

- A cook or an image build is the most expensive action here. A first `make all` builds the container, the toolchain prefix and every package of the config, and Redox's [Developer FAQ](https://doc.redox-os.org/book/developer-faq.html) asks for "50GB of storage space or more". Start one when the task needs its result, once, at the end.
- Cook with `CI=1 COOKBOOK_LOGS=true` in the environment. `CI` turns off the cookbook's terminal interface, which needs a terminal and waits at a prompt when a recipe fails. `COOKBOOK_LOGS=true` keeps each recipe's output in `build/logs/<target>/<recipe>.log`.
- A Podman build runs `podman run --interactive --tty` (`PODMAN_OPTIONS` in `mk/podman.mk`), and Podman's [manual](https://docs.podman.io/en/latest/markdown/podman-run.1.html) says of `--tty`: "This option is only used when run interactively in a terminal." Start a Podman-mode make from a terminal or a pseudo-terminal.
- Write the whole output of a long command to a log file and read it from there. What `grep`, `head` or `tail` drop from a pipe costs a whole run to get back.
- Run a check again only after changing something it covers. A cook is incremental: an unchanged recipe reports `cached`.
- A failure in a recipe or test you did not touch is usually pre-existing. Git recipes without `rev` follow upstream's branch head, so any fetch can bring in a broken commit; `target/<target>/source_info.toml` in the recipe directory records the revision that was built. Check [`redox-state.md`](redox-state.md) for a known limit, report the failure with its log, and keep it out of your change.

## Suites that run inside the OS

Each is a filesystem config whose init script starts the suite and ends with `shutdown`, so QEMU exits when the run is over:

- `config/acid.toml`: `acid-runner`.
- `config/auto-test.toml`: `acid-runner`, relibc's tests and `os-test-runner`.
- `config/os-test.toml`: `os-test-runner`.
- `config/sys-build.toml`: cooks the system on FerrionOS itself.

`make ci-os-test` in `mk/ci.mk` is the worked example: it boots `CONFIG_NAME=os-test` with `gpu=no`, then mounts the image and copies the results to `build/os-test/<arch>/`. [`agents/skills/images-and-qemu.md`](../agents/skills/images-and-qemu.md) has the procedure for a suite of your own.

## What upstream's CI ran

Redox gates its merge requests on a `.gitlab-ci.yml` that is not part of this checkout, and FerrionOS has no CI of its own yet. The upstream jobs, read on 2026-11-20:

- `cargo fmt -- --check`
- `RUSTFLAGS="-Dwarnings" cargo test --locked`
- `make CONFIG_NAME=ci SKIP_CHECK_TOOLS=1 repo-tree ARCH=<arch>` and the same for `CONFIG_NAME=demo`, with `PODMAN_BUILD=0`, for each of the four architectures
- `./scripts/fetch-changed.sh`, which fetches every recipe whose `recipe.toml` differs from `origin/master` and so needs a git checkout
- `make desktop` with `REPO_BINARY=1` and `FSTOOLS_NO_MOUNT=1`, an image assembled from prebuilt packages
