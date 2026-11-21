# Checks

What can be checked in this repository, cheapest first: what each check proves and what it needs. Climb only as far as the change requires, and name in your report the highest rung you reached and what you could not run.

## The ladder

| Rung | Command | Proves | Needs |
|---|---|---|---|
| 1. Syntax | `bash -n <script>` for shell, `python3 -c 'import sys,tomllib; [tomllib.load(open(f,"rb")) for f in sys.argv[1:]]' <file.toml>` for TOML, `make -n <target>` for makefiles, `python3 scripts/security-audit.py` after a change to accounts, a recipe's `[source]` or a download, `python3 scripts/naming-check.py` after a change to any text, `python3 scripts/no-x11-check.py` after a change to a recipe's dependencies or a config's packages | The file parses; for make, which commands a target would run; for the audit, that the change adds no security finding beyond `scripts/security-audit.baseline.jsonl` (exit code 1 when it does, the script's header lists the checks); for the naming check, that no new line names the upstream project outside the exceptions of `AGENTS.md` "Naming" and `scripts/naming-check.baseline.jsonl`; for the X11 check, that no recipe depends on an X11 package and no config installs one. | Nothing. |
| 2. Cookbook | `cargo fmt -- --check`, `cargo test --locked` | Formatting and the unit tests in `src/`. | The Rust toolchain pinned in `rust-toolchain.toml`, and network on the first run: `Cargo.toml` takes four git repositories from upstream's GitLab ([`upstream.md`](upstream.md)). |
| 3. Graph | `make rt.<recipe>`, `make repo-tree`, `make image-tree` | The recipe matches the schema, and every dependency and every package of the config names an existing recipe. | The cookbook built (`build/fstools.tag`). |
| 4. Fetch | `make f.<recipe>` | The source URL answers, the `blake3` matches and the patches apply. | Network. |
| 5. Cook | `make r.<recipe>` | The recipe builds and packages for one architecture. | The container or the native tools, the prefix, and its build dependencies cooked. |
| 6. Image | `make image`, `make qemu`, `scripts/vm-test.py boot` | The config assembles into an image, and the image boots; with the script, that the boot stays quiet and reaches the login (see [`agents/skills/images-and-qemu.md`](../agents/skills/images-and-qemu.md)). | Every package of the config cooked. |
| 7. In the OS | A config whose init script runs a suite (below) | Behavior inside FerrionOS. | Rung 6. |

`make -n` still parses every makefile. With `PODMAN_BUILD=0`, which image targets also pass to a nested make, that parse calls `rustc`, and rustup then installs the pinned toolchain when it is missing.

## Spending a run well

- A cook or an image build is the most expensive action here. A first `make all` builds the container, the toolchain prefix and every package of the config, and the book's [Developer FAQ](../book/src/developer-faq.md) asks for "50GB of storage space or more". Start one when the task needs its result, once, at the end.
- Cook with `CI=1 COOKBOOK_LOGS=true` in the environment. `CI` turns off the cookbook's terminal interface, which needs a terminal and waits at a prompt when a recipe fails. `COOKBOOK_LOGS=true` keeps each recipe's output in `build/logs/<target>/<recipe>.log`.
- A Podman build runs `podman run --interactive --tty` (`PODMAN_OPTIONS` in `mk/podman.mk`), and Podman's [manual](https://docs.podman.io/en/latest/markdown/podman-run.1.html) says of `--tty`: "This option is only used when run interactively in a terminal." Start a Podman-mode make from a terminal or a pseudo-terminal.
- Write the whole output of a long command to a log file and read it from there. What `grep`, `head` or `tail` drop from a pipe costs a whole run to get back.
- Run a check again only after changing something it covers. A cook is incremental: an unchanged recipe reports `cached`.
- A failure in a recipe or test you did not touch is usually pre-existing. Git recipes without `rev` follow upstream's branch head, so any fetch can bring in a broken commit; `target/<target>/source_info.toml` in the recipe directory records the revision that was built. Check [`platform-state.md`](platform-state.md) for a known limit, report the failure with its log, and keep it out of your change.

## Suites that run inside the OS

Each is a filesystem config whose init script starts the suite and ends with `shutdown`, so QEMU exits when the run is over:

- `config/acid.toml`: `acid-runner`.
- `config/auto-test.toml`: `acid-runner`, relibc's tests and `os-test-runner`.
- `config/os-test.toml`: `os-test-runner`.
- `config/sys-build.toml`: cooks the system on FerrionOS itself.

`make ci-os-test` in `mk/ci.mk` is the worked example: it boots `CONFIG_NAME=os-test` with `gpu=no`, then mounts the image and copies the results to `build/os-test/<arch>/`. [`agents/skills/images-and-qemu.md`](../agents/skills/images-and-qemu.md) has the procedure for a suite of your own.

The x86_64 desktop's command and name-resolution regressions run with `python3 scripts/vm-test.py cli-tools resolver --arch x86_64 --config desktop --timeout 120`. `cli-tools` checks GNU sed, GNU awk and extrautils' gzip tar round-trip with `-C`; it does not cover process arguments. `resolver` checks hosts-file names as an ordinary user and captures DHCP and DNS packets to compare the queried server with the advertised server. Both need an image rebuilt with the recipe patches; installing a new libc alone does not update programs that linked it statically.

The package commands run with `python3 scripts/vm-test.py ferrion-cli update-today --arch x86_64 --config desktop --timeout 120`. `ferrion-cli` reaches no outside server: it checks that `ferrion search` refuses with exit code 6 while the image names no package source, then adds a source served from the host for the check, holding one package of this build's `repo/` (`gnu-make`, which the desktop image lacks) signed by the build's key, and installs, runs and removes it; `gnu-make` must be cooked. `update-today` runs `ferrion update` and `pkg update` as root with the network up and passes when nothing was fetched: no TCP connection or DNS question in a capture of `net0`, the hashes of `usr/lib/boot/kernel` and `usr/lib/boot/initfs` unchanged, and the next start reaching the login. That a replaced kernel or initfs is refused at the next start is `scripts/secure-boot-test.sh`'s tamper cases.

The GPU host reference runs with `nice -n 10 python3 scripts/gpu-reference-guest.py --vk-driver /usr/share/vulkan/icd.d/intel_icd.json --render-node /dev/dri/renderD128` (choose the host's actual ICD and render node; `VK_DRIVER_FILES` also supplies the ICD). It needs Python 3.11+, x86_64 Linux with accessible KVM, QEMU with Venus and `egl-headless`, and working host graphics drivers. It needs no FerrionOS build or host root: checksum-pinned Debian packages become an initramfs, whose programs execute only inside an offline, windowless QEMU guest with `-snapshot`. First use downloads the inputs; `--fetch-only` populates the cache, and subsequent runs check every cached hash. The pins and the three device lines live in the script. `vulkaninfo`, `eglinfo`, and the host package manager add runtime and package versions when installed; an unusable host or a driver mismatch is `BLOCKED` (exit 2).

By default the reference boots each device line twice in two guest variants and compares scene hashes, differing-pixel counts, and the full Venus capset bytes. `--variant debian13` selects the shared Debian 13 baseline; `--variant mesa26` selects that base with newer Mesa and its minimal runtime dependency overlay from a signed unstable snapshot. Both retain the Debian 13 kernel. The record labels each variant with its Mesa, glibc, LLVM and snapshot versions, and lists every input hash; the overlay includes no init system. Its Mesa is the newest available in the pinned unstable index and can lag FerrionOS's target release. Each invocation writes one `build/gpu-reference/run-*/result.json`, with per-boot serial logs and `virtio_gpu_*` QEMU traces beside it. For a capset comparison, select the matching variant and the same host GPU and QEMU device line, then read `variants[].runs[].guest.venus_capset` (`bytes_hex`, byte count and SHA-256).

`PASS` checks the pinned guest's available renderers and host submission evidence; it does not require Vulkan 1.4 or Intel native contexts from a guest whose Mesa and kernel predate them. Unsupported native renderers are named in the record. This is a Linux reference for the host, not proof of FerrionOS GPU support. `python3 scripts/gpu-reference/test_reference.py` checks checksum rejection, host floors, and repeatability without downloading or booting.

## What upstream's CI ran

Upstream gates its merge requests on a `.gitlab-ci.yml` that is not part of this checkout, and FerrionOS has no CI of its own yet. The upstream jobs, read on 2026-11-20:

- `cargo fmt -- --check`
- `RUSTFLAGS="-Dwarnings" cargo test --locked`
- `make CONFIG_NAME=ci SKIP_CHECK_TOOLS=1 repo-tree ARCH=<arch>` and the same for `CONFIG_NAME=demo`, with `PODMAN_BUILD=0`, for each supported architecture: `x86_64`, `aarch64` and `riscv64gc`
- `./scripts/fetch-changed.sh`, which fetches every recipe whose `recipe.toml` differs from `origin/master` and so needs a git checkout
- `make desktop` with `REPO_BINARY=1` and `FSTOOLS_NO_MOUNT=1`, an image assembled from prebuilt packages. Here that job also needs `VERIFIED_BOOT=0`: verified-boot signing mounts the image through FUSE, which `FSTOOLS_NO_MOUNT=1` leaves out
