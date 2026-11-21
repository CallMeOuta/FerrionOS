# The TPM is driven by tpmd and reached only through attestd

Status: accepted

## Context

`recipes/core/tpmd` is FerrionOS's TPM 2.0 stack, the first step of item S20 in `plans/security-roadmap.md`. It drives a TPM on the Command Response Buffer (CRB) interface, which the ACPI TPM2 table names and QEMU's `tpm-crb` device implements, and it answers attestation requests. `config/tpm.toml` installs it and starts it at boot; `config/x86_64/desktop.toml` includes that file, and `scripts/vm-test.py tpm` checks it with a software TPM. What it does is described here; no remote verifier judges its statements yet, so a statement is checked on the machine that made it.

## Decision

### Three programs, one handle

- `tpmd` drives the TPM. It registers no scheme name: it makes one handle to itself, starts `attestd` with that handle, passes it the notification pipe and keeps neither, so `attestd` is the only process that can send the TPM a command. When `attestd` closes the handle, `tpmd` exits. Without a usable TPM, `tpmd` starts `attestd` with no handle, and its statements say `tpm: absent`.
- `attestd` serves the `attest` scheme to root alone, and `/etc/login_schemes.toml` gives `user` no `attest`. `/scheme/attest/pcrs` reads PCRs 7, 8 and 9 of the SHA-256 bank, one per line, for diagnosis on the machine. `/scheme/attest/statement` takes a nonce of 1 to 32 bytes and then returns a statement about it.
- `tpm-pcrs` and `attest-statement` are the clients. `attest-statement` seeks back to offset 0 after writing the nonce, because the statement starts there.
- The write end of init's `INIT_NOTIFY` pipe is left inheritable, so a daemon can hand it to a child it starts; `tpmd` hands it to `attestd`, which registers the scheme.

### What a statement is

A statement is a verdict: it says what the evidence shows and never carries the evidence (PCR values, the quote, keys or the event log). The quote behind it is signed by a key made in the TPM's null hierarchy, whose seed changes at every TPM reset, so the key is new on every boot and links nothing across boots. The key is a restricted ECDSA P-256 signing key with the attributes fixedTPM, fixedParent, sensitiveDataOrigin, userWithAuth, noDA, restricted and sign. `parse_quote` reads what a quote attests; it does not check the signature.

### Which PCRs mean what

PCR 7 holds the Secure Boot state and keys the firmware measured; every firmware with a TPM measures it. With measured boot, FerrionOS's bootloader adds its verification key to PCR 7, the boot environment and entry to PCR 8, and the manifest, kernel and initfs to PCR 9. So `attestd` decides whether the bootloader measured what it loaded from PCRs 8 and 9, not from PCR 7.

### Commands tpmd forwards

`ALLOWED_COMMANDS` lists the only commands `attestd` may send: random bytes, PCR reads, creating and flushing the null-hierarchy key, and quotes. Everything that changes the TPM for later boots or other users (PCR extends, NV storage, hierarchy control, clearing) is refused.

### The driver

- The TPM2 table's start method 7 is the CRB interface; the control area sits at offset `0x40` of the register page, and the locality registers exist when it does. Without them, `tpmd` maps the control area alone and addresses it as if it sat at its offset.
- Timeouts: 750 ms for locality and state changes (the specification's TIMEOUT_C is 200 ms; Linux allows more), 30 s for a command from `attestd` (key creation is the slowest), 5 s for each command of the boot-time probe, because init waits for `tpmd` while it probes and a hung TPM must not hold up the boot.
- `tpmd` polls the status register with `yield_now`, as base's `common::Timeout` does: after `setrens(0, 0)` it has no time scheme, and a sleep never returns.
- The probe sends `TPM2_Startup` first when the TPM answers `TPM_RC_INITIALIZE` (the firmware never started it), then asks for random bytes and passes them to randd before `tpmd` enters the null namespace; randd counts a hardware write as seeding only at 32 bytes.
- Responses are at most 4096 bytes, the largest the CRB interface carries.

### The wire format

`src/tpm2.rs` builds and parses the few commands by hand, with names and numbers from the TCG TPM 2.0 Library specification, Parts 2 and 3; every integer on the wire is big-endian. Authorization is one empty password (`TPM_RS_PW`, no nonce, no attributes). PCR selections are below 24 in the SHA-256 bank, and `parse_pcr_read` returns digests in the order asked. The module has no OS code, so its tests run on the build machine with `cargo test --lib`; everything else runs on FerrionOS only.

### The scheme crate's pin

`redox-scheme` is pinned to the commit base's `Cargo.lock` uses at the revision `recipes/core/base/recipe.toml` names, so the scheme protocol matches the kernel and init base is built for.

## Consequences

- Giving `tpmd` a scheme name, or `user` the `attest` scheme, widens who can reach the TPM: such a change is named in the report as AGENTS.md's Security rules ask.
- A command added to `ALLOWED_COMMANDS` that changes state for later boots breaks the per-boot design.
- Moving base's pin without moving `redox-scheme`'s can break the scheme protocol.

## Where

- `config/tpm.toml`
- `recipes/core/tpmd/recipe.toml`, `recipes/core/tpmd/crate/Cargo.toml`: the `redox-scheme` pin
- `recipes/core/tpmd/crate/src/bin/tpmd.rs`: `START_METHOD_CRB`, `CTRL_AREA`, `STATE_TIMEOUT`, `COMMAND_TIMEOUT`, `PROBE_TIMEOUT`, `ALLOWED_COMMANDS`, `tpm2_table`, `start_attestd`, `feed_randd`, `main`
- `recipes/core/tpmd/crate/src/bin/attestd.rs`: `RESPONSE_MAX`; `recipes/core/tpmd/crate/src/bin/attest-statement.rs`
- `recipes/core/tpmd/crate/src/lib.rs`: `BOOT_PCRS`; `recipes/core/tpmd/crate/src/service.rs`: `INIT_NOTIFY`
- `recipes/core/tpmd/crate/src/tpm2.rs`: `RC_INITIALIZE`, `create_attestation_key`, `pcr_read`, `parse_pcr_read`, `parse_quote`
