#!/usr/bin/env bash
# Makes a FerrionOS image a verified-boot image (security roadmap S16, slice 2). `make image` runs it
# on harddrive.img when VERIFIED_BOOT=1 (mk/config.mk). It writes into the image:
#   usr/lib/boot/manifest       SHA-512 of usr/lib/boot/kernel and usr/lib/boot/initfs
#   usr/lib/boot/manifest.sig   Ed25519 signature of the manifest
# and puts the public key into the key slot of the UEFI bootloader, both the copy in the EFI system
# partition and usr/lib/boot/bootloader.efi. A bootloader with a key starts only the kernel and
# initfs the manifest names; the cooked bootloader has an empty slot and verifies nothing.
# The key pair lives in <key-dir> (boot.key, boot.pub), generated on first use. It is a
# development key: never sign an image that leaves the build machine with it.
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

Needs openssl 3 or later, python3 and FUSE.
EOF
    exit 2
}

[ $# -eq 3 ] || usage
REDOXFS_BIN="$1"
KEY_DIR="$2"
IMAGE="$3"
[ -x "$REDOXFS_BIN" ] || { echo "verified-boot-sign: $REDOXFS_BIN is not executable" >&2; exit 2; }
[ -f "$IMAGE" ] || { echo "verified-boot-sign: $IMAGE is not a file" >&2; exit 2; }
for tool in openssl python3; do
    command -v "$tool" >/dev/null || { echo "verified-boot-sign: $tool is missing; VERIFIED_BOOT=1 needs it on the host" >&2; exit 2; }
done
# Ed25519 through pkeyutl -rawin is tested with OpenSSL 3.5 and 4.1; LibreSSL (macOS's openssl) is not.
openssl version | grep -q '^OpenSSL [3-9]' || { echo "verified-boot-sign: $(openssl version) found; OpenSSL 3 or later is needed" >&2; exit 2; }
if command -v fusermount3 >/dev/null; then
    FUMOUNT="fusermount3 -u"
else
    FUMOUNT="fusermount -u"
fi

mkdir -p "$KEY_DIR"
if [ ! -f "$KEY_DIR/boot.key" ]; then
    (umask 077 && openssl genpkey -algorithm ed25519 -out "$KEY_DIR/boot.key")
    echo "verified-boot-sign: new development key in $KEY_DIR"
fi
# The public key is the last 32 bytes of its DER form.
openssl pkey -in "$KEY_DIR/boot.key" -pubout -outform DER | tail -c 32 > "$KEY_DIR/boot.pub.partial"
mv "$KEY_DIR/boot.pub.partial" "$KEY_DIR/boot.pub"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ferrion-verified-boot.XXXXXX")"
MOUNTED=0
cleanup() {
    if [ "$MOUNTED" = 1 ]; then
        $FUMOUNT "$WORK/mnt" || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

mkdir "$WORK/mnt"
"$REDOXFS_BIN" "$IMAGE" "$WORK/mnt" >"$WORK/redoxfs.log" 2>&1 &
MOUNTED=1
BOOT="$WORK/mnt/usr/lib/boot"
for _ in $(seq 100); do
    [ -f "$BOOT/kernel" ] && break
    sleep 0.1
done
[ -f "$BOOT/kernel" ] || { cat "$WORK/redoxfs.log" >&2; echo "verified-boot-sign: could not mount $IMAGE" >&2; exit 1; }

{
    echo "ferrion-boot-manifest 1"
    echo "kernel $(openssl dgst -sha512 -r "$BOOT/kernel" | cut -d' ' -f1)"
    echo "initfs $(openssl dgst -sha512 -r "$BOOT/initfs" | cut -d' ' -f1)"
} > "$WORK/manifest"
openssl pkeyutl -sign -rawin -inkey "$KEY_DIR/boot.key" -in "$WORK/manifest" -out "$WORK/manifest.sig"
cp "$WORK/manifest" "$WORK/manifest.sig" "$BOOT/"
cp "$BOOT/bootloader.efi" "$WORK/bootloader.efi"

# Writes the key after the slot's marker: once in the bootloader file, once in the EFI system
# partition, where the bootloader's bytes around the slot must match the file's, so that a
# fragmented FAT copy is caught instead of patched blindly.
python3 - "$WORK/bootloader.efi" "$IMAGE" "$KEY_DIR/boot.pub" <<'EOF'
import struct, sys, uuid

MARKER = b"FerrionBootKey/1"
ESP = uuid.UUID("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
efi_path, image_path, pub_path = sys.argv[1:]
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
    header = f.read(1024)[512:]
    if header[:8] != b"EFI PART":
        sys.exit("verified-boot-sign: no GPT in the image")
    lba, count, size = struct.unpack_from("<QII", header, 72)
    f.seek(lba * 512)
    table = f.read(count * size)
    for i in range(count):
        entry = table[i * size:(i + 1) * size]
        if uuid.UUID(bytes_le=entry[:16]) == ESP:
            first, last = struct.unpack_from("<QQ", entry, 32)
            break
    else:
        sys.exit("verified-boot-sign: no EFI system partition in the image")
    f.seek(first * 512)
    esp = f.read((last - first + 1) * 512)
    esp_slot = one_slot(esp, "the EFI system partition")
    span = 4096
    lo, hi = max(0, slot - span), min(len(efi), slot + 32 + span)
    window = esp[esp_slot - (slot - lo):esp_slot + (hi - slot)]
    expected = bytes(efi[lo:slot]) + bytes(32) + bytes(efi[slot + 32:hi])
    if window != expected:
        sys.exit("verified-boot-sign: the bootloader in the EFI system partition is not stored contiguously around its key slot")
    f.seek(first * 512 + esp_slot)
    f.write(key)
EOF
cp "$WORK/bootloader.efi" "$BOOT/bootloader.efi"
sync
$FUMOUNT "$WORK/mnt"
MOUNTED=0

openssl pkeyutl -verify -rawin -pubin -inkey <(openssl pkey -in "$KEY_DIR/boot.key" -pubout) \
    -in "$WORK/manifest" -sigfile "$WORK/manifest.sig" >/dev/null
echo "verified-boot-sign: $IMAGE signed for verified boot with $KEY_DIR/boot.pub"
