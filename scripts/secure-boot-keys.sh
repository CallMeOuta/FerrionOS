#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<EOF
FerrionOS's own UEFI Secure Boot keys: create them, sign an image's bootloader with db, and export
what a user enrolls in firmware.

Usage: $0 [-k <key-dir>] create
       $0 [-k <key-dir>] [-r <redoxfs>] sign <harddrive.img>
       $0 [-k <key-dir>] export <out-dir>

  -k <key-dir>  PK, KEK and db as <name>.crt and, for development keys, <name>.key, and
                owner.guid (default: build/secure-boot-keys)
  -r <redoxfs>  the FUSE tool that mounts the image's filesystem (default:
                build/fstools/bin/redoxfs)

create  makes what is missing: RSA-2048 keys with self-signed certificates, or, for a key named
        by URI below, a certificate for that token key.
sign    signs EFI/BOOT/BOOT<arch>.EFI in the image's EFI system partition and
        usr/lib/boot/bootloader.efi in its filesystem with db, and checks the result. Run it
        after the image is built, verified-boot signing included: any later change to the
        bootloader voids the signature.
export  writes PK, KEK and db as .cer (DER), .esl and .auth into <out-dir>, with README.txt
        telling the user how to enroll them.

Environment:
  SECURE_BOOT_PK, SECURE_BOOT_KEK, SECURE_BOOT_DB
                PKCS#11 URIs of keys on a hardware token, used instead of <key-dir>/<name>.key,
                such as 'pkcs11:token=YubiKey%20PIV%20%23<serial>;id=%05;type=private' for PIV
                slot 82 of a YubiKey. Needs the OpenSSL pkcs11 provider (package
                pkcs11-provider) and the token's PKCS#11 module in PKCS11_PROVIDER_MODULE.

Needs openssl, python3, mtools and sbsigntools (sbsign, sbverify, sbsiglist, sbvarsign); sign
works with osslsigncode too, for key files.

Example: $0 create && $0 sign build/x86_64/desktop/harddrive.img && $0 export build/secure-boot-enroll
EOF
    exit 2
}

KEYS=build/secure-boot-keys
REDOXFS_BIN=build/fstools/bin/redoxfs
while getopts "k:r:h" opt; do
    case "$opt" in
        k) KEYS="$OPTARG" ;;
        r) REDOXFS_BIN="$OPTARG" ;;
        *) usage ;;
    esac
done
shift $((OPTIND - 1))
[ $# -ge 1 ] || usage
COMMAND="$1"
shift

BOOT_PROG=secure-boot-keys
. scripts/boot-signing-lib.sh
boot_need openssl "OpenSSL"
boot_need python3 "Python 3"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/ferrion-secure-boot-keys.XXXXXX")"
BOOT_WORK="$WORK"
trap boot_cleanup EXIT

key() {
    local uri
    case "$1" in
        PK) uri="${SECURE_BOOT_PK:-}" ;;
        KEK) uri="${SECURE_BOOT_KEK:-}" ;;
        db) uri="${SECURE_BOOT_DB:-}" ;;
    esac
    if [ -n "$uri" ]; then
        boot_is_uri "$uri" || BOOT_EXIT=2 boot_die "the key for $1 must be a PKCS#11 URI (pkcs11:...)"
        echo "$uri"
    else
        echo "$KEYS/$1.key"
    fi
}

have() {
    [ -f "$KEYS/$1.crt" ] || boot_die "$KEYS/$1.crt is missing; run $0 -k $KEYS create first"
    local k
    k="$(key "$1")"
    boot_is_uri "$k" || [ -f "$k" ] || boot_die "$k is missing and SECURE_BOOT_$(echo "$1" | tr a-z A-Z) is not set"
}

create() {
    [ $# -eq 0 ] || usage
    mkdir -p "$KEYS"
    local name k
    for name in PK KEK db; do
        k="$(key "$name")"
        if boot_is_uri "$k"; then
            sb_make_cert "$KEYS" "$name" "/CN=FerrionOS Secure Boot $name/" "$k"
        else
            sb_make_cert "$KEYS" "$name" "/CN=FerrionOS Secure Boot $name (development)/"
        fi
    done
    [ -f "$KEYS/owner.guid" ] || python3 -c 'import uuid; print(uuid.uuid4())' > "$KEYS/owner.guid"
    echo "secure-boot-keys: PK, KEK and db in $KEYS"
}

sign() {
    [ $# -eq 1 ] || usage
    local image="$1" range offset loader
    [ -f "$image" ] || BOOT_EXIT=2 boot_die "$image is not a file"
    boot_need mcopy "mtools"
    sb_find_signer
    have db
    range="$(boot_esp_range "$image")"
    read -r offset _ <<< "$range"
    loader="$(boot_esp_loader "$image" "$offset")"
    mcopy -n -i "$image@@$offset" "$loader" "$WORK/unsigned.efi"
    ! sb_is_signed "$WORK/unsigned.efi" || boot_die "the bootloader of $image is already signed; sign an image as built"
    boot_mount "$REDOXFS_BIN" "$image" "$WORK/mnt"
    cmp -s "$WORK/unsigned.efi" "$WORK/mnt/usr/lib/boot/bootloader.efi" \
        || boot_die "the bootloader in the EFI system partition differs from usr/lib/boot/bootloader.efi"
    sb_sign_efi "$(key db)" "$KEYS/db.crt" "$WORK/unsigned.efi" "$WORK/signed.efi"
    sb_verify_efi "$KEYS/db.crt" "$WORK/signed.efi"
    cp "$WORK/signed.efi" "$WORK/mnt/usr/lib/boot/bootloader.efi"
    boot_unmount "$WORK/mnt"
    mcopy -o -i "$image@@$offset" "$WORK/signed.efi" "$loader"
    echo "secure-boot-keys: the bootloader of $image is signed with $KEYS/db.crt"
}

export_keys() {
    [ $# -eq 1 ] || usage
    local out="$1" name signer owner attrs
    boot_need sbsiglist "sbsigntools (sbsigntool on Debian and Ubuntu)"
    boot_need sbvarsign "sbsigntools (sbsigntool on Debian and Ubuntu)"
    have PK
    have KEK
    [ -f "$KEYS/db.crt" ] || boot_die "$KEYS/db.crt is missing; run $0 -k $KEYS create first"
    [ -f "$KEYS/owner.guid" ] || boot_die "$KEYS/owner.guid is missing; run $0 -k $KEYS create first"
    owner="$(cat "$KEYS/owner.guid")"
    mkdir -p "$out"
    attrs=NON_VOLATILE,BOOTSERVICE_ACCESS,RUNTIME_ACCESS,TIME_BASED_AUTHENTICATED_WRITE_ACCESS
    for name in PK KEK db; do
        openssl x509 -in "$KEYS/$name.crt" -outform DER -out "$out/$name.cer"
        sbsiglist --owner "$owner" --type x509 --output "$out/$name.esl" "$out/$name.cer"
        case "$name" in
            PK) signer=PK ;;
            KEK) signer=PK ;;
            db) signer=KEK ;;
        esac
        local a="$attrs"
        [ "$name" = PK ] || a="$attrs,APPEND_WRITE"
        boot_with_key "$(key "$signer")" sbvarsign --key "$(key "$signer")" --cert "$KEYS/$signer.crt" \
            --attr "$a" --output "$out/$name.auth" "$name" "$out/$name.esl" >/dev/null
    done
    cp "$KEYS/owner.guid" "$out/owner.guid"
    readme > "$out/README.txt"
    if ! boot_is_uri "$(key db)"; then
        echo "secure-boot-keys: these are development keys; enroll them only on a test machine" >&2
    fi
    echo "secure-boot-keys: $out holds PK, KEK and db as .cer, .esl and .auth, and README.txt"
}

readme() {
    cat <<'EOF'
FerrionOS Secure Boot keys

These files let a PC's firmware trust the FerrionOS bootloader with Secure Boot on. A PC trusts
only Microsoft's keys out of the box, so its owner enrolls FerrionOS's once.

  db.cer    FerrionOS's signing certificate (DER). The FerrionOS bootloader is signed with it.
  KEK.cer   FerrionOS's key-exchange certificate. With it enrolled, FerrionOS can later send
            signed updates to db (a new signing certificate) and dbx (revoked bootloaders).
  PK.cer    FerrionOS's platform certificate. Whoever holds the platform key controls which
            keys the firmware accepts: enroll it only if FerrionOS should have that control.
  *.esl     the same certificates as EFI signature lists.
  *.auth    the same certificates as signed updates: PK.auth replaces the platform key;
            KEK.auth (signed with PK) and db.auth (signed with KEK) add to the keys already
            enrolled.
  owner.guid  the owner GUID written into each list.

Keep Microsoft's keys. Graphics cards, network cards and disk controllers carry firmware of
their own (option ROMs) signed by Microsoft; a PC whose db lacks Microsoft's certificate may
not start them with Secure Boot on, and can lose its display.

Before you start: copy this folder to a USB stick formatted FAT32, and know how to enter your
PC's firmware setup (often F2, F10, F12 or Delete at power-on).

Way 1: add db.cer in the firmware menu. Firmware differs; the names below are common ones.
  1. In firmware setup, open the Secure Boot page. If the keys can only be changed in a
     "Custom" mode, choose it.
  2. Open key management, choose the "db" (Authorized Signatures) entry, and choose Append
     (never Set, Replace or Delete). Pick db.cer on the USB stick; if the menu asks for
     another format, pick db.esl or db.auth.
  3. Turn Secure Boot on, save and restart.

Way 2: with sbctl from a Linux system on that PC, keeping your own platform key.
  1. In firmware setup, put Secure Boot into Setup Mode: clear the keys, or at least the
     Platform Key, and leave Secure Boot enabled. Restart into Linux.
  2. As root:
       sbctl create-keys
       mkdir -p /var/lib/sbctl/keys/custom/db /var/lib/sbctl/keys/custom/KEK
       cp db.cer /var/lib/sbctl/keys/custom/db/FerrionOS-db.cer
       cp KEK.cer /var/lib/sbctl/keys/custom/KEK/FerrionOS-KEK.cer
       sbctl enroll-keys --microsoft --custom
     Sign that Linux system's own boot files with sbctl too, or it no longer starts.
  3. Restart into firmware setup, check that Secure Boot is on, set a firmware password, save.

Way 3: with the .auth files and efitools from a Linux system, FerrionOS's platform key.
  1. In firmware setup, delete only the Platform Key (this enters Setup Mode and keeps the
     KEK and db that hold Microsoft's keys). Restart into Linux.
  2. As root, in this order (the platform key goes first, and each later file is checked
     against the key enrolled before it):
       efi-updatevar -f PK.auth PK
       efi-updatevar -a -f KEK.auth KEK
       efi-updatevar -a -f db.auth db
     If a command fails with "Operation not permitted", run chattr -i on the variable's
     file under /sys/firmware/efi/efivars/ and repeat it.
  3. Restart into firmware setup, check that Secure Boot is on, set a firmware password, save.

Then start FerrionOS. A PC that refuses it says so before the FerrionOS menu appears, often as
"Secure Boot violation" or "Access Denied": check that db.cer was appended to db, not to KEK
or dbx.
EOF
}

case "$COMMAND" in
    create) create "$@" ;;
    sign) sign "$@" ;;
    export) export_keys "$@" ;;
    *) usage ;;
esac
