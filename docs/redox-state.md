# Redox as of 2026-11-20

FerrionOS builds Redox's components ([`upstream.md`](upstream.md)), so Redox's state is FerrionOS's state until a component changes here. This page keeps the facts that change how you work: beliefs about Redox that are out of date, limits that explain a failing port, test or boot, and places where upstream's documentation is wrong. Every fact is as of 2026-11-20 and carries its source. Confirm a fact at its source before a decision rests on it.

Upstream develops on gitlab.redox-os.org. On 2026-11-20 its web pages answered scripted requests with a Cloudflare challenge (HTTP 403) while git over HTTPS kept working: open its links in a browser, and read source with git or through the GitHub mirrors at `github.com/redox-os/<repo>`, which exist for kernel, relibc, redoxfs, syscall and orbital among others. `base` has no mirror.

## Out of date: what you may remember

| Remembered | State now | Source |
|---|---|---|
| "Scheme URLs instead of Unix paths break ports" | "The Unix path format replaced the previous URI format" | [0.9.0 release notes](https://www.redox-os.org/news/release-0.9.0/) |
| "No dynamic linking" | RISC-V was "the last CPU architecture using static linking in our build system" and is now fixed | [October 2026](https://www.redox-os.org/news/this-month-261031/) |
| "rustc and cargo do not run on Redox" | "Cargo and `rustc` are now working on Redox!" Full self-hosting is still open, see below. | [January 2026](https://www.redox-os.org/news/this-month-260131/) |
| "Signals and process management are incomplete" | The NLnet signals project concluded | [February 2026](https://www.redox-os.org/news/this-month-260228/) |
| "No Unix domain sockets or fd passing" | Implemented in RSoC 2025 | [RSoC 2025 report](https://www.redox-os.org/news/rsoc-2025-fdtbl/) |
| "Single-core by default" | "fixed some key bugs in multi-threading and enabled it by default for x86" | [September 2025](https://www.redox-os.org/news/this-month-250930/) |
| "No real browser" | WebKitGTK is "the first full-featured browser engine ported to Redox" | [November 2025](https://www.redox-os.org/news/this-month-251130/) |
| "Build system is shell scripts" | Cookbook scripts were "reimplemented ... in Rust" | [October 2025](https://www.redox-os.org/news/this-month-251031/) |
| "POSIX compliance is poor" | 84% on os-test, level with Solaris, above macOS and Minix. Weak suites remain, see below. | [os-test](https://sortix.org/os-test/) |
| "Bugs filed on GitHub vanish" | GitHub issues are disabled and the repo is described as "Mirror of https://gitlab.redox-os.org/redox-os/redox" | [GitHub API](https://api.github.com/repos/redox-os/redox) |

## Limits that explain a failure

### Architectures

- x86_64 is the only broadly exercised target. Published packages: [x86_64](https://static.redox-os.org/pkg/x86_64-unknown-redox/) 489, [aarch64](https://static.redox-os.org/pkg/aarch64-unknown-redox/) 115, [i586](https://static.redox-os.org/pkg/i586-unknown-redox/) 99, [riscv64gc](https://static.redox-os.org/pkg/riscv64gc-unknown-redox/) 83. Prove a change on x86_64 first; a recipe that fails elsewhere has probably never built there.
- "(ARM64 has limited support)" ([book: Working with ARM64](https://doc.redox-os.org/book/aarch64.html)), and an aarch64 boot panic has been open since 2023-05-11 ([redox#1374](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1374)).
- "RiscV fails to boot", open since 2026-01-18 ([redox#1774](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1774)).

### Hardware

- `HARDWARE.md` in this repository lists what holds on every machine: "Wi-Fi and Bluetooth aren't supported yet", "I2C devices aren't supported yet", "ACPI support is incomplete (some things are hardcoded in the kernel to work)", "Only Intel GPUs are supported, other GPU vendors use BIOS VESA or UEFI GOP" ([HARDWARE.md](https://gitlab.redox-os.org/redox-os/redox/-/blob/master/HARDWARE.md)).
- The same file holds 39 reports: 3 "Recommended", 19 "Booting", 17 "Broken", and all 3 "Recommended" rows are Redox 0.8.0 images dated 2022-11-11. Verify in QEMU; a failed boot on a physical machine is the expected case.
- Graphics are drawn by the CPU: "As we don't support 2D/3D hardware acceleration yet, we use LLVMpipe/Lavapipe" ([book: Windowing and Graphics](https://doc.redox-os.org/book/graphics-windowing.html)).
- The driver tree has five network drivers (`e1000d`, `ixgbed`, `rtl8139d`, `rtl8168d`, `virtio-netd`) and one USB host-controller driver (`xhcid`) ([base/drivers](https://gitlab.redox-os.org/redox-os/base/-/tree/main/drivers), at commit `20f7573b`); "Missing EHCI controller driver" ([base#56](https://gitlab.redox-os.org/redox-os/base/-/work_items/56)). Emulate devices from that list, as the `net=` choices in `mk/qemu.mk` do.

### Networking

- IPv4 only: the stack builds smoltcp with `proto-ipv4` only, so there is no IPv6 ([netstack/Cargo.toml](https://gitlab.redox-os.org/redox-os/base/-/blob/main/netstack/Cargo.toml), at `20f7573b`). Turn IPv6 off in a port, as `recipes/net/curl` and `recipes/dev/git` do.
- os-test scores the udp suite 52% (70/134) ([os-test](https://sortix.org/os-test/)).

### Terminals, POSIX and relibc

- os-test scores Redox 84% (3875/4594). Weak suites: pty 34% (10/29), fnmatch 18% (7/37) ([os-test](https://sortix.org/os-test/)).
- "Controlling Terminal needs a proper implementation" ([Development Priorities 2025/26](https://www.redox-os.org/news/development-priorities-2025-09/); [redox#1424](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1424), open since 2024-01-09). A port that needs job control or a controlling terminal fails for this known reason.
- Functions still absent from relibc are tracked in [Missing POSIX APIs](https://gitlab.redox-os.org/redox-os/relibc/-/work_items/173).
- Tests hang in `waitpid` and `ppoll` ([redox#1752](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1752)); `recipes/tests/os-test-bins/recipe.toml` skips them.
- "we have trouble with languages that come with their own runtime" ([Development Priorities 2025/26](https://www.redox-os.org/news/development-priorities-2025-09/)).
- There is no Linux binary compatibility; programs are ported from source by design ([Developer FAQ](https://doc.redox-os.org/book/developer-faq.html)).
- "The kernel syscall interface itself is unstable" ([book: How Redox Compares](https://doc.redox-os.org/book/how-redox-compares.html)), and Rust targets "are only expected to work with the latest version of Redox OS as the ABI is not yet stable" ([rustc book](https://doc.rust-lang.org/nightly/rustc/platform-support/redox.html)). The Book places the stable interface in the libraries above the kernel: "Currently only libredox will have a stable ABI, relibc will be unstable only as long as it's under heavy development and redox_syscall will remain unstable even after the 1.0 version of Redox" ([book: Libraries and APIs](https://doc.redox-os.org/book/libraries-apis.html)). Write a port against relibc or `libredox`, not against raw system calls.
- Ported programs known to misbehave, "as of August 2026": Servo "Often Crashes", Neovim "Crashes", Zsh "Hangs", LuaJit "Often Crashing" ([book: Important Programs](https://doc.redox-os.org/book/important-programs.html)).

### Filesystem

- RedoxFS answers requests on one thread: "Run scheme requests in a thread pool" is unchecked ([redoxfs#57](https://gitlab.redox-os.org/redox-os/redoxfs/-/work_items/57)).
- RedoxFS has no fsck or repair tool: the repository ships five tools (mount, `ar`, `clone`, `mkfs`, `resize`) ([redoxfs/Cargo.toml](https://gitlab.redox-os.org/redox-os/redoxfs/-/blob/master/Cargo.toml), at `5ba45a68`). A damaged image is rebuilt, not repaired.
- A "No space left on device" bug, reported through the FUSE build on Linux, has been open since 2024-10-21 ([redoxfs#50](https://gitlab.redox-os.org/redox-os/redoxfs/-/work_items/50)). Images here are written through that FUSE build: when the installer reports it, raise `filesystem_size` first, then suspect this bug.

### Debugging, virtual machines and self-hosting

- There is no native debugger: "Redox's `ptrace` implementation needs to be reworked" ([Development Priorities 2025/26](https://www.redox-os.org/news/development-priorities-2025-09/)), and [redox#1777](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1777) is open. Debug from the host: `make qemu gdb=yes` with `make gdb`, or `scripts/backtrace.sh`.
- Virtual machines that run on Redox are unaccelerated: "Redox does not yet have support for KVM-like virtual machine acceleration, so performance can be significantly slow" ([August 2026](https://www.redox-os.org/news/this-month-260831/)). This limits guests of Redox. Redox as a guest is accelerated: `make qemu` enables KVM when `ARCH` matches a Linux host.
- Building the system on itself is partial. The tracker ticks only relibc and acid; bootloader, kernel, redoxfs, base, ion and orbital are unchecked ([redox#1872](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1872)), and cmake, meson, ninja-build, perl, bison, flex and m4 are unchecked in the toolchain tracker ([redox#1390](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1390)). Build on a Linux host.

### Open bugs that can look like yours

- Memory is leaked "on every process spawn" ([kernel#192](https://gitlab.redox-os.org/redox-os/kernel/-/work_items/192)).
- Kernel allocation failures still need to "return to userspace with `ENOMEM` instead of panicking" ([kernel#78](https://gitlab.redox-os.org/redox-os/kernel/-/work_items/78)).
- "\"pkill\" randomly not working" ([redox#1863](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1863)).

## Upstream's own checks are red

"It has been a longstanding issue that the CI for tests for redox relibc x86_64 has been flakey" ([relibc#293](https://gitlab.redox-os.org/redox-os/relibc/-/work_items/293)). Of the last 100 default-branch pipelines, relibc had 78 failed and 14 passed, kernel 51 failed and 46 passed, redoxfs 87 failed and 11 passed, and relibc's `test:x86_64` job failed in 22 of its 25 most recent failed pipelines (GitLab API query of 2026-11-20, `/pipelines?ref=<default>&per_page=100`). Upstream's branch head is therefore not a known-good state, and a recipe that follows it can break from one fetch to the next.

## Where upstream's documentation is wrong

The Book says of itself that it "sometimes can be outdated" ([book: Introduction](https://doc.redox-os.org/book/)). Checked examples:

- The [Developer FAQ](https://doc.redox-os.org/book/developer-faq.html) sends driver authors to the old [`drivers` repo README](https://gitlab.redox-os.org/redox-os/drivers/-/blob/master/README.md), which lists `alxd` and `bgad`; neither exists in [base/drivers](https://gitlab.redox-os.org/redox-os/base/-/tree/main/drivers), where drivers now live.
- The FAQ credits RedoxFS with "volumes, snapshots" ([FAQ](https://www.redox-os.org/faq/)); neither word appears in the RedoxFS source or README, and "Volume management" is unchecked in [redoxfs#42](https://gitlab.redox-os.org/redox-os/redoxfs/-/work_items/42).
- The feature table says NUMA "No (planned)" ([book: Features](https://doc.redox-os.org/book/features.html)); NUMA was tested on real hardware in [October 2026](https://www.redox-os.org/news/this-month-261031/).
- The real-hardware page says "USB devices are not yet supported during run-time" and, a paragraph later, "Only USB input devices (HID) work" ([book: Real Hardware](https://doc.redox-os.org/book/real-hardware.html)).
- The status table lists Rust 1.90 under "Can compile programs on Redox?" as "Always Crashing" ([book: Important Programs](https://doc.redox-os.org/book/important-programs.html)), against the January 2026 news in the table above.
- Against this repository: the Book's [Application Porting](https://doc.redox-os.org/book/porting-applications.html) page gives `make ur.recipe-name` for a fresh build, and `mk/repo.mk` has no `ur.%` target (`ucr.%` unfetches, cleans and cooks). The October 2026 news above calls RISC-V dynamic linking fixed, and `DYNAMIC_INIT` in `src/cook/script.rs` still returns early for `riscv64gc-unknown-redox`.

## Security: unhardened

- "I just discovered we don't do permission checks for setuid..." ([base#92](https://gitlab.redox-os.org/redox-os/base/-/work_items/92), opened 2026-03-01, still open).
- "Exploit Mitigations: No (planned)" ([book: Features](https://doc.redox-os.org/book/features.html)). The tracking issue ticks only SMAP, SMEP and a zero-initialised user stack ([redox#1393](https://gitlab.redox-os.org/redox-os/redox/-/work_items/1393)).
- Spectre v1, Spectre v2 and MDS mitigations are unchecked, and the list has "Meltdown (PTI - unfinished)" ([kernel#124](https://gitlab.redox-os.org/redox-os/kernel/-/work_items/124)).
- "we have not implemented support for IOMMU yet" ([Development Priorities 2025/26](https://www.redox-os.org/news/development-priorities-2025-09/)).
- Capabilities are mid-migration: "many system services and drivers use effective UID and GID as the primary mechanism for security" ([January 2026](https://www.redox-os.org/news/this-month-260131/)), and the sandbox tool `contain` is "a partial implementation" ([Developer FAQ](https://doc.redox-os.org/book/developer-faq.html)).

Describe FerrionOS's security as it is in any text you write. A test or design that leans on setuid checks, user separation or `contain` as a boundary rests on an open gap.

## Provenance

A `critical` relibc issue says: "In some cases, code may have been copied from musl without citing musl as the source", needing "license clarification" ([relibc#195](https://gitlab.redox-os.org/redox-os/relibc/-/work_items/195), open since 2024-09-09). Code you bring in from another project keeps its licence notice and names its source.
