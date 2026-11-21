# Helpers for the scripts that sign and check FerrionOS's boot files (secure-boot-keys.sh,
# secure-boot-test.sh, verified-boot-sign.sh). Source it after setting BOOT_PROG, the calling
# script's name, which prefixes every message:
#   BOOT_PROG=my-script; . scripts/boot-signing-lib.sh
# and set BOOT_WORK to the caller's scratch directory before signing with a token key.
# A key is a key file, or the PKCS#11 URI ("pkcs11:token=...;object=...") of a key on a hardware
# token, used through OpenSSL 3's pkcs11 provider (boot_pkcs11_conf). Nothing here changes the
# shell's options; the callers run with set -euo pipefail.

boot_die() {
    echo "$BOOT_PROG: $*" >&2
    exit "${BOOT_EXIT:-1}"
}

# boot_need <command> <what to install>: stops with exit code 2 when the command is missing.
boot_need() {
    command -v "$1" >/dev/null || BOOT_EXIT=2 boot_die "$1 is missing; install $2"
}

if command -v fusermount3 >/dev/null; then
    BOOT_FUMOUNT="fusermount3 -u"
else
    BOOT_FUMOUNT="fusermount -u"
fi

# boot_mount <redoxfs-binary> <image> <dir>: mounts the image's RedoxFS filesystem on <dir>
# through FUSE, in the background, and waits for usr/lib/boot/kernel to appear. The caller
# unmounts with $BOOT_FUMOUNT <dir>, also when it fails.
boot_mount() {
    local redoxfs="$1" image="$2" dir="$3"
    [ -x "$redoxfs" ] || BOOT_EXIT=2 boot_die "$redoxfs is not executable"
    mkdir -p "$dir"
    "$redoxfs" "$image" "$dir" >"$dir.log" 2>&1 &
    for _ in $(seq 100); do
        [ -f "$dir/usr/lib/boot/kernel" ] && return 0
        sleep 0.1
    done
    cat "$dir.log" >&2
    boot_die "could not mount $image"
}

# boot_esp_range <image>: prints the byte offset and the size of the image's EFI system
# partition, read from its GPT.
boot_esp_range() {
    python3 - "$1" "$BOOT_PROG" <<'EOF'
import struct, sys, uuid
ESP = uuid.UUID("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
image, prog = sys.argv[1:]
with open(image, "rb") as f:
    header = f.read(1024)[512:]
    if header[:8] != b"EFI PART":
        sys.exit(f"{prog}: no GPT in {image}")
    lba, count, size = struct.unpack_from("<QII", header, 72)
    f.seek(lba * 512)
    table = f.read(count * size)
for i in range(count):
    entry = table[i * size:(i + 1) * size]
    if uuid.UUID(bytes_le=entry[:16]) == ESP:
        first, last = struct.unpack_from("<QQ", entry, 32)
        print(first * 512, (last - first + 1) * 512)
        break
else:
    sys.exit(f"{prog}: no EFI system partition in {image}")
EOF
}

# boot_esp_loader <image> <esp-offset>: prints the mtools path of the bootloader the firmware
# starts from the EFI system partition, such as ::/EFI/BOOT/BOOTX64.EFI.
boot_esp_loader() {
    local found
    found="$(mdir -b -i "$1@@$2" ::/EFI/BOOT 2>/dev/null | grep -i -E '/BOOT[A-Z0-9]+\.EFI$' || true)"
    [ -n "$found" ] && [ "$(echo "$found" | wc -l)" -eq 1 ] \
        || boot_die "expected one EFI/BOOT/BOOT<arch>.EFI in the EFI system partition of $1"
    echo "$found"
}

# boot_is_uri <key>: true when the key is a PKCS#11 URI rather than a file.
boot_is_uri() {
    case "$1" in
        pkcs11:*) return 0 ;;
        *) return 1 ;;
    esac
}

# boot_pkcs11_conf: writes $BOOT_WORK/openssl-pkcs11.cnf, an OpenSSL configuration that loads
# the pkcs11 provider (package pkcs11-provider) beside the default one, and prints its path.
# The token's own PKCS#11 module comes from PKCS11_PROVIDER_MODULE (for a YubiKey,
# libykcs11.so from yubico-piv-tool) or, unset, from p11-kit's proxy. The provider offers no
# digests here, so a module that hashes with OpenSSL itself (SoftHSM, YKCS11) is not called
# back into, and it skips deinitialization, which crashes SoftHSM 2.7 at exit.
boot_pkcs11_conf() {
    local modules provider conf="$BOOT_WORK/openssl-pkcs11.cnf"
    modules="${OPENSSL_MODULES:-$(openssl version -m | sed -e 's/^MODULESDIR: "//' -e 's/"$//')}"
    provider="$modules/pkcs11.so"
    [ -f "$provider" ] || BOOT_EXIT=2 boot_die "the OpenSSL pkcs11 provider is not at $provider; install pkcs11-provider, or set OPENSSL_MODULES to its directory"
    cat > "$conf" <<EOF
openssl_conf = openssl_init
[openssl_init]
providers = provider_sect
[provider_sect]
default = default_sect
pkcs11 = pkcs11_sect
[default_sect]
activate = 1
[pkcs11_sect]
module = $provider
pkcs11-module-block-operations = digest
pkcs11-module-quirks = no-deinit
activate = 1
EOF
    echo "$conf"
}

# boot_with_key <key> <command...>: runs the command, with the pkcs11 provider loaded when the
# key is a PKCS#11 URI. The token asks for its PIN on the terminal unless the URI carries
# pin-source.
boot_with_key() {
    local key="$1"
    shift
    if boot_is_uri "$key"; then
        OPENSSL_CONF="$(boot_pkcs11_conf)" "$@"
    else
        "$@"
    fi
}

# boot_sign_manifest <key> <file> <signature>: the Ed25519 signature of a file that the
# verified-boot bootloader checks, 64 raw bytes.
boot_sign_manifest() {
    boot_with_key "$1" openssl pkeyutl -sign -rawin -inkey "$1" -in "$2" -out "$3"
}

# boot_public_pem <key> <out.pem>: the public half of a key file or token key.
boot_public_pem() {
    boot_with_key "$1" openssl pkey -in "$1" -pubout -out "$2"
}

# sb_find_signer: sets SB_SIGNER to the Authenticode signer found, sbsign or osslsigncode.
sb_find_signer() {
    if command -v sbsign >/dev/null; then
        SB_SIGNER=sbsign
    elif command -v osslsigncode >/dev/null; then
        SB_SIGNER=osslsigncode
    else
        BOOT_EXIT=2 boot_die "no signer; install sbsigntools (Arch, Fedora; sbsigntool on Debian and Ubuntu) or osslsigncode"
    fi
}

# sb_make_cert <key-dir> <name> <subject> [<token-key>]: makes <key-dir>/<name>.crt, a
# self-signed RSA certificate with SHA-256. Without a token key it also makes
# <key-dir>/<name>.key, RSA-2048 (the size every UEFI implementation must accept),
# unencrypted. An existing certificate is kept.
sb_make_cert() {
    local dir="$1" name="$2" subject="$3" key="${4:-}"
    [ -f "$dir/$name.crt" ] && return 0
    if [ -n "$key" ]; then
        boot_with_key "$key" openssl req -new -x509 -sha256 -days 3650 -key "$key" \
            -subj "$subject" -out "$dir/$name.crt"
    else
        (umask 077 && openssl req -quiet -new -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
            -subj "$subject" -keyout "$dir/$name.key" -out "$dir/$name.crt" 2>"$dir/$name.log") \
            || { cat "$dir/$name.log" >&2; rm -f "$dir/$name.log"; boot_die "could not make $name"; }
        rm -f "$dir/$name.log"
    fi
}

# sb_sign_efi <key> <cert> <in.efi> <out.efi>: Authenticode signature with $SB_SIGNER
# (sb_find_signer). A token key needs an sbsign with OpenSSL provider support (sbsigntools
# commit 53922bc1, which Arch's 0.9.5-5 carries).
sb_sign_efi() {
    local key="$1" cert="$2" in="$3" out="$4"
    if [ "$SB_SIGNER" = sbsign ]; then
        boot_with_key "$key" sbsign --key "$key" --cert "$cert" --output "$out" "$in" 2>"$out.log" \
            || { cat "$out.log" >&2; rm -f "$out.log"; boot_die "sbsign could not sign $in with $key"; }
        rm -f "$out.log"
    else
        boot_is_uri "$key" && BOOT_EXIT=2 boot_die "signing with a token key needs sbsign (sbsigntools)"
        osslsigncode sign -certs "$cert" -key "$key" -h sha256 -in "$in" -out "$out" >/dev/null
    fi
}

# sb_is_signed <file.efi>: true when the PE file's certificate table holds a signature.
sb_is_signed() {
    python3 - "$1" <<'EOF'
import struct, sys
data = open(sys.argv[1], "rb").read()
pe = struct.unpack_from("<I", data, 0x3c)[0]
optional = pe + 24
# PE32+ (0x20b) keeps its data directories 16 bytes further on than PE32 (0x10b).
directories = optional + (112 if struct.unpack_from("<H", data, optional)[0] == 0x20b else 96)
size = struct.unpack_from("<I", data, directories + 4 * 8 + 4)[0]
sys.exit(0 if size else 1)
EOF
}

# sb_verify_efi <cert> <file.efi>: checks that the file carries a signature by the certificate.
sb_verify_efi() {
    if [ "$SB_SIGNER" = sbsign ]; then
        sbverify --cert "$1" "$2" >/dev/null
    else
        osslsigncode verify -CAfile "$1" -in "$2" >/dev/null
    fi
}
