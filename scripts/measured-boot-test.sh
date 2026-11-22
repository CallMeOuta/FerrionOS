#!/usr/bin/env bash

set -euo pipefail

usage() {
    cat <<EOF
Proves measured boot for an x86_64 FerrionOS image in QEMU with a software TPM.

Usage: $0 [-o <out-dir>] [-t <seconds>] [-r <redoxfs>] [-k <boot.key>] <harddrive.img>

  -o <out-dir>   results: <case>/serial.log, <case>/screen.png, <case>/pcrs.txt,
                 <case>/eventlog.bin, <case>/eventlog.yaml, summary.txt
                 (default: build/measured-boot-test)
  -t <seconds>   how long one boot may take to reach the login (default: 120)
  -r <redoxfs>   the FUSE tool that mounts the image's filesystem
                 (default: build/fstools/bin/redoxfs)
  -k <boot.key>  the verified-boot key, a file or a PKCS#11 URI, that signs the manifest again
                 in the tpm-initfs case of a verified-boot image
                 (default: build/verified-boot/boot.key)

Needs qemu-system-x86_64, swtpm and swtpm_ioctl (package swtpm), tpm2_pcrread and
tpm2_eventlog (package tpm2-tools), mtools (mcopy), python3, FUSE, and openssl 3 or later for
a verified-boot image. OVMF is looked for in /usr/share/edk2/x64, /usr/share/OVMF and
/usr/share/edk2/ovmf; OVMF_CODE and OVMF_VARS in the environment override it. Exits 1 when
any case fails, 3 when none failed but one was blocked by two firmware stalls.

Example: $0 build/x86_64/desktop/harddrive.img
EOF
    exit 2
}

OUT=build/measured-boot-test
TIMEOUT=120
REDOXFS_BIN=build/fstools/bin/redoxfs
BOOT_KEY=build/verified-boot/boot.key
while getopts "o:t:r:k:h" opt; do
    case "$opt" in
        o) OUT="$OPTARG" ;;
        t) TIMEOUT="$OPTARG" ;;
        r) REDOXFS_BIN="$OPTARG" ;;
        k) BOOT_KEY="$OPTARG" ;;
        *) usage ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 1 ] || usage
IMAGE="$1"
BOOT_PROG=measured-boot-test
. scripts/boot-signing-lib.sh
[ -f "$IMAGE" ] || BOOT_EXIT=2 boot_die "$IMAGE is not a file"
[[ "$TIMEOUT" =~ ^[0-9]+$ ]] || usage

boot_need qemu-system-x86_64 "QEMU"
boot_need swtpm "swtpm"
boot_need swtpm_ioctl "swtpm"
boot_need tpm2_pcrread "tpm2-tools"
boot_need tpm2_eventlog "tpm2-tools"
boot_need mcopy "mtools"
boot_need python3 "Python 3"
[ -x "$REDOXFS_BIN" ] || BOOT_EXIT=2 boot_die "$REDOXFS_BIN is missing (-r)"

find_firmware() {
    local name
    for name in "$@"; do
        [ -f "$name" ] && { echo "$name"; return; }
    done
}
OVMF_CODE="${OVMF_CODE:-$(find_firmware /usr/share/edk2/x64/OVMF_CODE.4m.fd /usr/share/OVMF/OVMF_CODE_4M.fd /usr/share/edk2/ovmf/OVMF_CODE.fd)}"
OVMF_VARS="${OVMF_VARS:-$(find_firmware /usr/share/edk2/x64/OVMF_VARS.4m.fd /usr/share/OVMF/OVMF_VARS_4M.fd /usr/share/edk2/ovmf/OVMF_VARS.fd)}"
if [ -z "$OVMF_CODE" ] || [ -z "$OVMF_VARS" ]; then
    BOOT_EXIT=2 boot_die "OVMF not found; install edk2-ovmf (Arch, Fedora) or ovmf (Debian, Ubuntu), or set OVMF_CODE and OVMF_VARS"
fi

mkdir -p "$OUT"
WORK="$(mktemp -d "$OUT/work.XXXXXX")"
BOOT_WORK="$WORK"
trap boot_cleanup EXIT

range="$(boot_esp_range "$IMAGE")"
read -r ESP_OFFSET _ <<< "$range"
cp --reflink=auto "$IMAGE" "$WORK/harddrive.img"
mcopy -n -i "$WORK/harddrive.img@@$ESP_OFFSET" "$(boot_esp_loader "$WORK/harddrive.img" "$ESP_OFFSET")" "$WORK/bootloader.efi"
SLOT="$(python3 - "$WORK/bootloader.efi" <<'EOF'
import sys
data = open(sys.argv[1], "rb").read()
at = data.find(b"FerrionBootKey/1")
if at < 0:
    sys.exit("measured-boot-test: the bootloader has no key slot; is it cooked with ferrion-verified-boot.patch?")
print(data[at + 16:at + 48].hex())
EOF
)"
VERIFIED=0
[ "$SLOT" != "$(printf '0%.0s' $(seq 64))" ] && VERIFIED=1

prepare() {
    local name="$1"
    cp --reflink=auto "$WORK/harddrive.img" "$WORK/$name.img"
    mkdir -p "$WORK/$name.files"
    boot_mount "$REDOXFS_BIN" "$WORK/$name.img" "$WORK/mnt"
    local boot="$WORK/mnt/usr/lib/boot"
    if [ "$name" = tpm-initfs ]; then
        head -c 4096 /dev/zero >> "$boot/initfs"
        if [ "$VERIFIED" = 1 ]; then
            boot_is_uri "$BOOT_KEY" || [ -f "$BOOT_KEY" ] \
                || BOOT_EXIT=2 boot_die "$BOOT_KEY is missing; the tpm-initfs case of a verified-boot image signs with it (-k)"
            boot_public_pem "$BOOT_KEY" "$WORK/boot.pub.pem"
            [ "$(openssl pkey -pubin -in "$WORK/boot.pub.pem" -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')" = "$SLOT" ] \
                || BOOT_EXIT=2 boot_die "$BOOT_KEY is not the key in the image's bootloader"
            {
                echo "ferrion-boot-manifest 1"
                echo "kernel $(openssl dgst -sha512 -r "$boot/kernel" | cut -d' ' -f1)"
                echo "initfs $(openssl dgst -sha512 -r "$boot/initfs" | cut -d' ' -f1)"
            } > "$boot/manifest"
            boot_sign_manifest "$BOOT_KEY" "$boot/manifest" "$boot/manifest.sig"
        fi
    fi
    cp "$boot/kernel" "$boot/initfs" "$WORK/$name.files/"
    [ "$VERIFIED" = 0 ] || cp "$boot/manifest" "$WORK/$name.files/"
    boot_unmount "$WORK/mnt"
}

KVM=0
[ -w /dev/kvm ] && KVM=1

boot_case() {
    local name="$1" disk="$2" files="$3"
    mkdir -p "$OUT/$name"
    cp "$OVMF_VARS" "$WORK/vars.fd"
    python3 -B - "$name" "$disk" "$files" "$OUT/$name" "$WORK" "$OVMF_CODE" "$TIMEOUT" "$KVM" "$SLOT" <<'EOF'
import hashlib, importlib.util, mmap, re, shutil, socket, struct, subprocess, sys, time
from pathlib import Path

name, disk, files, out, work, code, timeout, kvm, slot = sys.argv[1:]
out, work, timeout = Path(out), Path(work), float(timeout)
spec = importlib.util.spec_from_file_location("vm_test", Path("scripts/vm-test.py"))
vm_test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vm_test)
sha256 = lambda data: hashlib.sha256(data).digest()

MEMORY = 2048 * 1024 * 1024
EV_NO_ACTION, EV_SEPARATOR, EV_IPL, EV_EFI_VARIABLE_AUTHORITY = 0x3, 0x4, 0xD, 0x800000E0
SEPARATOR = sha256(bytes(4))
FERRION_GUID = bytes.fromhex("f1c169e593793b458b3a2109d5d9da22")  # e569c1f1-7993-453b-8b3a-2109d5d9da22
MONITOR_TIMEOUT_S = 30.0
MONITOR_CONNECT_TRIES = 200
CHILD_TERM_WAIT_S = 5.0
CHILD_KILL_WAIT_S = 10.0
PORT_ATTEMPTS = 256
SWTPM_START_TRIES = 100
SWTPM_SHUTDOWN_WAIT_S = 20.0

children = []
monitor = None


def spawn(command, **kwargs):
    child = subprocess.Popen(command, **kwargs)
    children.append(child)
    return child


def cleanup_children():
    global monitor
    if monitor is not None:
        try:
            monitor.close()
        except OSError:
            pass
        monitor = None
    for child in reversed(children):
        if child.poll() is not None:
            continue
        try:
            child.terminate()
            child.wait(timeout=CHILD_TERM_WAIT_S)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait(timeout=CHILD_KILL_WAIT_S)
        except OSError:
            pass


def record_result(verdict):
    assert verdict in ("pass", "fail", "stall", "blocked"), f"invalid result {verdict!r}"
    (out / "result").write_text(verdict + "\n")


def fail(note):
    record_result("fail")
    print(f"FAIL {name}: {note}")
    sys.exit(1)


def free_ports():
    """Two free TCP ports in a row: the swtpm TCTI finds the control port after the data port."""
    for _ in range(PORT_ATTEMPTS):
        with socket.socket() as data, socket.socket() as control:
            data.bind(("127.0.0.1", 0))
            port = data.getsockname()[1]
            try:
                control.bind(("127.0.0.1", port + 1))
            except OSError:
                continue
            return port, port + 1
    fail(f"no consecutive free TCP ports found in {PORT_ATTEMPTS} attempts")


def run(*command):
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        fail(f"{' '.join(command)} failed: {result.stderr.strip()}")
    return result.stdout


def reply(sock):
    data = b""
    while not data.endswith(b"(qemu) "):
        chunk = sock.recv(65536)
        if not chunk:
            raise RuntimeError("QEMU closed its monitor")
        data += chunk
    return data


def screen():
    path = work / "frame.ppm"
    monitor.sendall(f"screendump {path}\n".encode())
    reply(monitor)
    return vm_test.Frame.from_ppm(path.read_bytes())


def serial():
    return serial_path.read_bytes().decode(errors="replace") if serial_path.exists() else ""


tpm = files != ""
state = work / "tpm-state"
ctrl = work / "swtpm-ctrl.sock"
monitor_path = work / "monitor.sock"
serial_path = out / "serial.log"
try:
    if tpm:
        shutil.rmtree(state, ignore_errors=True)
        state.mkdir()
        swtpm = spawn(["swtpm", "socket", "--tpm2", "--tpmstate", f"dir={state}",
                       "--ctrl", f"type=unixio,path={ctrl}",
                       "--log", f"file={out / 'swtpm.log'}"],
                      stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(SWTPM_START_TRIES):
            if swtpm.poll() is not None:
                fail(f"swtpm exited with {swtpm.returncode}")
            if ctrl.exists():
                break
            time.sleep(0.05)
        else:
            fail(f"swtpm did not create {ctrl} within {SWTPM_START_TRIES * 0.05:.1f}s")

    command = [
        "qemu-system-x86_64", "-name", f"FerrionOS measured-boot-test {name}",
        "-machine", "q35",
        *(["-enable-kvm", "-cpu", "host"] if kvm == "1" else ["-cpu", "max"]),
        "-smp", "4", "-m", str(MEMORY // (1024 * 1024)),
        "-drive", f"if=pflash,format=raw,unit=0,file={code},readonly=on",
        "-drive", f"if=pflash,format=raw,unit=1,file={work / 'vars.fd'}",
        "-drive", f"file={disk},format=raw,if=none,id=drv0,snapshot=on",
        "-device", "nvme,drive=drv0,serial=NVME_SERIAL", "-device", "qemu-xhci",
        *(["-chardev", f"socket,id=chrtpm,path={ctrl}", "-tpmdev", "emulator,id=tpm0,chardev=chrtpm",
           "-device", "tpm-crb,tpmdev=tpm0"] if tpm else []),
        "-vga", "std", "-display", "none", "-nic", "none",
        "-serial", f"file:{serial_path}", "-monitor", f"unix:{monitor_path},server=on,wait=off",
    ]
    proc = spawn(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    start = time.monotonic()

    for _ in range(MONITOR_CONNECT_TRIES):
        if proc.poll() is not None:
            fail(f"QEMU exited with {proc.returncode}")
        try:
            monitor = socket.socket(socket.AF_UNIX)
            monitor.settimeout(MONITOR_TIMEOUT_S)
            monitor.connect(str(monitor_path))
            reply(monitor)
            break
        except (FileNotFoundError, ConnectionRefusedError):
            monitor.close()
            monitor = None
            time.sleep(0.05)
    else:
        fail(f"QEMU monitor did not appear within {MONITOR_CONNECT_TRIES * 0.05:.1f}s")

    KERNEL = re.compile(r"^kernel::", re.M)  # the kernel prefixes its log lines with its module path
    if name == "tpm-menu":
        # A key within the bootloader's first second asks for its mode menu, and Enter takes the
        # default mode: a boot whose environment has no QUIET=1.
        while time.monotonic() - start < timeout and "BdsDxe: starting" not in serial():
            # Not a plain sleep: while nothing arrives on QEMU's monitor, OVMF loses its swtpm in
            # most boots, so this wait keeps asking the monitor, as the screenshot loop below does.
            monitor.sendall(b"info status\n")
            reply(monitor)
            time.sleep(0.05)
        for _ in range(10):
            monitor.sendall(b"sendkey spc\n")
            reply(monitor)
            time.sleep(0.1)
        while time.monotonic() - start < timeout and "Arrow keys and enter select mode" not in serial():
            time.sleep(0.1)
        monitor.sendall(b"sendkey ret\n")
        reply(monitor)
    login, frame = None, None
    while time.monotonic() - start < timeout:
        frame = screen()
        if frame.kind() == "login" and KERNEL.search(serial()):
            login = time.monotonic() - start
            break
        time.sleep(0.5)
    if frame is not None:
        frame.save_png(out / "screen.png")

    log = None
    if tpm and login is not None:
        # The bootloader's copy of the event log, found through the environment it handed over.
        dump = work / "memory.bin"
        monitor.sendall(f'pmemsave 0 {MEMORY:#x} "{dump}"\n'.encode())
        reply(monitor)
        with open(dump, "rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as memory:
            found = re.search(rb"TPM_EVENT_LOG_ADDR=([0-9a-f]{16})\nTPM_EVENT_LOG_SIZE=([0-9a-f]{16})\n", memory)
            if found:
                addr, size = int(found[1], 16), int(found[2], 16)
                if addr + size <= len(memory):
                    log = memory[addr:addr + size]
        dump.unlink()
    # Killed rather than quit: a clean exit would shut the TPM down and drop its volatile state.
    proc.kill()
    proc.wait(timeout=CHILD_KILL_WAIT_S)
    monitor.close()
    monitor = None
    if login is None:
        if "BdsDxe: starting" not in serial():
            # The firmware never started the bootloader: the caller retries once.
            record_result("stall")
            print(f"STALL {name}: the firmware did not start the bootloader within {timeout:.0f}s")
            sys.exit(3)
        fail(f"no graphical login within {timeout:.0f}s")
    if not tpm:
        record_result("pass")
        print(f"PASS {name}: graphical login at {login:.1f}s without a TPM")
        sys.exit(0)

    # Saves the TPM's volatile state, then resumes it in a second swtpm that tpm2-tools can reach.
    run("swtpm_ioctl", "--unix", str(ctrl), "-v")
    run("swtpm_ioctl", "--unix", str(ctrl), "-s")
    swtpm.wait(timeout=SWTPM_SHUTDOWN_WAIT_S)
    port, ctrl_port = free_ports()
    swtpm = spawn(["swtpm", "socket", "--tpm2", "--tpmstate", f"dir={state}",
                   "--server", f"type=tcp,port={port},bindaddr=127.0.0.1",
                   "--ctrl", f"type=tcp,port={ctrl_port},bindaddr=127.0.0.1",
                   "--log", f"file={out / 'swtpm-read.log'}"],
                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(SWTPM_START_TRIES):
        if swtpm.poll() is not None:
            fail(f"swtpm (read) exited with {swtpm.returncode}")
        try:
            socket.create_connection(("127.0.0.1", ctrl_port), timeout=1.0).close()
            break
        except OSError:
            time.sleep(0.05)
    else:
        fail(f"swtpm (read) did not listen on {ctrl_port} within {SWTPM_START_TRIES * 0.05:.1f}s")
    try:
        run("swtpm_ioctl", "--tcp", f"127.0.0.1:{ctrl_port}", "-i")
        text = run("tpm2_pcrread", "-T", f"swtpm:host=127.0.0.1,port={port}", "sha256:0,1,2,3,4,5,6,7,8,9")
    finally:
        subprocess.run(["swtpm_ioctl", "--tcp", f"127.0.0.1:{ctrl_port}", "-s"], capture_output=True)
        try:
            swtpm.wait(timeout=SWTPM_SHUTDOWN_WAIT_S)
        except subprocess.TimeoutExpired:
            pass
    (out / "pcrs.txt").write_text(text)
    pcrs = {int(i): bytes.fromhex(v) for i, v in re.findall(r"^\s*(\d+)\s*:\s*0x([0-9A-Fa-f]{64})", text, re.M)}
    if pcrs.get(4) == bytes(32):
        # The firmware measures the boot application into PCR 4 before starting it: its TPM had
        # stopped answering before the bootloader ran, which the caller retries once like a stall.
        record_result("stall")
        print(f"STALL {name}: the firmware's TPM failed before the bootloader started (PCR 4 is zero)")
        sys.exit(3)

    if log is None:
        fail("no TPM_EVENT_LOG_ADDR and TPM_EVENT_LOG_SIZE in the guest's memory: the bootloader handed over no event log")
    (out / "eventlog.bin").write_bytes(log)
    eventlog = subprocess.run(["tpm2_eventlog", str(out / "eventlog.bin")], capture_output=True, text=True).stdout
    (out / "eventlog.yaml").write_text(eventlog)
    # tpm2_eventlog's own replay, at the end of its output, as a second reading of the log.
    bank = re.search(r"^  sha256:\n((?:    \d+\s*: 0x[0-9a-f]+\n)+)", eventlog.split("\npcrs:\n")[-1], re.M)
    tool_replay = {int(i): bytes.fromhex(v) for i, v in re.findall(r"(\d+)\s*: 0x([0-9a-f]{64})", bank[1])} if bank else {}

    # The log: a SHA-1-format header event whose data is the Spec ID event, then crypto-agile events.
    header_size = struct.unpack_from("<I", log, 28)[0]
    spec_event = log[32:32 + header_size]
    if not spec_event.startswith(b"Spec ID Event03\0"):
        fail("the event log does not start with a Spec ID event")
    count = struct.unpack_from("<I", spec_event, 24)[0]
    sizes = dict(struct.unpack_from("<HH", spec_event, 28 + 4 * i) for i in range(count))
    events, at = [], 32 + header_size
    while at < len(log):
        pcr, kind, count = struct.unpack_from("<III", log, at)
        at += 12
        digests = {}
        for _ in range(count):
            alg = struct.unpack_from("<H", log, at)[0]
            digests[alg] = log[at + 2:at + 2 + sizes[alg]]
            at += 2 + sizes[alg]
        size = struct.unpack_from("<I", log, at)[0]
        events.append((pcr, kind, digests.get(0x000B), log[at + 4:at + 4 + size]))
        at += 4 + size

    replay = {i: bytes(32) for i in range(24)}
    for pcr, kind, digest, _ in events:
        if kind != EV_NO_ACTION:
            replay[pcr] = sha256(replay[pcr] + digest)
    problems = [f"PCR {i}: the TPM holds {pcrs[i].hex()}, the log replays to {replay[i].hex()}"
                for i in (7, 8, 9) if pcrs.get(i) != replay[i]]
    problems += [f"PCR {i}: tpm2_eventlog replays to {tool_replay.get(i, b'').hex()}"
                 for i in (7, 8, 9) if tool_replay.get(i) != replay[i]]

    # PCR 9 computed from the image's files, in the order the bootloader measures them.
    names = [n for n in ("manifest", "kernel", "initfs") if (Path(files) / n).exists()]
    expected = bytes(32)
    for n in names:
        expected = sha256(expected + sha256((Path(files) / n).read_bytes()))
    expected = sha256(expected + SEPARATOR)
    if pcrs.get(9) != expected:
        problems.append(f"PCR 9 is {pcrs.get(9, b'').hex()}, the image's {' '.join(names)} give {expected.hex()}")
    pcr9 = [(kind, data) for pcr, kind, _, data in events if pcr == 9]
    if pcr9 != [(EV_IPL, f"usr/lib/boot/{n}".encode()) for n in names] + [(EV_SEPARATOR, bytes(4))]:
        problems.append(f"PCR 9's events are {pcr9}")

    pcr8 = [(kind, digest, data) for pcr, kind, digest, data in events if pcr == 8]
    if [kind for kind, _, _ in pcr8] != [EV_IPL, EV_IPL, EV_SEPARATOR] or pcr8[0][2] != b"normal" \
            or any(digest != sha256(data) for _, digest, data in pcr8):
        problems.append(f"PCR 8's events are {[(k, d.hex(), v) for k, d, v in pcr8]}")
    else:
        (out / "environment.txt").write_bytes(pcr8[1][2])

    key_events = [data for pcr, kind, _, data in events if pcr == 7 and kind == EV_EFI_VARIABLE_AUTHORITY
                  and data.startswith(FERRION_GUID)]
    name16 = "FerrionBootKey".encode("utf-16-le")
    want = FERRION_GUID + struct.pack("<QQ", 14, 32) + name16 + bytes.fromhex(slot)
    if key_events != [want]:
        problems.append(f"PCR 7 holds {len(key_events)} FerrionOS key events, not the one for slot {slot}")

    (out / "pcr9.hex").write_text(pcrs.get(9, b"").hex() + "\n")
    (out / "pcr8.hex").write_text(pcrs.get(8, b"").hex() + "\n")
    if name == "tpm-menu" and len(pcr8) == 3 and b"QUIET=1" in pcr8[1][2]:
        problems.append("the menu boot's environment still holds QUIET=1")
    if problems:
        fail("; ".join(problems))
    record_result("pass")
    print(f"PASS {name}: login at {login:.1f}s; PCR 9 {pcrs[9].hex()} = {' + '.join(names)} + separator; "
          f"PCRs 7, 8, 9 replay from the bootloader's event log ({len(events)} events)")
finally:
    if not (out / "result").exists():
        record_result("fail")
    cleanup_children()
EOF
}

CASE_MAX_ATTEMPTS=2
CASE_NAMES=()
rm -f "$OUT"/compare-*.result

read_result() {
    local path="$1" value
    [ -f "$path" ] || { echo missing; return; }
    value="$(tr -d '[:space:]' < "$path")"
    case "$value" in
        pass|fail|stall|blocked|not-comparable) echo "$value" ;;
        *) boot_die "assertion failed: invalid result '$value' in $path" ;;
    esac
}

run_case() {
    local name="$1" attempt status outcome
    CASE_NAMES+=("$name")
    rm -f "$OUT/$name"/attempt-*.result "$OUT/$name/result" "$OUT/$name/pcr8.hex" "$OUT/$name/pcr9.hex"
    for ((attempt = 1; attempt <= CASE_MAX_ATTEMPTS; attempt++)); do
        rm -f "$OUT/$name/pcr8.hex" "$OUT/$name/pcr9.hex" "$OUT/$name/result"
        set +e
        boot_case "$@" | tee -a "$OUT/summary.txt"
        status="${PIPESTATUS[0]}"
        set -e
        if [ ! -f "$OUT/$name/result" ]; then
            if [ "$status" = 3 ]; then
                printf '%s\n' stall > "$OUT/$name/result"
            else
                printf '%s\n' fail > "$OUT/$name/result"
            fi
        fi
        outcome="$(read_result "$OUT/$name/result")"
        printf '%s\n' "$outcome" > "$OUT/$name/attempt-$attempt.result"
        [ "$outcome" = stall ] || break
        mv "$OUT/$name/serial.log" "$OUT/$name/serial.stall$attempt.log"
    done
    if [ "$(read_result "$OUT/$name/result")" = stall ]; then
        printf '%s\n' blocked > "$OUT/$name/result"
        echo "BLOCKED $name: two firmware stalls in a row, so the case proved nothing" | tee -a "$OUT/summary.txt"
    fi
}

compare() {
    local pcr="$1" a="$2" b="$3" expect="$4" ra rb result words out_file
    out_file="$OUT/compare-pcr${pcr}-${a}-${b}.result"
    ra="$(read_result "$OUT/$a/result")"
    rb="$(read_result "$OUT/$b/result")"
    if [ "$ra" != pass ] || [ "$rb" != pass ]; then
        echo "NOT COMPARABLE $b: PCR $pcr vs $a ($a=$ra, $b=$rb)" | tee -a "$OUT/summary.txt"
        printf '%s\n' not-comparable > "$out_file"
        return 0
    fi
    [ -f "$OUT/$a/pcr$pcr.hex" ] && [ -f "$OUT/$b/pcr$pcr.hex" ] \
        || boot_die "assertion failed: passed case missing pcr$pcr.hex ($a, $b)"
    result=differs
    cmp -s "$OUT/$a/pcr$pcr.hex" "$OUT/$b/pcr$pcr.hex" && result=same
    words="differs from"
    [ "$result" = same ] && words="is the same as"
    if [ "$result" = "$expect" ]; then
        echo "PASS $b: PCR $pcr $words $a's" | tee -a "$OUT/summary.txt"
        printf '%s\n' pass > "$out_file"
    else
        echo "FAIL $b: PCR $pcr $words $a's" | tee -a "$OUT/summary.txt"
        printf '%s\n' fail > "$out_file"
    fi
}

: > "$OUT/summary.txt"
for name in tpm tpm-initfs; do
    prepare "$name"
    run_case "$name" "$WORK/$name.img" "$WORK/$name.files"
done
run_case tpm-menu "$WORK/tpm.img" "$WORK/tpm.files"
compare 9 tpm tpm-initfs differs
compare 8 tpm tpm-initfs same
compare 8 tpm tpm-menu differs
compare 9 tpm tpm-menu same
run_case no-tpm "$WORK/harddrive.img" ""

BOOTS=0
STALLS=0
FAILED=0
BLOCKED=0
for name in "${CASE_NAMES[@]}"; do
    for attempt_file in "$OUT/$name"/attempt-*.result; do
        [ -f "$attempt_file" ] || continue
        BOOTS=$((BOOTS + 1))
        [ "$(read_result "$attempt_file")" = stall ] && STALLS=$((STALLS + 1))
    done
    case "$(read_result "$OUT/$name/result")" in
        pass) ;;
        blocked) BLOCKED=1 ;;
        fail) FAILED=1 ;;
        *) boot_die "assertion failed: unexpected final result for $name" ;;
    esac
done
for cmp_file in "$OUT"/compare-*.result; do
    [ -f "$cmp_file" ] || continue
    [ "$(read_result "$cmp_file")" = fail ] && FAILED=1
done
{
    echo "firmware stalls (bootloader never started, or the TPM was gone by then): $STALLS of $BOOTS boots"
    echo "image: $IMAGE ($([ "$VERIFIED" = 1 ] && echo "verified boot, key $SLOT" || echo "no verified-boot key"))"
    echo "firmware: $OVMF_CODE; TPM: $(swtpm --version | head -n1)"
} >> "$OUT/summary.txt"
echo "results in $OUT: summary.txt, <case>/serial.log, <case>/screen.png, <case>/pcrs.txt, <case>/eventlog.bin"
if [ "$FAILED" = 1 ]; then
    exit 1
elif [ "$BLOCKED" = 1 ]; then
    exit 3
fi
