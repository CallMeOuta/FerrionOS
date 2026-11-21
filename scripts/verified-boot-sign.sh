#!/usr/bin/env bash
# Makes a FerrionOS image a verified-boot image (security roadmap S16, slice 2). `make image` runs it
# on harddrive.img when VERIFIED_BOOT=1, the default (mk/config.mk). It writes into the image:
#   usr/lib/boot/manifest       SHA-512 of usr/lib/boot/kernel and usr/lib/boot/initfs
#   usr/lib/boot/manifest.sig   Ed25519 signature of the manifest
# and puts the public key into the key slot of the UEFI bootloader, both the copy in the EFI system
# partition and usr/lib/boot/bootloader.efi. A bootloader with a key starts only the kernel and
# initfs the manifest names; the cooked bootloader has an empty slot and verifies nothing.
# The key is the development key pair in <key-dir> (boot.key, boot.pub), generated on first use,
# unless VERIFIED_BOOT_SIGNER names a key on a hardware token. A development key never signs an
# image that leaves the build machine.
# Run from the repository root.

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
  VERIFIED_BOOT_SIGNER  a PKCS#11 URI of an Ed25519 key on a hardware token, such as
                        'pkcs11:token=YubiKey%20PIV%20%23<serial>;id=%02;type=private' for PIV
                        slot 9c of a YubiKey; the image is signed with it instead of <key-dir>,
                        which is then not read. Needs the OpenSSL pkcs11 provider (package
                        pkcs11-provider) and the token's PKCS#11 module in PKCS11_PROVIDER_MODULE
                        (libykcs11.so from yubico-piv-tool for a YubiKey); the token asks for its
                        PIN, once for the public key and once for the signature.

Needs openssl 3 or later, python3 and FUSE.
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
# Ed25519 through pkeyutl -rawin is tested with OpenSSL 3.5 and 4.1; LibreSSL (macOS's openssl) is not.
openssl version | grep -q '^OpenSSL [3-9]' || BOOT_EXIT=2 boot_die "$(openssl version) found; OpenSSL 3 or later is needed"
if [ -n "$SIGNER" ]; then
    boot_is_uri "$SIGNER" || BOOT_EXIT=2 boot_die "VERIFIED_BOOT_SIGNER must be a PKCS#11 URI (pkcs11:...), not $SIGNER"
    KEY="$SIGNER"
    # Messages leave out the URI's query, which can hold the PIN.
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
MOUNTED=0
cleanup() {
    if [ "$MOUNTED" = 1 ]; then
        $BOOT_FUMOUNT "$WORK/mnt" || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

# The public key is the last 32 bytes of its DER form.
boot_public_pem "$KEY" "$WORK/boot.pub.pem"
openssl pkey -pubin -in "$WORK/boot.pub.pem" -outform DER | tail -c 32 > "$WORK/boot.pub"
case "$(openssl pkey -pubin -in "$WORK/boot.pub.pem" -noout -text)" in
    "ED25519 Public-Key"*) ;;
    *) boot_die "$KEY_NAME is not an Ed25519 key" ;;
esac
[ -n "$SIGNER" ] || cp "$WORK/boot.pub" "$KEY_DIR/boot.pub"
ESP_RANGE="$(boot_esp_range "$IMAGE")"
read -r ESP_OFFSET ESP_SIZE <<< "$ESP_RANGE"

MOUNTED=1
boot_mount "$REDOXFS_BIN" "$IMAGE" "$WORK/mnt"
BOOT="$WORK/mnt/usr/lib/boot"
[ -f "$BOOT/bootloader.efi" ] || boot_die "$IMAGE has no UEFI bootloader (usr/lib/boot/bootloader.efi); i586 boots through BIOS only and cannot verify, build it with VERIFIED_BOOT=0"

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

# Writes the key after the slot's marker: once in the bootloader file, once in the EFI system
# partition, where the bootloader's bytes around the slot must match the file's, so that a
# fragmented FAT copy, or a bootloader other than the file's, is caught instead of patched
# blindly. The slot itself may already hold a key: signing an image again (after a push
# changed its kernel or initfs) rewrites it.
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
sync
$BOOT_FUMOUNT "$WORK/mnt"
MOUNTED=0

echo "verified-boot-sign: $IMAGE signed for verified boot with $KEY_NAME"
