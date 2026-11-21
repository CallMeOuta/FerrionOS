#!/usr/bin/env bash
# Proves UEFI Secure Boot for an x86_64 FerrionOS image in QEMU (security roadmap S16, slices 1
# and 2). It makes test keys (PK, KEK, db and one key that is never enrolled), enrolls PK, KEK
# and db into a copy of OVMF's variable store with Secure Boot on, and boots copies of the image
# with OVMF_CODE.secboot, each with a different bootloader in the EFI system partition:
#   signed         signed with the db key: must reach the graphical login
#   unsigned       as built: the firmware must refuse it
#   wrong-key      signed with the key that is not in db: the firmware must refuse it
# On a verified-boot image (VERIFIED_BOOT=1, its bootloader holds a key) three more cases boot
# the signed bootloader on a copy whose files were changed; the bootloader must refuse each:
#   tamper-kernel  one byte of usr/lib/boot/kernel changed
#   tamper-initfs  one byte of usr/lib/boot/initfs changed
#   tamper-sig     usr/lib/boot/manifest.sig signed by another key
# The image itself is never changed. Each case leaves serial.log and screen.png under the
# output directory, beside summary.txt and the enrolled variable store. The keys are test
# keys, stored unencrypted: never sign anything that leaves the build machine with them.
# Run from the repository root.

set -euo pipefail

usage() {
    cat <<EOF
Proves UEFI Secure Boot for an x86_64 FerrionOS image in QEMU.

Usage: $0 [-o <out-dir>] [-k <key-dir>] [-t <seconds>] [-r <redoxfs>] <harddrive.img>

  -o <out-dir>   results: <case>/serial.log, <case>/screen.png, summary.txt
                 (default: build/secure-boot-test)
  -k <key-dir>   PK, KEK and db as <name>.key and <name>.crt, and owner.guid; missing ones are
                 generated (default: <out-dir>/keys). The development keys of
                 scripts/secure-boot-keys.sh (build/secure-boot-keys) work here too
  -t <seconds>   how long one boot may take to reach the login or the refusal (default: 120)
  -r <redoxfs>   the FUSE tool that mounts the image's filesystem, for the tamper cases of a
                 verified-boot image (default: build/fstools/bin/redoxfs)

Needs qemu-system-x86_64, openssl, mtools (mcopy), python3, virt-fw-vars (package
virt-firmware) and sbsign (package sbsigntools) or osslsigncode. OVMF is looked for in
/usr/share/edk2/x64, /usr/share/OVMF and /usr/share/edk2/ovmf; OVMF_CODE and OVMF_VARS
in the environment override it. Exits 1 when any case fails.

Example: $0 build/x86_64/desktop/harddrive.img
EOF
    exit 2
}

OUT=build/secure-boot-test
KEYS=
TIMEOUT=120
REDOXFS_BIN=build/fstools/bin/redoxfs
while getopts "o:k:t:r:h" opt; do
    case "$opt" in
        o) OUT="$OPTARG" ;;
        k) KEYS="$OPTARG" ;;
        t) TIMEOUT="$OPTARG" ;;
        r) REDOXFS_BIN="$OPTARG" ;;
        *) usage ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 1 ] || usage
IMAGE="$1"
[ -f "$IMAGE" ] || { echo "secure-boot-test: $IMAGE is not a file" >&2; exit 2; }
[[ "$TIMEOUT" =~ ^[0-9]+$ ]] || usage
KEYS="${KEYS:-$OUT/keys}"

BOOT_PROG=secure-boot-test
. scripts/boot-signing-lib.sh
boot_need qemu-system-x86_64 "QEMU"
boot_need openssl "OpenSSL"
boot_need mcopy "mtools"
boot_need python3 "Python 3"
boot_need virt-fw-vars "virt-firmware (Arch: pacman -S virt-firmware; elsewhere: pip install virt-firmware)"
sb_find_signer

find_firmware() {
    local name
    for name in "$@"; do
        [ -f "$name" ] && { echo "$name"; return; }
    done
}
OVMF_CODE="${OVMF_CODE:-$(find_firmware /usr/share/edk2/x64/OVMF_CODE.secboot.4m.fd /usr/share/OVMF/OVMF_CODE_4M.secboot.fd /usr/share/edk2/ovmf/OVMF_CODE.secboot.fd)}"
OVMF_VARS="${OVMF_VARS:-$(find_firmware /usr/share/edk2/x64/OVMF_VARS.4m.fd /usr/share/OVMF/OVMF_VARS_4M.fd /usr/share/edk2/ovmf/OVMF_VARS.fd)}"
if [ -z "$OVMF_CODE" ] || [ -z "$OVMF_VARS" ]; then
    echo "secure-boot-test: OVMF with Secure Boot not found; install edk2-ovmf (Arch, Fedora) or ovmf (Debian, Ubuntu), or set OVMF_CODE and OVMF_VARS" >&2
    exit 2
fi

mkdir -p "$OUT" "$KEYS"
WORK="$(mktemp -d "$OUT/work.XXXXXX")"
BOOT_WORK="$WORK"
MOUNTED=0
cleanup() {
    if [ "$MOUNTED" = 1 ]; then
        $BOOT_FUMOUNT "$WORK/mnt" || true
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

# Test keys, and one that is never enrolled, made fresh each run.
for name in PK KEK db; do
    sb_make_cert "$KEYS" "$name" "/CN=FerrionOS Secure Boot test $name/"
done
sb_make_cert "$WORK" other "/CN=FerrionOS Secure Boot test other/"
[ -f "$KEYS/owner.guid" ] || python3 -c 'import uuid; print(uuid.uuid4())' > "$KEYS/owner.guid"
OWNER="$(cat "$KEYS/owner.guid")"

# PK, KEK and db enrolled, Secure Boot on: the firmware leaves setup mode.
VARS="$OUT/OVMF_VARS.enrolled.fd"
virt-fw-vars --loglevel warning -i "$OVMF_VARS" -o "$VARS" \
    --set-pk "$OWNER" "$KEYS/PK.crt" --add-kek "$OWNER" "$KEYS/KEK.crt" \
    --add-db "$OWNER" "$KEYS/db.crt" --secure-boot

ESP_RANGE="$(boot_esp_range "$IMAGE")"
read -r ESP_OFFSET _ <<< "$ESP_RANGE"
BOOT_PATH="$(boot_esp_loader "$IMAGE" "$ESP_OFFSET")"

cp --reflink=auto "$IMAGE" "$WORK/harddrive.img"
mcopy -n -i "$WORK/harddrive.img@@$ESP_OFFSET" "$BOOT_PATH" "$WORK/unsigned.efi"
! sb_is_signed "$WORK/unsigned.efi" || boot_die "the bootloader of $IMAGE is already signed; test an image as built"

sb_sign_efi "$KEYS/db.key" "$KEYS/db.crt" "$WORK/unsigned.efi" "$WORK/signed.efi"
sb_sign_efi "$WORK/other.key" "$WORK/other.crt" "$WORK/unsigned.efi" "$WORK/wrong-key.efi"
sb_verify_efi "$KEYS/db.crt" "$WORK/signed.efi"

KVM=0
[ -w /dev/kvm ] && KVM=1

# A verified-boot image: the bootloader's key slot (after the marker FerrionBootKey/1) holds a key.
VERIFIED="$(python3 - "$WORK/unsigned.efi" <<'EOF'
import sys
data = open(sys.argv[1], "rb").read()
at = data.find(b"FerrionBootKey/1")
print(1 if at >= 0 and any(data[at + 16:at + 48]) else 0)
EOF
)"

# Makes $WORK/<case>.img: a copy of the image with one change in its filesystem.
tamper() {
    local name="$1"
    cp --reflink=auto "$WORK/harddrive.img" "$WORK/$name.img"
    MOUNTED=1
    boot_mount "$REDOXFS_BIN" "$WORK/$name.img" "$WORK/mnt"
    local boot="$WORK/mnt/usr/lib/boot"
    case "$name" in
        tamper-kernel) flip_byte "$boot/kernel" ;;
        tamper-initfs) flip_byte "$boot/initfs" ;;
        tamper-sig)
            openssl genpkey -algorithm ed25519 -out "$WORK/other-boot.key"
            boot_sign_manifest "$WORK/other-boot.key" "$boot/manifest" "$boot/manifest.sig"
            ;;
    esac
    sync
    $BOOT_FUMOUNT "$WORK/mnt"
    MOUNTED=0
}

# Inverts the byte in the middle of a file, in place.
flip_byte() {
    python3 - "$1" <<'EOF'
import os, sys
with open(sys.argv[1], "r+b") as f:
    middle = os.fstat(f.fileno()).st_size // 2
    f.seek(middle)
    byte = f.read(1)[0]
    f.seek(middle)
    f.write(bytes([byte ^ 0xff]))
EOF
}

# Boots one case and judges it from the serial console and QEMU's screen. The screen
# classifier is the one scripts/vm-test.py uses. Arguments: case name, expected outcome
# (boot, refuse, verified-refuse), bootloader file, disk image.
boot_case() {
    local name="$1" expect="$2" efi="$3" disk="$4"
    mkdir -p "$OUT/$name"
    cp "$VARS" "$WORK/vars.fd"
    mcopy -o -i "$disk@@$ESP_OFFSET" "$efi" "$BOOT_PATH"
    # -B: loading scripts/vm-test.py leaves no __pycache__ in the tree
    python3 -B - "$name" "$expect" "$OUT/$name" "$WORK" "$OVMF_CODE" "$TIMEOUT" "$KVM" "$disk" <<'EOF'
import importlib.util, re, socket, subprocess, sys, time
from pathlib import Path

name, expect, out, work, code, timeout, kvm, disk = sys.argv[1:]
out, work, timeout = Path(out), Path(work), float(timeout)
spec = importlib.util.spec_from_file_location("vm_test", Path("scripts/vm-test.py"))
vm_test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vm_test)

monitor_path = work / "monitor.sock"
serial_path = out / "serial.log"
command = [
    "qemu-system-x86_64", "-name", f"FerrionOS secure-boot-test {name}",
    # OVMF's Secure Boot build keeps its variables behind SMM.
    "-machine", "q35,smm=on", "-global", "driver=cfi.pflash01,property=secure,value=on",
    "-global", "ICH9-LPC.disable_s3=1",
    *(["-enable-kvm", "-cpu", "host"] if kvm == "1" else ["-cpu", "max"]),
    "-smp", "4", "-m", "2048",
    "-drive", f"if=pflash,format=raw,unit=0,file={code},readonly=on",
    "-drive", f"if=pflash,format=raw,unit=1,file={work / 'vars.fd'}",
    "-drive", f"file={disk},format=raw,if=none,id=drv0,snapshot=on",
    "-device", "nvme,drive=drv0,serial=NVME_SERIAL", "-device", "qemu-xhci",
    "-vga", "std", "-display", "none", "-nic", "none",
    "-serial", f"file:{serial_path}", "-monitor", f"unix:{monitor_path},server=on,wait=off",
]
proc = subprocess.Popen(command, stdin=subprocess.DEVNULL)
start = time.monotonic()


def reply(sock):
    data = b""
    while not data.endswith(b"(qemu) "):
        chunk = sock.recv(65536)
        if not chunk:
            raise RuntimeError("QEMU closed its monitor")
        data += chunk
    return data


while True:
    if proc.poll() is not None:
        print(f"FAIL {name}: QEMU exited with {proc.returncode}")
        sys.exit(1)
    try:
        monitor = socket.socket(socket.AF_UNIX)
        monitor.connect(str(monitor_path))
        reply(monitor)
        break
    except (FileNotFoundError, ConnectionRefusedError):
        monitor.close()
        time.sleep(0.05)


def screen():
    path = work / "frame.ppm"
    monitor.sendall(f"screendump {path}\n".encode())
    reply(monitor)
    return vm_test.Frame.from_ppm(path.read_bytes())


def serial():
    return serial_path.read_bytes().decode(errors="replace") if serial_path.exists() else ""


REFUSED = re.compile(r"BdsDxe: failed to load .*Access Denied.*")
VERIFIED_REFUSED = re.compile(r"FerrionOS verified boot refused to start: .*")
KERNEL = re.compile(r"^kernel::", re.M)  # the kernel prefixes its log lines with its module path
ok, note, frame = False, f"neither the login nor a refusal within {timeout:.0f}s", None
while time.monotonic() - start < timeout:
    frame = screen()
    log = serial()
    refused = REFUSED.search(log) or VERIFIED_REFUSED.search(log)
    if expect == "boot" and frame.kind() == "login":
        ok = not refused and KERNEL.search(log) is not None
        note = f"graphical login at {time.monotonic() - start:.1f}s" if ok else "login seen, but the serial log disagrees"
        break
    if refused:
        time.sleep(3)  # long enough for a bootloader that ran anyway to reach the kernel
        frame, log = screen(), serial()
        started = KERNEL.search(log) is not None
        by_firmware = refused.re is REFUSED
        ok = expect == ("refuse" if by_firmware else "verified-refuse") and not started
        note = f"{'firmware' if by_firmware else 'bootloader'}: {refused.group(0).strip()}" + ("; but the kernel started" if started else "")
        break
    time.sleep(0.5)
if frame is not None:
    frame.save_png(out / "screen.png")
monitor.sendall(b"quit\n")
try:
    proc.wait(timeout=20)
except subprocess.TimeoutExpired:
    proc.kill()
    proc.wait()
monitor.close()
print(f"{'PASS' if ok else 'FAIL'} {name} (expect {expect}): {note}")
sys.exit(0 if ok else 1)
EOF
}

FAILED=0
: > "$OUT/summary.txt"
for case in signed:boot unsigned:refuse wrong-key:refuse; do
    name="${case%%:*}"
    if ! boot_case "$name" "${case#*:}" "$WORK/$name.efi" "$WORK/harddrive.img" | tee -a "$OUT/summary.txt"; then
        FAILED=1
    fi
done
if [ "$VERIFIED" = 1 ]; then
    [ -x "$REDOXFS_BIN" ] || { echo "secure-boot-test: $REDOXFS_BIN is missing; the tamper cases need it (-r)" >&2; exit 2; }
    for name in tamper-kernel tamper-initfs tamper-sig; do
        tamper "$name"
        if ! boot_case "$name" verified-refuse "$WORK/signed.efi" "$WORK/$name.img" | tee -a "$OUT/summary.txt"; then
            FAILED=1
        fi
        rm -f "$WORK/$name.img"
    done
else
    echo "not a verified-boot image: tamper cases skipped" | tee -a "$OUT/summary.txt"
fi
{
    echo "image: $IMAGE"
    echo "firmware: $OVMF_CODE, variables enrolled in $VARS"
    echo "signer: $SB_SIGNER; keys: $KEYS"
} >> "$OUT/summary.txt"
echo "results in $OUT: summary.txt, <case>/serial.log, <case>/screen.png"
exit "$FAILED"
