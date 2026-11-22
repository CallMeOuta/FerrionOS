#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<EOF
Makes a FerrionOS image a verified-boot image: signs its kernel and initfs and puts the key into
its UEFI bootloader.

Usage: $0 <redoxfs-binary> <key-dir> <harddrive.img>

  <redoxfs-binary>  the FUSE tool that mounts the image's filesystem (build/fstools/bin/redoxfs)
  <key-dir>         holds boot.key (Ed25519, PEM) and boot.pub (32 raw bytes); generated when
                    missing (make uses build/verified-boot)

Environment:
  REDOXFS_KEY_FILE      file containing the exact disk password bytes (no newline added).
  INSTALLER            installer executable; together with FILESYSTEM_CONFIG, exports the
                        merged config to read general.encrypt_disk when no key file is set.
  FILESYSTEM_CONFIG    filesystem config used to build the image (includes are resolved by
                        INSTALLER). The extracted password is kept only in a private temp file.
  VERIFIED_BOOT_SIGNER  a PKCS#11 URI of an Ed25519 key on a hardware token, such as
                        'pkcs11:token=YubiKey%20PIV%20%23<serial>;id=%02;type=private' for PIV
                        slot 9c of a YubiKey; the image is signed with it instead of <key-dir>,
                        which is then not read. Needs the OpenSSL pkcs11 provider (package
                        pkcs11-provider) and the token's PKCS#11 module in PKCS11_PROVIDER_MODULE
                        (libykcs11.so from yubico-piv-tool for a YubiKey); the token asks for its
                        PIN, once for the public key and once for the signature.

Needs openssl 3 or later, python3 with tomllib (3.11 or later) and FUSE.
EOF
    exit 2
}

[ $# -eq 3 ] || usage
REDOXFS_BIN="$1"
KEY_DIR="$2"
IMAGE="$3"
SIGNER="${VERIFIED_BOOT_SIGNER:-}"
BOOT_PROG=verified-boot-sign
. scripts/boot-signing-lib.sh
[ -x "$REDOXFS_BIN" ] || BOOT_EXIT=2 boot_die "$REDOXFS_BIN is not executable"
[ -f "$IMAGE" ] || BOOT_EXIT=2 boot_die "$IMAGE is not a file"
for tool in openssl python3; do
    command -v "$tool" >/dev/null || BOOT_EXIT=2 boot_die "$tool is missing; VERIFIED_BOOT=1 needs it on the host (or build with VERIFIED_BOOT=0, an image that verifies nothing)"
done
openssl version | grep -q '^OpenSSL [3-9]' || BOOT_EXIT=2 boot_die "$(openssl version) found; OpenSSL 3 or later is needed"
if [ -n "$SIGNER" ]; then
    boot_is_uri "$SIGNER" || BOOT_EXIT=2 boot_die "VERIFIED_BOOT_SIGNER must be a PKCS#11 URI (pkcs11:...), not $SIGNER"
    KEY="$SIGNER"
    KEY_NAME="${SIGNER%%\?*}"
else
    KEY="$KEY_DIR/boot.key"
    KEY_NAME="$KEY"
    mkdir -p "$KEY_DIR"
    if [ ! -f "$KEY" ]; then
        (umask 077 && openssl genpkey -algorithm ed25519 -out "$KEY")
        echo "verified-boot-sign: new development key in $KEY_DIR"
    fi
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ferrion-verified-boot.XXXXXX")"
BOOT_WORK="$WORK"
trap boot_cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ -z "${REDOXFS_KEY_FILE:-}" ] && \
    { [ -n "${INSTALLER:-}" ] || [ -n "${FILESYSTEM_CONFIG:-}" ]; }; then
    [ -n "${INSTALLER:-}" ] && [ -n "${FILESYSTEM_CONFIG:-}" ] ||
        BOOT_EXIT=2 boot_die "INSTALLER and FILESYSTEM_CONFIG must be set together"
    (umask 077 && "$INSTALLER" -c "$FILESYSTEM_CONFIG" --output-config="$WORK/config.toml")
    python3 -c '
import os
import sys
import tomllib

try:
    with open(sys.argv[1], "rb") as merged:
        config = tomllib.load(merged)
except (ValueError, UnicodeError):
    sys.exit("verified-boot-sign: cannot parse merged filesystem config")
password = config.get("general", {}).get("encrypt_disk")
if password is not None:
    if not isinstance(password, str):
        sys.exit("verified-boot-sign: general.encrypt_disk must be a string")
    fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as key:
        key.write(password.encode("utf-8"))
' "$WORK/config.toml" "$WORK/redoxfs.key"
    rm -f "$WORK/config.toml"
    if [ -f "$WORK/redoxfs.key" ]; then
        REDOXFS_KEY_FILE="$WORK/redoxfs.key"
    fi
fi

boot_public_pem "$KEY" "$WORK/boot.pub.pem"
openssl pkey -pubin -in "$WORK/boot.pub.pem" -outform DER | tail -c 32 > "$WORK/boot.pub"
case "$(openssl pkey -pubin -in "$WORK/boot.pub.pem" -noout -text)" in
    "ED25519 Public-Key"*) ;;
    *) boot_die "$KEY_NAME is not an Ed25519 key" ;;
esac
[ -n "$SIGNER" ] || cp "$WORK/boot.pub" "$KEY_DIR/boot.pub"
ESP_RANGE="$(boot_esp_range "$IMAGE")"
read -r ESP_OFFSET ESP_SIZE <<< "$ESP_RANGE"

boot_mount "$REDOXFS_BIN" "$IMAGE" "$WORK/mnt"
BOOT="$WORK/mnt/usr/lib/boot"
[ -f "$BOOT/bootloader.efi" ] || boot_die "$IMAGE has no UEFI bootloader (usr/lib/boot/bootloader.efi); verified boot requires a UEFI bootloader"

{
    echo "ferrion-boot-manifest 1"
    echo "kernel $(openssl dgst -sha512 -r "$BOOT/kernel" | cut -d' ' -f1)"
    echo "initfs $(openssl dgst -sha512 -r "$BOOT/initfs" | cut -d' ' -f1)"
} > "$WORK/manifest"
boot_sign_manifest "$KEY" "$WORK/manifest" "$WORK/manifest.sig"
openssl pkeyutl -verify -rawin -pubin -inkey "$WORK/boot.pub.pem" \
    -in "$WORK/manifest" -sigfile "$WORK/manifest.sig" >/dev/null
cp "$WORK/manifest" "$WORK/manifest.sig" "$BOOT/"
cp "$BOOT/bootloader.efi" "$WORK/bootloader.efi"

python3 - "$WORK/bootloader.efi" "$IMAGE" "$WORK/boot.pub" "$ESP_OFFSET" "$ESP_SIZE" <<'EOF'
import sys

MARKER = b"FerrionBootKey/1"
efi_path, image_path, pub_path, esp_offset, esp_size = sys.argv[1:]
esp_offset, esp_size = int(esp_offset), int(esp_size)
key = open(pub_path, "rb").read()
assert len(key) == 32, "boot.pub must hold 32 bytes"


def one_slot(data, where):
    at = data.find(MARKER)
    if at < 0 or data.find(MARKER, at + 1) >= 0:
        sys.exit(f"verified-boot-sign: {where} needs exactly one key slot; is the bootloader cooked with ferrion-verified-boot.patch?")
    return at + len(MARKER)


efi = bytearray(open(efi_path, "rb").read())
slot = one_slot(efi, "the bootloader")
efi[slot:slot + 32] = key
open(efi_path, "wb").write(efi)

with open(image_path, "r+b") as f:
    f.seek(esp_offset)
    esp = f.read(esp_size)
    esp_slot = one_slot(esp, "the EFI system partition")
    span = 4096
    lo, hi = max(0, slot - span), min(len(efi), slot + 32 + span)
    before = esp[esp_slot - (slot - lo):esp_slot]
    after = esp[esp_slot + 32:esp_slot + (hi - slot)]
    if before != bytes(efi[lo:slot]) or after != bytes(efi[slot + 32:hi]):
        sys.exit("verified-boot-sign: the bootloader in the EFI system partition differs from usr/lib/boot/bootloader.efi around its key slot (a fragmented copy, or a bootloader pushed without `make image`)")
    f.seek(esp_offset + esp_slot)
    f.write(key)
EOF
cp "$WORK/bootloader.efi" "$BOOT/bootloader.efi"
boot_unmount "$WORK/mnt"

echo "verified-boot-sign: $IMAGE signed for verified boot with $KEY_NAME"
