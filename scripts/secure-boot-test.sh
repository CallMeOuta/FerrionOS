#!/usr/bin/env bash
# Proves UEFI Secure Boot for an x86_64 FerrionOS image in QEMU (security roadmap S16, slice 1).
# It makes test keys (PK, KEK, db and one key that is never enrolled), enrolls PK, KEK and db
# into a copy of OVMF's variable store with Secure Boot on, and boots a copy of the image three
# times with OVMF_CODE.secboot, each with a different bootloader in the EFI system partition:
#   signed     signed with the db key: must reach the graphical login
#   unsigned   as built: the firmware must refuse it
#   wrong-key  signed with the key that is not in db: the firmware must refuse it
# The image itself is never changed. Each case leaves serial.log and screen.png under the
# output directory, beside summary.txt and the enrolled variable store. The keys are test
# keys, stored unencrypted: never sign anything that leaves the build machine with them.
# Run from the repository root.

set -euo pipefail

usage() {
    cat <<EOF
Proves UEFI Secure Boot for an x86_64 FerrionOS image in QEMU.

Usage: $0 [-o <out-dir>] [-k <key-dir>] [-t <seconds>] <harddrive.img>

  -o <out-dir>   results: <case>/serial.log, <case>/screen.png, summary.txt
                 (default: build/secure-boot-test)
  -k <key-dir>   PK, KEK, db and other as <name>.key and <name>.crt; missing ones are
                 generated (default: <out-dir>/keys)
  -t <seconds>   how long one boot may take to reach the login or the refusal (default: 120)

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
while getopts "o:k:t:h" opt; do
    case "$opt" in
        o) OUT="$OPTARG" ;;
        k) KEYS="$OPTARG" ;;
        t) TIMEOUT="$OPTARG" ;;
        *) usage ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 1 ] || usage
IMAGE="$1"
[ -f "$IMAGE" ] || { echo "secure-boot-test: $IMAGE is not a file" >&2; exit 2; }
[[ "$TIMEOUT" =~ ^[0-9]+$ ]] || usage
KEYS="${KEYS:-$OUT/keys}"

need() {
    command -v "$1" >/dev/null || { echo "secure-boot-test: $1 is missing; install $2" >&2; exit 2; }
}
need qemu-system-x86_64 "QEMU"
need openssl "OpenSSL"
need mcopy "mtools"
need python3 "Python 3"
need virt-fw-vars "virt-firmware (Arch: pacman -S virt-firmware; elsewhere: pip install virt-firmware)"
if command -v sbsign >/dev/null; then
    SIGNER=sbsign
elif command -v osslsigncode >/dev/null; then
    SIGNER=osslsigncode
else
    echo "secure-boot-test: no signer; install sbsigntools (Arch, Fedora; sbsigntool on Debian and Ubuntu) or osslsigncode" >&2
    exit 2
fi

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
trap 'rm -f "$WORK"/*; rmdir "$WORK"' EXIT

# Test keys: self-signed RSA-2048 with SHA-256, the size every UEFI implementation must accept.
for name in PK KEK db other; do
    if [ ! -f "$KEYS/$name.key" ] || [ ! -f "$KEYS/$name.crt" ]; then
        openssl req -quiet -new -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
            -subj "/CN=FerrionOS Secure Boot test $name/" \
            -keyout "$KEYS/$name.key" -out "$KEYS/$name.crt" 2>"$WORK/openssl.log" \
            || { cat "$WORK/openssl.log" >&2; exit 1; }
    fi
done
[ -f "$KEYS/owner.guid" ] || python3 -c 'import uuid; print(uuid.uuid4())' > "$KEYS/owner.guid"
OWNER="$(cat "$KEYS/owner.guid")"

# PK, KEK and db enrolled, Secure Boot on: the firmware leaves setup mode.
VARS="$OUT/OVMF_VARS.enrolled.fd"
virt-fw-vars --loglevel warning -i "$OVMF_VARS" -o "$VARS" \
    --set-pk "$OWNER" "$KEYS/PK.crt" --add-kek "$OWNER" "$KEYS/KEK.crt" \
    --add-db "$OWNER" "$KEYS/db.crt" --secure-boot

# The EFI system partition's byte offset, from the GPT.
ESP_OFFSET="$(python3 - "$IMAGE" <<'EOF'
import struct, sys, uuid
ESP = uuid.UUID("c12a7328-f81f-11d2-ba4b-00a0c93ec93b")
with open(sys.argv[1], "rb") as f:
    header = f.read(1024)[512:]
    if header[:8] != b"EFI PART":
        sys.exit("secure-boot-test: no GPT in the image")
    lba, count, size = struct.unpack_from("<QII", header, 72)
    f.seek(lba * 512)
    table = f.read(count * size)
for i in range(count):
    entry = table[i * size:(i + 1) * size]
    if uuid.UUID(bytes_le=entry[:16]) == ESP:
        print(struct.unpack_from("<Q", entry, 32)[0] * 512)
        break
else:
    sys.exit("secure-boot-test: no EFI system partition in the image")
EOF
)"
BOOT_PATH=::/EFI/BOOT/BOOTX64.EFI

cp --reflink=auto "$IMAGE" "$WORK/harddrive.img"
mcopy -n -i "$WORK/harddrive.img@@$ESP_OFFSET" "$BOOT_PATH" "$WORK/unsigned.efi"

sign() {
    if [ "$SIGNER" = sbsign ]; then
        sbsign --key "$KEYS/$1.key" --cert "$KEYS/$1.crt" --output "$2" "$WORK/unsigned.efi" 2>/dev/null
    else
        osslsigncode sign -certs "$KEYS/$1.crt" -key "$KEYS/$1.key" -h sha256 -in "$WORK/unsigned.efi" -out "$2" >/dev/null
    fi
}
sign db "$WORK/signed.efi"
sign other "$WORK/wrong-key.efi"
if [ "$SIGNER" = sbsign ]; then
    sbverify --cert "$KEYS/db.crt" "$WORK/signed.efi" >/dev/null
else
    osslsigncode verify -CAfile "$KEYS/db.crt" -in "$WORK/signed.efi" >/dev/null
fi

KVM=0
[ -w /dev/kvm ] && KVM=1

# Boots one case and judges it from the serial console and QEMU's screen. The screen
# classifier is the one scripts/vm-test.py uses.
boot_case() {
    local name="$1" expect="$2"
    mkdir -p "$OUT/$name"
    cp "$VARS" "$WORK/vars.fd"
    mcopy -o -i "$WORK/harddrive.img@@$ESP_OFFSET" "$WORK/$name.efi" "$BOOT_PATH"
    # -B: loading scripts/vm-test.py leaves no __pycache__ in the tree
    python3 -B - "$name" "$expect" "$OUT/$name" "$WORK" "$OVMF_CODE" "$TIMEOUT" "$KVM" <<'EOF'
import importlib.util, re, socket, subprocess, sys, time
from pathlib import Path

name, expect, out, work, code, timeout, kvm = sys.argv[1:]
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
    "-drive", f"file={work / 'harddrive.img'},format=raw,if=none,id=drv0,snapshot=on",
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
KERNEL = re.compile(r"^kernel::", re.M)  # the kernel prefixes its log lines with its module path
ok, note, frame = False, f"neither the login nor a refusal within {timeout:.0f}s", None
while time.monotonic() - start < timeout:
    frame = screen()
    log = serial()
    refused = REFUSED.search(log)
    if expect == "boot" and frame.kind() == "login":
        ok = not refused and KERNEL.search(log) is not None
        note = f"graphical login at {time.monotonic() - start:.1f}s" if ok else "login seen, but the serial log disagrees"
        break
    if refused:
        time.sleep(3)  # long enough for a bootloader that ran anyway to reach the kernel
        frame, log = screen(), serial()
        started = KERNEL.search(log) is not None
        ok = expect == "refuse" and not started
        note = f"firmware: {refused.group(0).strip()}" + ("; but the kernel started" if started else "")
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
    if ! boot_case "${case%%:*}" "${case#*:}" | tee -a "$OUT/summary.txt"; then
        FAILED=1
    fi
done
{
    echo "image: $IMAGE"
    echo "firmware: $OVMF_CODE, variables enrolled in $VARS"
    echo "signer: $SIGNER; keys: $KEYS"
} >> "$OUT/summary.txt"
echo "results in $OUT: summary.txt, <case>/serial.log, <case>/screen.png"
exit "$FAILED"
