# App manifests, undeclared means denied, and outside software through the broker

Status: accepted

## Context

The maintainer, 2026-11-23, chose a permission model after macOS and GrapheneOS: anything can be installed, software from outside FerrionOS's repository asks before its first start, and every program is sandboxed either way. The design follows the maintainer's plan.

The parts exist or are planned separately. `ferrion-run` ([ADR 0014](0014-ferrion-run.md)) starts a program in a narrower namespace from a profile that holds only `network` and `allow_sudo`; `ferrion-scope` serves a session's file scheme from its grants; netstack's views (NW5), the compositor's capabilities and the session broker, `audiod`'s grants and the capture service are each planned with their own checks. Nothing ties them to one statement of what a program may ask for, nothing records what the owner gave it, nothing distinguishes a program from the repository from one from elsewhere, and any user can execute any file it can write, so a sandbox that a launcher applies can be stepped around.

After the monorepo cutover ([ADR 0027](0027-monorepo.md)), the four core trees (`components/kernel`, `components/relibc`, `components/base`, `components/redoxfs`) live directly in this repository, drivers live in `ferrion-hardware` ([ADR 0047](0047-drivers-in-ferrion-hardware.md)), and the native UI toolkit lives in `components/ferrion-ui` (with headless rendering and Wayland popup support landed at `ffaf93ad6`). In `config/x86_64/desktop.toml`, `ferrion-run` has only one version-1 profile (`ferrion-run-probe.toml`, a test helper kept on profile v1 until PFR1 replaces profiles), so slice PFR0 covers the four ported desktop apps the image installs (`cosmic-edit`, `cosmic-files`, `cosmic-term`, `mpv`) and classifies every remaining package explicitly in a bounded non-app list.

## Decision

### The manifest

- Every program a person or another program starts through the broker or a launcher has a manifest, `format = 1`; daemons, drivers (`drivers`, `initfs`), base-system tools (`ferrion-run`, `installer-gui`), legacy shell packages (`orbutils`, which bundles `orblogin` and `launcher` and leaves with Orbital), libraries, fonts, icons, command-line tools and test probes do not. The manifest is parsed by one crate (`ferrion-manifest`) that the broker, `ferrion-run`, `linux-run` and the host validator share. Unknown formats, unknown keys, missing reasons, traversal ids (leading or trailing `.` or `-`, or `..`), reserved ids and over-bound inputs are refused with named error codes.
- Keys: `format`, `id`, `name`, `kind` (`native`, `ported`, `linux`, `windows`), `exec`, a `[capabilities.<name>]` table where each entry carries a `reason`, and optional `[review]` tables (`[[review.scene]]`, `[[review.exception]]`) that only the repository's review reads. Every list and string has a named upper bound in `recipes/core/ferrion-manifest/crate/src/lib.rs`, which is the single source of truth.
- A repository package's manifest is `manifest.toml` beside its `recipe.toml`, installed to `/usr/share/ferrion/apps/<package>.toml` on the system volume. An outside bundle carries `manifest.toml` at its root; the installer copies it to `/var/lib/ferrion/apps/<uid>/<id>/`, which only the broker writes. A repository id cannot be claimed by an outside bundle.
- In PFR0, `exec` was checked syntactically as a normalized absolute path; in PFR1, the cookbook validates `manifest.toml`, verifies `exec` exists inside the staged package (`missing-exec`), and installs it to `/usr/share/ferrion/apps/<package>.toml`.
- The terminal (`cosmic-term`) declares only what it needs to draw (`display`, `gpu`); treating the terminal as the owner's tool is a runtime rule, not a manifest grant.
- `ferrion-run` builds each manifest program's namespace, scope and environment from the intersection of its installed manifest (matched by canonical system `exec` path under `/usr` or `/bin`, refusing `duplicate-exec`) and `/var/lib/ferrion/grants/<uid>.toml` (missing uses class defaults; malformed fails closed with exit 125). Legacy profile format version 1 remains as a deprecated fallback for programs without a manifest for one release.
- Socket pass-through (`ferrion-scope` `Access::Socket`, bounded by `MAX_SOCKET_GRANTS = 4`): an exact-match socket grant opens the real UNIX stream socket with `O_RDWR | O_NOFOLLOW` and answers `OpenResult::OtherScheme`, avoiding `EXDEV` on `AF_UNIX` `connect` while folder grants stay fail-closed (`open_real` checks `fpath` and returns `ENOENT` on non-file descriptors). `ferrion-run --display` and the manifest `display` capability validate `$XDG_RUNTIME_DIR` (canonical, caller-owned, mode `0700`) and `$WAYLAND_DISPLAY` (`wayland-[0-9]{1,3}`, non-symlink `S_IFSOCK` owned by caller, distinct from `$FERRION_WM_SOCKET`), mount `<session>/run/wayland-0`, set `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY=wayland-0`, and strip `FERRION_WM_SOCKET`. Widening: with `--display` or the `display` capability a session reaches `ferrion-wm`'s main socket as a restricted client: windows, input to its own windows, paste with focus; no capture, input emulation, clipboard monitoring, layer surfaces or lock; it can see the person's idle state. DD3 replaces the socket with the broker's endpoint without changing the grant shape.
- Session-private `shm`: every `ferrion-run` session receives a private `shm` prefix handle registered as `shm` in its namespace (no manifest capability is required). A sandboxed app's `shm` objects live under its own prefix and it cannot reach any object outside it, another session's object with the same name, or the audio buffers. Widening: sessions can create shared-memory objects in their own prefix.

### Capabilities and grants

- The catalog of capabilities and their classes (`automatic`, `asked`, `settings-only`, `never`) lives in `CATALOG` in `recipes/core/ferrion-manifest/crate/src/catalog.rs`, which is its single source of truth; the host validator invokes the crate's binary directly rather than copying the table or parser.
- Undeclared means denied. The broker builds a program's namespace, file table and service grants from the intersection of its manifest and the owner's grant record, `/var/lib/ferrion/grants/<uid>.toml`, which only the broker writes and which stores only explicit answers.
- A denial looks like absence: a network that is down, an empty folder, no device.
- A capability that an update adds starts off.

### Outside software

- A package verified against the signed index ([ADR 0035](0035-signed-update-index.md)) is from the repository. Anything else is outside software.
- Outside code runs only through the broker. User-writable storage is not executable (enforced in `components/kernel` and `components/redoxfs` in PFR2); the broker maps outside code inside the program's namespace.
- Before an outside bundle's first start the broker shows a sheet, answered only by physical input, that names the source and hash and lists each declared capability with its reason. The answer is stored with the bundle's hash; new bytes from the same author key ask only about added capabilities; another key is another app.
- There is no notarization and no developer registry.

### Revocation

- Every grant is an object the broker can close or narrow. Revoking a grant acts on the running program; a grant a running process cannot lose ends the program.

### The repository's review

- A package in the index's `app` set with `kind = "native"` enters only with a passing review record: `components/ferrion-ui` at a supported version, and the headless design checks (tier 2, planned in PFR6). `ferrion-release` refuses to sign such a package without the record. Ported apps are exempt from tier 2 and follow tier 1 only.

## Consequences

- One format describes every program, so a new grant service adds a row to `CATALOG` in `ferrion-manifest`, not a new configuration file.
- The host validator is fail-closed over `config/x86_64/desktop.toml`: adding a new package without either a valid `manifest.toml` or a reasoned entry in `scripts/app-manifest-non-apps.toml` fails the check.
- No path starts outside code without the broker, which makes the broker and the kernel's exec right part of the trusted path.
- A program built before a capability existed cannot use it until it declares it and the owner grants it.
- Live revocation of the network and of folders waits for NW5 and Q5; until then revocation restarts the program.
- Adding a field to the manifest is a new `format` number.
- PFR0 changes no runtime behavior; every tier-1 protection remains planned until its slice passes on a built image, and protections are claimed per capability on a built image as the security roadmap's wording rules require.

## Where

- Landed in PFR0:
  - `recipes/core/ferrion-manifest/crate/`: manifest parser, named bounds (`src/lib.rs`), capability catalog (`src/catalog.rs`), named error codes (`src/error.rs`), parser (`src/parse.rs`) and host CLI (`src/bin/ferrion-manifest-check.rs`)
  - `scripts/app-manifest-check.py`, `scripts/app-manifest-non-apps.toml` and `scripts/app-manifest-fixtures/`: fail-closed host validator, explicit non-app list and refusal fixtures
  - `recipes/cosmic/cosmic-edit/manifest.toml`, `recipes/cosmic/cosmic-files/manifest.toml`, `recipes/cosmic/cosmic-term/manifest.toml`, `recipes/video/mpv/manifest.toml`: desktop image app manifests
- Landed in socket-grants and PFR1:
  - `recipes/core/ferrion-scope/crate/src/{scope.rs,proxy.rs}` and `sim/ferrion-scope/src/main.rs`: `Access::Socket` exact-path pass-through (`OpenResult::OtherScheme`), fail-closed `fpath` check in `open_real`, and simulator coverage
  - `recipes/core/ferrion-manifest/crate/src/grants.rs`: grant record parser and `effective_grant`, plus `duplicate-recipe` detection in `scripts/app-manifest-check.py`
  - `src/cook/cook_build.rs`: validates `manifest.toml`, verifies staged `exec` (`missing-exec`), and stages `/usr/share/ferrion/apps/<package>.toml`
  - `recipes/core/ferrion-run/crate/src/{lib.rs,manifest.rs,launch.rs,bin/ferrion-run.rs,bin/ferrion-run-probe.rs}` and `config/base.toml`: path-bound manifest lookup executing the resolved canonical `exec` path, persistent per-app `HOME` with component-by-component symlink/owner checks (`unsafe-app-home`), `XDG_*_DIR` and atomic `O_NOFOLLOW` `user-dirs.dirs` skipping symlinked or foreign granted folders, `display` mapped onto socket grants (`--display` for ad hoc), session-private `shm` prefix registered per session, `network-internet` gating `ip`/`icmp`/`tcp`/`udp` (`network-local` and `network-loopback-shared` fail closed until NW5), `audio-playback` gating `audio` (capture still requires `audiod`'s grant), `sudo` never passed, CLI widening flags (including `--pass-fd`) refused under a top-level manifest
  - `scripts/vm-test.py` and `scripts/display_shm.py`: `app-undeclared`, `app-declared`, `display-socket` and `display-shm` VM checks
- Planned for later slices: `components/kernel`, `components/redoxfs`, `components/base`, `components/relibc` (PFR2 to PFR4), the session broker (PFR3 and PFR4), `scripts/app-review.py` and `src/bin/ferrion_release.rs` against `components/ferrion-ui` (PFR6), and the `scripts/vm-test.py` checks `app-exec-path`, `app-first-start` and `app-revoke`.
