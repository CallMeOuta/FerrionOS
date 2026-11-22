# Signed update index, verify-before-parse client and release tool

Status: accepted

## Context

FerrionOS's inherited package tool fetches unsigned `repo.toml` indexes with no version number and accepts a server's public key from that server on first contact. Slice UP0 removed the default package remote so `ferrion update` exits 6 without contacting any server, leaving the signed update format and client to be built from scratch. Slice UP1 (security roadmap S15) settles the static repository layout, the `ferrion-update` client library, the `ferrion-release` host signing tool, the deterministic simulation under `sim/ferrion-update/`, and the `update-index` VM check, while keeping `ferrion update` at exit 6 until slice UP3 builds the store and generations.

## Decision

### Repository layout and file formats

A release repository is static files: a small signed root file that moves channel pointers, one immutable signed snapshot index per release and architecture, and content-addressed package objects. Every release number is a ten-digit integer `YYYYMMDDNN` (`ReleaseNumber`) compared strictly as an integer and never by the client's clock; a package revision `rev` is the release number in which its bytes last changed.

- `root.toml` and its raw 64-byte detached Ed25519 signature `root.toml.sig` (at most `MAX_ROOT_BYTES` = 64 KiB): `format = 1`, strictly increasing `serial >= 1`, a pure TOML calendar date `signed = YYYY-MM-DD`, `fresh_days` (1 to `MAX_FRESH_DAYS` = 3650), `[channels]` (`stable`, `beta`, `dev` — no other channel names), `[releases.<YYYYMMDDNN>]` (`index` mapping FerrionOS architectures `x86_64`, `aarch64` or `riscv64gc` to `"blake3:<64 hex>"`, `floor <= release`, `security`, optional waypoint `via < release`, optional `withdrawn`, optional monotonic `rollout` steps), optional `[keys] next = "ed25519:<64 hex>"`, and optional `[[mirrors]] url` (`http://` or `https://`).
- `snap/<release>/<arch>/index.toml` and `snap/<release>/<arch>/index.toml.sig` (at most `MAX_INDEX_BYTES` = 8 MiB): `format = 1`, `release`, `system <= release`, `fixes` (advisory identifiers), and `[[package]]` entries with unique valid package names, `set` (`"system"` or `"app"`), `rev <= release`, `head = "blake3:..."` (the pkgar entry-table hash), `object = "blake3:..."` (the BLAKE3 hash of the served `.pkgar` bytes), `size` (1 to `MAX_OBJECT_BYTES` = 2 GiB), `needs`, and optional `patch` entries (`from`, `object`, `size`, parsed and validated now; delta application is planned for UP11).
- `objects/<aa>/<64-hex>`: raw `.pkgar` archive bytes under the first two hex digits and full 64-hex BLAKE3 hash of the served file.
- `rotation.toml` and `rotation.toml.sig` (at most `MAX_ROTATION_BYTES` = 4 KiB): `format = 1`, `serial`, and `new_key = "ed25519:..."`, signed by an offline `emergency_key` and accepted by `rotate_release_key` only together with a successor `root.toml` signed by `new_key` at a serial at or above the rotation's.

### Verify before parse and the seven client steps

`ferrion-update` uses `dryoc`'s detached Ed25519 verification (`crypto_sign_verify_detached`) and `blake3` so the update client adds no second signature implementation beyond what `pkgar-core` already links. No TOML parser or archive reader runs on unauthenticated bytes, and `ClientState` is never mutated until every step succeeds (`check_and_verify` returns a new `next_state`; `apply_update` swaps it only on `Ok`).

1. **Bounded, unidentifying fetches.** Every `FetchRequest` asserts a leading `/`, no `?` query string, no `..` traversal, no ASCII control bytes or spaces, a positive `max_bytes`, and the fixed `User-Agent: ferrion-update/1` (`USER_AGENT`). No machine identifier, installed version or cookie is sent. Root fetches cap at 64 KiB and signatures at 64 bytes.
2. **Verify root signature before parsing.** `verify_root_bytes` checks the size cap and verifies the 64-byte Ed25519 signature over the exact bytes of `root.toml` against `state.release_key` before touching `toml`. Only after verification does `parse_root_toml` inspect `format`: an unknown format returns `UnsupportedFormat` naming `state.last_waypoint` (or the release medium when none is known).
3. **Monotonicity and waypoints.** A `serial` below `state.trusted_serial` is refused (`rollback-serial`). A channel pointer below `state.installed_release` is refused (`rollback-release`), even under a higher `serial`. A missing or `withdrawn` target release is refused. If the target names `via` above `state.installed_release`, the client targets the waypoint release first.
4. **Staleness notice without refusal.** When `today.days_since(root.signed) > root.fresh_days`, the outcome carries `StaleNotice` (`signed`, `today`, `age_days`, `fresh_days`) and is still accepted so a newer signed release is never blocked by a stale or host-skewed clock.
5. **Snapshot index verified by hash and signature before parsing.** `verify_index_bytes` checks `MAX_INDEX_BYTES`, compares `blake3(index_bytes)` against the hash `root.toml` names for `(target_release, state.arch)` first, then verifies the Ed25519 signature over `index_bytes` with `state.release_key`, and only then parses `index.toml` and verifies `index.release == target_release`.
6. **Package objects verified before pkgar parsing.** Each changed package (or all packages when requested) is fetched with `max_bytes = pkg.size`. `verify_package_object` refuses oversized or short payloads and checks `blake3(bytes) == pkg.object` before `verify_pkgar` inspects a byte. `verify_pkgar` then verifies the 136-byte pkgar header signature, matches `header.blake3 == pkg.head`, checks `blake3(entries_table) == header.blake3`, rejects unsafe entry paths, and checks every entry's file bytes against its BLAKE3 hash (currently uncompressed `packaging == 0`, matching every package `repo/` builds; LZMA2 is rejected with `pkgar-invalid` until compressed distribution is needed).
7. **Strict schema and named refusal reasons.** Every TOML type uses `#[serde(deny_unknown_fields)]` and TigerStyle upper bounds (`MAX_RELEASES_IN_ROOT`, `MAX_PACKAGES_IN_INDEX`, `MAX_FIXES_IN_INDEX`, `MAX_NEEDS_PER_PACKAGE`, `MAX_PATCHES_PER_PACKAGE`, `MAX_MIRRORS`, `MAX_ROLLOUT_STEPS`, `MAX_PKGAR_ENTRIES`, `MAX_STRING_LEN`, `MAX_URL_LEN`). Every refusal maps to a stable `UpdateError::reason_code()` string: `invalid-signature`, `invalid-index-signature`, `rollback-serial`, `rollback-release`, `index-hash-mismatch`, `index-release-mismatch`, `object-hash-mismatch`, `object-too-large`, `object-size-mismatch`, `pkgar-invalid`, `unknown-key-or-syntax`, `unsupported-format`, `root-too-large`, `index-too-large`, `release-withdrawn`, `missing-channel`, `missing-release`, `missing-arch`, `limit-exceeded`, `rotation-refused`, `size-limit-exceeded`, `not-found`, and `transport-error`.

### Signing keys and `ferrion-release`

`ferrion-release` (`src/bin/ferrion_release.rs`) builds and signs `index.toml` and `root.toml` from a cooked `repo/<target>/` directory via `openssl pkeyutl -sign -rawin` and extracts the 32-byte raw public key from the 44-byte RFC 8410 Ed25519 SPKI DER output, then self-verifies every signature with `ferrion-update` before returning.

- Key resolution follows `--key <PEM | pkcs11:...>`, then `FERRION_RELEASE_SIGNER`, then `VERIFIED_BOOT_SIGNER`, then `--dev-key-dir <DIR>` (creating a mode `0600` development `release.key` when missing).
- When the signer begins with `pkcs11:`, `ferrion-release` generates a temporary OpenSSL configuration activating `pkcs11.so` with `pkcs11-module-block-operations = digest` and `pkcs11-module-quirks = no-deinit`, matching `scripts/boot-signing-lib.sh` (ADR 0010).
- Today only development Ed25519 keys are exercised end to end; hardware token signing awaits the maintainer's choice of token path, and PKCS#11 URI dispatch is wired and checked for OpenSSL provider activation without an attached token.
- `build` verifies every `.pkgar` archive before publishing it to `objects/`, preserves `rev` for unchanged package heads when `--previous-index` is given, refuses to overwrite an existing `OUT/root.toml` unless `--previous-root` or `--fresh` is given, refuses non-increasing `--serial` or `--release` when `--previous-root` is given, and preserves earlier releases, architectures and per-release metadata (`floor`, `security`, `via`) when adding another architecture to a release. `sign-root` promotes channels or marks a release withdrawn by incrementing `serial` without rebuilding packages or indexes.

### Scope boundary until UP3

`ferrion update` stays at exit code 6 with `data.package_source = "none"` and is not wired to `ferrion-update` in UP1. Slice UP3 will add the content-addressed `/system/store`, `/system/gen/<n>` generation trees, the update service that composes a new generation from `ferrion-update`'s verified packages, and the CLI wiring. Until then, `update-index-probe` (`recipes/tests/update-index-probe/`) exercises `ferrion-update` inside the guest over a bounded HTTP/1.0 client for the `update-index` VM check, and `sim/ferrion-update/` exercises it in deterministic host simulation.

## Consequences

- Corrupted or attacker-forged bytes never reach the TOML or pkgar parsers, and any refusal leaves the client's installed release, trusted serial, release key and package set unchanged.
- Adding a new field to `root.toml` or `index.toml` is a breaking change for older clients unless gated by a new `format` number and a waypoint release (`via`).
- `ferrion-update` has no OS-specific code or git dependencies, so the host release tool, the deterministic simulator and the FerrionOS guest binary share the exact same verification logic.
- Uncompressed pkgar verification avoids C decompression code in the verification path; compressed archives or delta patches remain explicit future slices (UP11).

## Where

- `recipes/core/ferrion-update/crate/src/lib.rs`: format constants and TigerStyle bounds
- `recipes/core/ferrion-update/crate/src/crypto.rs`: `Blake3Hash`, `PublicKey`, `Signature`, `Keypair`, `verify_detached`
- `recipes/core/ferrion-update/crate/src/format.rs`: `CalendarDate`, `ReleaseNumber`, `RootFile`, `IndexFile`, `KeyRotation`, `parse_root_toml`, `parse_index_toml`, `render_root_toml`, `render_index_toml`
- `recipes/core/ferrion-update/crate/src/pkgar.rs`: `inspect_pkgar_head`, `verify_pkgar`, `build_uncompressed_pkgar`
- `recipes/core/ferrion-update/crate/src/client.rs`: `FetchRequest`, `Fetcher`, `ClientState`, `verify_root_bytes`, `verify_index_bytes`, `verify_package_object`, `check_and_verify`, `apply_update`, `rotate_release_key`
- `recipes/core/ferrion-update/crate/src/error.rs`: `FetchError`, `UpdateError`, `UpdateError::reason_code`
- `src/bin/ferrion_release.rs` and `Cargo.toml`: `ferrion-release` host tool (`keygen`, `pubkey`, `build`, `sign-root`, `verify`)
- `sim/ferrion-update/`: deterministic seeded simulation (`run.sh`, `regressions.txt`, `src/main.rs`)
- `recipes/tests/update-index-probe/`, `config/x86_64/desktop.toml`, `scripts/vm-test.py` (`test_update_index`), and `docs/testing.md`
