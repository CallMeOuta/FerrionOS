# Drivers live in ferrion-hardware, are GPL-2.0-only, and get the hardware right only when signed

Status: accepted; moved in the monorepo cutover (ADR 0027), the signature check is planned (security roadmap S13)

## Context

ADR 0027 brings the kernel, relibc, base and RedoxFS into this repository. base holds the system's services and, under `drivers/`, every driver FerrionOS has: 47 crates of base's 65 at the revision pinned on 2026-11-22. Three things about drivers differ from the rest of the system. They are where outside contributors can help most, one device at a time, without learning the build system. They are what a hardware vendor would most like to ship closed. And a driver process holds the hardware right, with which it can reach memory and devices no other program can.

ferrion-hardware (github.com/CallMeOuta/ferrion-hardware) already exists for the Linux driver host (ADR 0046) and for per-machine profiles. On 2026-11-22 the maintainer decided that FerrionOS's own drivers belong there too, under the license of Linux's drivers, and that a driver gets the hardware right only when FerrionOS built and signed it. The maintainer chose this over keeping the drivers here until the driver interface is stable, knowing that a change spanning the system and a driver becomes paired commits in two repositories.

At the pin nothing in base outside `drivers/` depends on a crate inside it, and the crates inside depend on seven of FerrionOS's: `daemon`, `scheme-utils`, `config`, the library of `audiod`, `graphics-ipc`, the library of `inputd`, and relibc's `redox-ioctl`. Two crates under `drivers/` are linked by code outside it: `graphics-ipc` and `inputd`, by the compositor's backend and by Orbital.

## Decision

- **Where.** base's `drivers/` tree is the `drivers/` folder of ferrion-hardware, moved in the monorepo cutover as that tree at the pinned upstream revision with the driver parts of FerrionOS's patches as commits. Everything else of base is `components/base` here. `components/base` has no `drivers/` folder.
- **The boundary.** A crate that code in this repository links stays here under the MIT license: `graphics-ipc` and `inputd` stay, as `components/base/graphics-ipc` and `components/base/inputd`, and `inputd` no longer uses the drivers' library. Everything else under upstream's `drivers/` moves, the libraries drivers share (PCI, block, network, display, virtio, memory and port access) included. The rest of the system reaches a driver only through its scheme.
- **The license.** ferrion-hardware is GPL-2.0-only as a whole, so that no driver of FerrionOS's can be made proprietary and a driver built on the shared libraries is GPL too. What came from upstream came under the MIT license and keeps its notice: upstream's `LICENSE` is kept byte for byte as `drivers/LICENSE-MIT`, a file from upstream starts with `SPDX-License-Identifier: GPL-2.0-only AND MIT`, and a file FerrionOS adds with `SPDX-License-Identifier: GPL-2.0-only`. No code under the MIT license in this repository links a crate of ferrion-hardware's; `scripts/components-check.py` fails when a manifest does. A driver that cannot be combined with GPL-2.0-only code is not brought in: `ixgbed`, which is under the GNU Affero GPL version 3.
- **The pin.** This repository pins ferrion-hardware by revision in one recipe and builds its drivers against this repository's own seven crates, with a lock file kept here. ferrion-hardware names a FerrionOS revision only for builds of itself alone. A change on both sides lands as FerrionOS's half in a form the pinned drivers still build with, then the driver's commit, then the pin.
- **Packages.** The `drivers` package holds what ferrion-hardware builds, with both license texts and the revision it was built from; the boot file system is assembled from `base` and `drivers` by its own recipe.
- **Contributions.** A contributor's commit in ferrion-hardware carries the contributor's own name and e-mail as its author; the maintainer merges and pushes. This repository's history keeps its one identity.
- **Signed drivers.** The hardware right goes only to a driver binary that was built from ferrion-hardware's public source and signed by FerrionOS's release key, checked before the right is granted. The machine's owner can allow one specific other driver by an explicit choice that is recorded and shown, in the manner of enrolling a key for Secure Boot. Nothing else gets the right. This is planned: until its check passes on a built image, FerrionOS's text says "planned".

## Consequences

- Anyone can read, build and change a driver in a repository of its own, and type-check it with a Rust toolchain alone. Whether it works is still shown by FerrionOS's checks, which stay here and are run before the pin moves.
- A change to a driver is a commit in ferrion-hardware and a pin move here. A change across the system and a driver is three commits in two repositories instead of one. A change to a wire type that the drivers compile from this repository is still one.
- The GPL covers FerrionOS's changes from the cutover on and the drivers as ferrion-hardware ships them. It does not reach back: upstream's drivers remain available from upstream under the MIT license, and FerrionOS's patches to them, published in this repository under the MIT license until the cutover, remain so in its history.
- The license cannot stop a closed driver written from nothing; the signature rule is what does, once it is built. Until then a driver binary written to the root file system gets the right at the next boot, and FerrionOS claims no protection here.
- An image carries GPL programs beside MIT ones. They are separate programs; the `drivers` package ships the license texts and names its source.
- Images lose the Intel 10 GbE driver `ixgbed` until Linux's driver runs in the driver host or FerrionOS writes its own.
- An upstream revision of base is merged twice, once in each repository, from the same revision.
- The compositor's backend and Orbital link MIT crates only. Code under GPL version 3 could not link a GPL-2.0-only one.

## Where

- `components/upstream.toml`, `[base.split]`: what moved, what was kept and under which path, what was dropped
- `recipes/core/drivers/recipe.toml` (the pin) and `Cargo.lock`, `scripts/drivers-lock.sh`; `recipes/core/initfs/recipe.toml`
- `scripts/components-check.py`: no `components/base/drivers`, no manifest that names ferrion-hardware
- In ferrion-hardware: `drivers/`, `drivers/upstream.toml`, `drivers/LICENSE-MIT`, `scripts/license-check.py`, `scripts/ferrion-rev.sh`, `scripts/drivers-upstream.sh`, `CONTRIBUTING.md`
- `plans/security-roadmap.md` S13 (on the maintainer's machine) for the signature rule until it is built
