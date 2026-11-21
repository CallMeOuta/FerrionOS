# How images are signed for verified boot and Secure Boot, and how that is tested

Status: accepted

## Context

FerrionOS signs its boot files in two layers. Verified boot: the UEFI bootloader carries a public key and starts only the kernel and initfs a signed manifest names. UEFI Secure Boot: the firmware starts only a bootloader signed with a key in its `db`. Development keys live on the build machine; release keys live on a hardware token reached through PKCS#11. The checks run in QEMU with OVMF and a software TPM. FerrionOS has no TPM driver, so measured boot is read from the host. The roadmap items are S16's slices in `plans/security-roadmap.md`; what is proven on a built image is only what these tests show.

## Decision

### Signing an image (verified boot)

- `scripts/verified-boot-sign.sh` turns an image into a verified-boot image; `make image` runs it on `harddrive.img` when `VERIFIED_BOOT=1`, the default (`mk/config.mk`), through `VERIFIED_BOOT_SIGN` in `mk/disk.mk`, which appends the image's path. It writes `usr/lib/boot/manifest` (the SHA-512 of `usr/lib/boot/kernel` and `usr/lib/boot/initfs`) and `usr/lib/boot/manifest.sig` (its Ed25519 signature), and puts the public key into the bootloader's key slot. The cooked bootloader has an empty slot and verifies nothing.
- The key is the development pair `boot.key` and `boot.pub` in the key directory, made on first use, unless `VERIFIED_BOOT_SIGNER` names a key on a hardware token. A development key never signs an image that leaves the build machine.
- The key goes in twice: into `usr/lib/boot/bootloader.efi` and into the copy in the EFI system partition. In the partition, the bytes around the slot must match the file's, so a fragmented FAT copy or a different bootloader is refused rather than patched blindly. A slot that already holds a key is rewritten, which is how an image is signed again after a push changed its kernel or initfs.
- The public key is the last 32 bytes of its DER form. Ed25519 through `openssl pkeyutl -rawin` is tested with OpenSSL 3; LibreSSL (macOS's `openssl`) is not supported.
- Messages leave out a PKCS#11 URI's query, which can hold the PIN. The merged configuration and the disk password never go into shell variables, logs or arguments: they are written to files in the script's private `mktemp -d` directory, and the configuration file is removed right after reading. The script never deletes a directory recursively while a filesystem might still be mounted in it.

### The shared helpers

`scripts/boot-signing-lib.sh` serves `secure-boot-keys.sh`, `secure-boot-test.sh` and `verified-boot-sign.sh`. A caller sets `BOOT_PROG` (the prefix of every message) before sourcing it, sets `BOOT_WORK` to its scratch directory before signing with a token key, and runs with `set -euo pipefail`; the library changes no shell options.

- `boot_need` stops with exit code 2 when a command is missing.
- `boot_mount` mounts an image's RedoxFS through FUSE in the background and waits for `usr/lib/boot/kernel`; the caller unmounts with `$BOOT_FUMOUNT`, also on failure. `REDOXFS_KEY_FILE` supplies the password bytes without a prompt.
- `boot_esp_range` prints the EFI system partition's byte offset and size from the GPT; `boot_esp_loader` prints the mtools path of the bootloader the firmware starts, such as `::/EFI/BOOT/BOOTX64.EFI`.
- A key is a file or a PKCS#11 URI. `boot_pkcs11_conf` writes an OpenSSL configuration that loads the pkcs11 provider beside the default one; the token's module comes from `PKCS11_PROVIDER_MODULE` (for a YubiKey, `libykcs11.so`) or from p11-kit's proxy. The provider is configured to offer no digests, so a module that hashes with OpenSSL itself (SoftHSM, YKCS11) is not called back into, and to skip deinitialization, which crashes SoftHSM 2.7 at exit. `boot_with_key` loads it for a URI key; the token asks for its PIN on the terminal unless the URI carries `pin-source`.
- `boot_sign_manifest` writes the 64 raw bytes of an Ed25519 signature.
- `sb_make_cert` makes a self-signed RSA certificate with SHA-256 and, without a token key, an unencrypted RSA-2048 key, the size every UEFI implementation must accept; an existing certificate is kept.
- `sb_sign_efi` signs with `$SB_SIGNER`; a token key needs an `sbsign` with OpenSSL provider support. `sb_is_signed` only checks that the PE certificate table holds a signature.

### Secure Boot keys

`scripts/secure-boot-keys.sh` manages FerrionOS's own PK, KEK and db the way sbctl manages a machine owner's: `create` makes the certificates (and development keys), `sign` signs an image's bootloader with the db key in place, and `export` writes what a user enrolls: certificates, EFI signature lists, authenticated variable updates and a README with the firmware steps. Development keys are unencrypted and sign only what stays on the build machine; release keys are named by `SECURE_BOOT_PK`, `SECURE_BOOT_KEK` and `SECURE_BOOT_DB` as PKCS#11 URIs, and the key directory then holds only certificates. The PK update replaces; the KEK and db updates append to what the firmware holds, Microsoft's keys included.

### The tests

- `scripts/secure-boot-test.sh` makes test keys (PK, KEK, db and one never enrolled), enrolls PK, KEK and db into a copy of OVMF's variable store so the firmware leaves setup mode, and boots copies of the image: a signed bootloader must reach the graphical login, an unsigned one and one signed with the unenrolled key must be refused. On a verified-boot image (its bootloader's key slot, after the marker `FerrionBootKey/1`, holds a key) three more copies change a byte of the kernel, a byte of the initfs, or sign the manifest with another key, and the bootloader must refuse each.
- `scripts/measured-boot-test.sh` boots copies with OVMF and swtpm on a CRB interface: `tpm` (as built, PCRs 7, 8 and 9 measured), `tpm-initfs` (initfs grown by 4 KiB and the manifest signed again: PCR 9 changes, PCR 8 does not), `tpm-menu` (the bootloader's mode menu opened: the environment lacks `QUIET=1`, so PCR 8 changes and PCR 9 does not) and `no-tpm`. At the login the guest's memory is saved and searched for `TPM_EVENT_LOG_ADDR` and `TPM_EVENT_LOG_SIZE`, which locate the bootloader's copy of the event log; QEMU is then killed and a second swtpm resumes the saved state so `tpm2_pcrread` can read the PCRs. A case passes when the log replays to PCRs 7, 8 and 9, PCR 9 equals the value computed from the manifest, kernel and initfs, PCR 8 holds the entry name and the environment, and PCR 7 holds the key slot.
- With swtpm, OVMF often loses its TPM when the QEMU monitor stays idle through the firmware phase, so every wait keeps talking to the monitor from power-on. A boot whose firmware never started the bootloader, or whose PCR 4 is empty, is a stall: the case runs again, and two stalls are `BLOCKED`, not `FAIL`.
- Both tests leave the image unchanged, write each case's serial log and screenshot under the output directory beside `summary.txt`, judge the screen with the classifier of `scripts/vm-test.py`, and load that module with `python3 -B` so no `__pycache__` is left in the tree. The test keys are unencrypted and never sign anything that leaves the build machine.

## Consequences

- Signing only one bootloader copy, or patching the partition without comparing it to the file, yields an image whose two bootloaders disagree.
- A key put in a variable or an argument can reach a log or `ps`.
- A test that counted a stall as a failure would fail on host timing, not on FerrionOS.

## Where

- `mk/disk.mk`: `VERIFIED_BOOT_SIGN`, `$(BUILD)/harddrive.img`; `mk/config.mk`: `VERIFIED_BOOT`, `VERIFIED_BOOT_KEYS`, `VERIFIED_BOOT_SIGNER`
- `scripts/verified-boot-sign.sh`
- `scripts/boot-signing-lib.sh`: `boot_need`, `boot_mount`, `boot_esp_range`, `boot_esp_loader`, `boot_pkcs11_conf`, `boot_with_key`, `boot_sign_manifest`, `sb_make_cert`, `sb_sign_efi`, `sb_is_signed`
- `scripts/secure-boot-keys.sh`: `create`, `sign`, `export`
- `scripts/secure-boot-test.sh`, `scripts/measured-boot-test.sh`: `boot_case`
