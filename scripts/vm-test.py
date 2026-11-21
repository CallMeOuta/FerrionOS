#!/usr/bin/env python3
# Boots a FerrionOS image in QEMU through `make qemu` and checks it the way a
# person would: it watches the screen, reads the serial console and types.
#   boot    quiet boot: nothing but black, the splash and the graphical login
#           appear on screen, and the login arrives within the timeout
#   shells  logs in as `user` on the serial console and runs each shell the
#           x86_64 desktop offers
#   tpm     boots with a software TPM (swtpm) on QEMU's tpm-crb device, logs
#           in as root on the serial console, and checks that tpm-pcrs prints
#           the PCRs 7, 8 and 9 the host then reads from swtpm, and that
#           attest-statement answers with a verdict that carries none of them
#   lockdown  checks that every driver still works (the graphical login, a
#           keypress on it, a DHCP address, a file written to disk, the
#           drivers' processes), then logs in as root on the serial console
#           and checks that root reaches no raw hardware: no device scheme in
#           /scheme, no physical memory, and lockdown-probe passes (needs an
#           image whose config includes config/lockdown.toml)
# Every run boots with -snapshot, so the image is never changed, and `make` is
#   scheme-create  logs in as `user` on the serial console and checks that its
#           login may not open the scheme-creation capability, then as root:
#           root's login may, a program root starts under ferrion-run may not
#           (x86_64 desktop)
#   nsmgr-bounds  logs in as `user` on the serial console and runs
#           nsmgr-bounds-probe: fork requests whose name length runs past their
#           end get EINVAL and the namespace manager keeps answering (x86_64 desktop)
# told the image is up to date, so a run never starts a build. Screenshots
# (PNG) and the serial log of each run go under --out. Run from the
# repository root. Python standard library only.

import argparse
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ANSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\r")
BRIGHT = 97  # a green value at or above this counts as a lit pixel
LIT = bytes(0 if v < BRIGHT else 1 for v in range(256))  # bytes.translate table: 1 for a lit green value
# The login screen and the wallpaper are drawn on FerrionOS's ground color (`ground` in
# branding/color.tokens.json), which Orbital draws up to one step darker in images; the
# text console draws on black.
GROUND = (0x0F, 0x0E, 0x0C)
# orblogin's box, centered on the screen at every resolution: its fields, labels and
# buttons are lit, while the splash and the login background leave this rectangle dark.
LOGIN_BOX = (216, 164)


# scripts/secure-boot-test.sh loads this file and uses Frame (from_ppm, kind, save_png).
class Frame:
    def __init__(self, width, height, rgb):
        self.width, self.height, self.rgb = width, height, rgb
        self.green = rgb[1::3]

    @classmethod
    def from_ppm(cls, data):
        parts = data.split(maxsplit=4)
        if parts[0] != b"P6" or parts[3] != b"255":
            raise ValueError("not an 8-bit P6 image")
        width, height = int(parts[1]), int(parts[2])
        rgb = parts[4]
        if len(rgb) != width * height * 3:
            raise ValueError("truncated image")
        return cls(width, height, rgb)

    def lit_box(self):
        """Count and bounding box (x0, y0, x1, y1) of the lit pixels."""
        count, rows, x0, x1 = 0, [], self.width, -1
        for y in range(self.height):
            row = self.green[y * self.width:(y + 1) * self.width].translate(LIT)
            n = row.count(1)
            if n:
                count += n
                rows.append(y)
                x0, x1 = min(x0, row.find(1)), max(x1, row.rfind(1))
        return count, (x0, rows[0], x1, rows[-1]) if rows else None

    def lit_in(self, x0, y0, x1, y1):
        """Number of lit pixels from (x0, y0) up to, not including, (x1, y1)."""
        return sum(self.green[y * self.width + x0:y * self.width + x1].translate(LIT).count(1)
                   for y in range(y0, y1))

    def has_ground_row(self):
        """True when some row is mostly the ground color, as on the login screen, even half-painted."""
        stride = self.width * 3
        for y in range(0, self.height, 8):
            row = self.rgb[y * stride:(y + 1) * stride]
            # every fourth pixel of the row
            near = sum(all(abs(row[x + c] - GROUND[c]) <= 2 for c in range(3)) for x in range(0, stride, 12))
            if near > self.width // 8:
                return True
        return False

    def kind(self):
        """black, pointer, firmware, splash, drawing, login, placeholder (QEMU's own message) or text."""
        count, box = self.lit_box()
        if not count:
            return "black"
        x0, y0, x1, y1 = box
        if x1 - x0 < 32 and y1 - y0 < 32:
            return "pointer"  # the window server has started and drawn only its mouse pointer
        if (self.width, self.height) == (640, 480) and 0.44 * self.height <= y0 and y1 <= 0.52 * self.height:
            return "placeholder"  # "Guest has not initialized the display (yet)."
        width, height = LOGIN_BOX
        left, top = (self.width - width) // 2, (self.height - height) // 2
        in_box = self.lit_in(left, top, left + width, top + height)
        ground = self.has_ground_row()
        middle = self.width / 3 <= x0 and x1 < self.width * 2 / 3
        # The firmware's logo, alone in the middle of a black screen before the bootloader runs.
        if not ground and middle and self.height / 3 <= y0 and y1 < self.height * 2 / 3:
            return "firmware"
        # The splash draws the mark and its progress bar in the middle third of
        # the screen's width and its upper two thirds, never in orblogin's box;
        # the login background drawn before its box looks the same and counts as splash.
        if not in_box and middle and y1 < self.height * 2 / 3:
            return "splash"
        # The text console draws gray on black and never on the ground color.
        if not ground:
            return "text"
        return "login" if in_box else "drawing"

    def save_png(self, path):
        stride = self.width * 3
        raw = b"".join(b"\x00" + self.rgb[y * stride:(y + 1) * stride] for y in range(self.height))

        def chunk(kind, body):
            return (len(body).to_bytes(4, "big") + kind + body
                    + zlib.crc32(kind + body).to_bytes(4, "big"))

        header = self.width.to_bytes(4, "big") + self.height.to_bytes(4, "big") + b"\x08\x02\x00\x00\x00"
        Path(path).write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
                               + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


class Swtpm:
    """A software TPM 2.0 that QEMU reaches as a tpm-crb device, for one boot."""

    def __init__(self):
        self.dir = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-tpm-"))
        self.ctrl = self.dir / "ctrl.sock"
        self.proc = self._start("--ctrl", f"type=unixio,path={self.ctrl}")
        self.qemu_flags = (f"-chardev socket,id=chrtpm,path={self.ctrl} -tpmdev emulator,id=tpm0,chardev=chrtpm "
                           "-device tpm-crb,tpmdev=tpm0")

    def _start(self, *channels):
        proc = subprocess.Popen(["swtpm", "socket", "--tpm2", "--tpmstate", f"dir={self.dir}", *channels],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        socket_path = Path(channels[-1].split("path=")[1])
        deadline = time.monotonic() + 10
        while not socket_path.exists():
            if time.monotonic() > deadline or proc.poll() is not None:
                raise RuntimeError("swtpm did not start")
            time.sleep(0.05)
        return proc

    def read_pcrs(self, pcrs):
        """The SHA-256 PCRs as lowercase hex, read on the host once QEMU was killed.

        A QEMU that quits cleanly shuts swtpm down and its PCRs are lost; after a kill,
        swtpm stores its volatile state and a second swtpm resumes from it on a data
        socket that tpm2_pcrread can reach."""
        subprocess.run(["swtpm_ioctl", "--unix", str(self.ctrl), "-v"], check=True, capture_output=True)
        subprocess.run(["swtpm_ioctl", "--unix", str(self.ctrl), "-s"], check=True, capture_output=True)
        self.proc.wait(timeout=20)
        data = self.dir / "data.sock"
        self.proc = self._start("--flags", "not-need-init", "--server", f"type=unixio,path={data}",
                                "--ctrl", f"type=unixio,path={data}.ctrl")
        try:
            text = subprocess.run(["tpm2_pcrread", "-T", f"swtpm:path={data}",
                                   "sha256:" + ",".join(map(str, pcrs))],
                                  check=True, capture_output=True, text=True).stdout
        finally:
            subprocess.run(["swtpm_ioctl", "--unix", f"{data}.ctrl", "-s"], capture_output=True)
        return {int(i): v.lower() for i, v in re.findall(r"(\d+)\s*:\s*0x([0-9A-Fa-f]{64})", text)}

    def close(self):
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait(timeout=20)
        shutil.rmtree(self.dir)


class VM:
    """One QEMU run started by `make qemu`: serial on stdio, monitor on a socket."""

    def __init__(self, args, out, tpm=False):
        image = ROOT / "build" / args.arch / args.config / "harddrive.img"
        if not image.exists():
            sys.exit(f"vm-test: {image.relative_to(ROOT)} is missing; build it first: "
                     f"make image ARCH={args.arch} CONFIG_NAME={args.config}")
        # `make qemu` unmounts a mounted image before booting it; someone else's
        # `make mount` must never be pulled out from under them.
        mount_dir = image.parent / "filesystem"
        if mount_dir.exists():
            sys.exit(f"vm-test: {mount_dir.relative_to(ROOT)} exists, so the image is mounted or was left "
                     f"mounted; run `make unmount ARCH={args.arch} CONFIG_NAME={args.config}` when it is free")
        self.out = out
        self.scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-"))
        self.monitor_path = self.scratch / "monitor.sock"
        self.tpm = Swtpm() if tpm else None
        flags = f"-display none -snapshot -monitor unix:{self.monitor_path},server=on,wait=off"
        if self.tpm:
            flags += " " + self.tpm.qemu_flags
        env = dict(os.environ, QEMUFLAGS=flags)
        # `-o` takes each target exactly as make spells it, relative to the
        # repository root (an absolute path would not match): neither the image
        # nor the package repository behind it is ever rebuilt by a test.
        build = image.parent.relative_to(ROOT)
        command = ["make", "-s", "-o", str(build / "harddrive.img"), "-o", str(build / "repo.tag"), "qemu",
                   f"ARCH={args.arch}", f"CONFIG_NAME={args.config}", *args.make]
        self.start = time.monotonic()
        self.proc = subprocess.Popen(command, cwd=ROOT, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.STDOUT, start_new_session=True)
        self.serial = bytearray()
        self.lock = threading.Lock()
        self.typing = threading.Lock()
        self.log = open(out / "serial.log", "wb")
        threading.Thread(target=self._read_serial, daemon=True).start()
        try:
            self.monitor = self._connect_monitor()
        except RuntimeError:
            if self.tpm:
                self.tpm.close()
            raise

    def elapsed(self):
        return time.monotonic() - self.start

    def _read_serial(self):
        while chunk := os.read(self.proc.stdout.fileno(), 65536):
            with self.lock:
                start = max(0, len(self.serial) - 3)
                self.serial += chunk
                # Line editors such as Nushell's ask the terminal where the cursor
                # is and wait for the answer; reply as a terminal would.
                queries = self.serial[start:].count(b"\x1b[6n")
            if queries:
                self.type("\x1b[1;1R" * queries)
            self.log.write(chunk)
            self.log.flush()

    def _connect_monitor(self):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"make qemu exited with {self.proc.returncode}; see {self.out / 'serial.log'}")
            try:
                sock = socket.socket(socket.AF_UNIX)
                sock.connect(str(self.monitor_path))
                self.monitor = sock
                self._monitor_reply()
                return sock
            except (FileNotFoundError, ConnectionRefusedError):
                sock.close()
                time.sleep(0.05)
        raise RuntimeError("QEMU's monitor socket never appeared")

    def _monitor_reply(self):
        reply = b""
        while not reply.endswith(b"(qemu) "):
            data = self.monitor.recv(65536)
            if not data:
                raise RuntimeError("QEMU closed its monitor")
            reply += data
        return reply.decode(errors="replace")

    def command(self, text):
        """Runs a QEMU monitor command and returns its output."""
        self.monitor.sendall(text.encode() + b"\n")
        return self._monitor_reply()

    def screenshot(self):
        path = self.scratch / "frame.ppm"
        self.command(f"screendump {path}")
        return Frame.from_ppm(path.read_bytes())

    def text(self, since=0):
        with self.lock:
            return ANSI.sub(b"", bytes(self.serial[since:])).decode(errors="replace")

    def wait_serial(self, pattern, timeout, since=0):
        """Waits for a regex on the serial console; returns the match or None."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            match = re.search(pattern, self.text(since))
            if match:
                return match
            if self.proc.poll() is not None:
                return None
            time.sleep(0.1)
        return None

    def type(self, text):
        """Types on the serial console."""
        with self.typing:
            self.proc.stdin.write(text.encode())
            self.proc.stdin.flush()

    def mark(self):
        with self.lock:
            return len(self.serial)

    def kill(self):
        """Stops QEMU at once, as pulling the plug would."""
        os.killpg(self.proc.pid, signal.SIGKILL)
        self.proc.wait(timeout=20)

    def close(self):
        if self.proc.poll() is None:
            try:
                self.monitor.sendall(b"quit\n")
                self.proc.wait(timeout=20)
            except (OSError, subprocess.TimeoutExpired):
                os.killpg(self.proc.pid, signal.SIGTERM)
                self.proc.wait(timeout=20)
        self.monitor.close()
        self.log.close()
        if self.tpm:
            self.tpm.close()
        for path in self.scratch.iterdir():
            path.unlink()
        self.scratch.rmdir()


def test_boot(vm, args):
    """No text from power-on to the graphical login, and the login arrives in time."""
    seen = []
    while vm.elapsed() < args.timeout:
        frame = vm.screenshot()
        kind = frame.kind()
        if not seen or seen[-1][1] != kind:
            seen.append((round(vm.elapsed(), 1), kind))
            if kind in ("login", "text"):
                frame.save_png(vm.out / f"{kind}.png")
        if kind == "login":
            break
        time.sleep(0.2)
    kinds = [kind for _, kind in seen]
    if "login" not in kinds:
        vm.screenshot().save_png(vm.out / "timeout.png")
        return False, f"no login within {args.timeout}s; screens {seen}"
    if "text" in kinds:
        return False, f"text on screen before the login ({vm.out / 'text.png'}); screens {seen}"
    return True, f"login at {seen[-1][0]}s; screens {seen}"


# Each line is typed into the user's shell; the pattern must match the output,
# never the echo of the typed line.
SHELL_CHECKS = [
    ("version | get version", r"\n0\.\d+\.\d+"),  # only Nushell runs this line
    ("bash -c 'echo bash-$((40+2)) $BASH_VERSION'", r"bash-42 5\."),
    # fish prints its version even when it cannot find its scripts; the data
    # directory shows whether it found them.
    ("fish -c 'echo fish-(math 40+2) $version $__fish_data_dir'", r"fish-42 4\.\S+ /usr/share/fish\n"),
    ("zsh -c 'echo zsh-$((40+2)) $ZSH_VERSION'", r"zsh-42 5\."),
    ("ion -c 'echo ion-$((40+2))'", r"ion-42"),
]


def test_shells(vm, args):
    """Logs in as `user` on the serial console and runs every offered shell."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    failed = []
    for line, pattern in SHELL_CHECKS:
        since = vm.mark()
        vm.type(line + "\n")
        if not vm.wait_serial(pattern, 20, since):
            failed.append(line)
    if failed:
        return False, f"no expected output from: {failed}; see {vm.out / 'serial.log'}"
    return True, f"{len(SHELL_CHECKS)} shell checks passed"


def test_nsmgr_bounds(vm, args):
    """Logs in as `user` on the serial console and runs nsmgr-bounds-probe: fork requests whose name length runs past their end get EINVAL, a well-formed fork still works, and the shell can still run a program afterwards."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    results = {}
    for tag, line in (("probe", "nsmgr-bounds-probe"), ("after", "ls / >/dev/null")):
        since = vm.mark()
        # The status is echoed inside bash: `user`'s login shell is not POSIX.
        vm.type(f"bash -c '{line}; echo nb-end-{tag}-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)nb-end-{tag}-(\d+)", 60, since)
        results[tag] = (int(found.group(2)) if found else None, found.group(1) if found else vm.text(since))
    (vm.out / "nsmgr-bounds.txt").write_text("".join(f"[{tag}] exit {status}\n{output}\n"
                                                     for tag, (status, output) in results.items()))
    lines = re.findall(r"nsmgr-bounds-probe: (?:pass|FAIL) .*", results["probe"][1])
    detail = "; ".join(line.strip() for line in lines) or "no result line"
    if results["probe"][0] != 0:
        return False, f"probe exit {results['probe'][0]}: {detail}; see {vm.out / 'nsmgr-bounds.txt'}"
    if results["after"][0] != 0:
        return False, f"the shell could not run `ls /` afterwards (exit {results['after'][0]}); see {vm.out}"
    return True, detail


# The nonce the tpm test asks attestd about, in hexadecimal, and the PCRs it compares.
TPM_NONCE = "f3" * 32
TPM_PCRS = (7, 8, 9)


def test_tpm(vm, args):
    """Root reads PCRs 7, 8 and 9 through attestd and they equal swtpm's; a statement carries none of them."""
    # OVMF can lose swtpm during the firmware phase when QEMU's monitor sits idle, and then
    # measures nothing more (PCR 4 stays zero); a monitor command every 0.3 s avoids it.
    deadline = time.monotonic() + args.timeout
    while not vm.wait_serial(r"login: ", 0.3):
        if time.monotonic() > deadline:
            return False, f"no serial login prompt within {args.timeout}s"
        vm.command("info status")
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20):
        return False, "no password prompt for root"
    vm.type("password\n")
    time.sleep(3)

    since = vm.mark()
    vm.type("tpm-pcrs\n")
    if not vm.wait_serial(r"PCR 9 sha256 [0-9a-f]{64}", 30, since):
        return False, f"tpm-pcrs printed no PCRs; see {vm.out / 'serial.log'}"
    guest = {int(i): v for i, v in re.findall(r"PCR (\d+) sha256 ([0-9a-f]{64})", vm.text(since))}

    since = vm.mark()
    vm.type(f"attest-statement {TPM_NONCE}\n")
    found = vm.wait_serial(r"FerrionOS attestation statement.*\n(?:[a-z-]+: .*\n)*?verdict: .*\n", 60, since)
    if not found:
        return False, f"attest-statement printed no statement; see {vm.out / 'serial.log'}"
    statement = found.group(0)

    # tpmd registers no scheme name, so nothing but attestd's handle reaches it.
    since = vm.mark()
    vm.type("ls /scheme; echo scheme-list-$((40+2))\n")
    listing = vm.wait_serial(r"(?s)(.*)scheme-list-42", 20, since)
    schemes = set(re.findall(r"[\w.-]+", listing.group(1))) if listing else set()

    vm.kill()
    # PCR 4 holds the firmware's measurement of the boot application: zero means the firmware
    # lost the TPM during boot, and the run proves nothing.
    host = vm.tpm.read_pcrs((4, *TPM_PCRS))
    healthy = host.get(4, "0" * 64) != "0" * 64
    host = {i: host.get(i) for i in TPM_PCRS}
    (vm.out / "statement.txt").write_text(statement)
    (vm.out / "pcrs.txt").write_text("".join(f"PCR {i} guest {guest.get(i)} host {host.get(i)}\n" for i in TPM_PCRS))

    problems = [] if healthy else ["PCR 4 is zero: the firmware lost the TPM during boot; run again"]
    if sorted(guest) != list(TPM_PCRS) or guest != host:
        problems.append(f"the PCRs read inside differ from swtpm's (see {vm.out / 'pcrs.txt'})")
    for line in (f"nonce: {TPM_NONCE}", "tpm: present", "quote: matches the PCRs read"):
        if line not in statement:
            problems.append(f"the statement lacks {line!r}")
    if any(value in statement for value in guest.values()):
        problems.append("the statement carries a PCR value")
    if not listing:
        problems.append("`ls /scheme` printed nothing")
    elif "attest" not in schemes or "tpm" in schemes:
        problems.append("`ls /scheme` should list attest and no scheme named tpm")
    if problems:
        return False, "; ".join(problems)
    verdict = re.search(r"verdict: (.*)", statement).group(1)
    return True, f"PCRs 7, 8 and 9 equal swtpm's; verdict: {verdict}; see {vm.out}"


def settle(vm, timeout, still=1.0):
    """Screenshots until the screen stops changing for `still` seconds; returns the last frame."""
    deadline = time.monotonic() + timeout
    frame, since = vm.screenshot(), time.monotonic()
    while time.monotonic() < deadline:
        time.sleep(0.25)
        new = vm.screenshot()
        if new.rgb != frame.rgb:
            frame, since = new, time.monotonic()
        elif time.monotonic() - since >= still:
            break
    return frame


# Lines the screenshots test types into the user's serial shell before opening
# programs: files for the programs to show.
SCREENSHOT_SETUP = [
    "bash -c 'printf \"%s\\n\" \"fn main() {\" \"    let name = \\\"FerrionOS\\\";\" "
    "\"    println!(\\\"Hello from {name}!\\\");\" \"}\" > /tmp/hello.rs'",
]

# Programs the screenshots test opens on the desktop, one at a time, from the
# serial console: (file name, command line without single quotes). The terminals
# run a command and stay open. Each window closes before the next opens.
SCREENSHOT_APPS = [
    # In COSMIC Terminal Nushell sees a terminal one column wide, and an
    # interactive Nushell clears the screen at its first prompt: the table gets
    # its width from the command line, and the terminal then waits instead.
    # An interactive fish there overflows its stack, so fish has no picture yet.
    ("terminal-nushell", 'cosmic-term -e bash -c "nu -c \\"ls /usr/share/ui | sort-by size | table --width 110\\"; sleep 3600"'),
    ("files", "cosmic-files /usr"),
    ("editor", "cosmic-edit /tmp/hello.rs"),
    ("viewer", "viewer /usr/share/ui/login.png"),
]


def test_screenshots(vm, args):
    """Captures the splash, the login, the desktop and each program in SCREENSHOT_APPS as PNG."""
    saved = []

    def save(name, frame):
        path = vm.out / f"{len(saved) + 1:02}-{name}.png"
        frame.save_png(path)
        saved.append(path.name)

    # The last frame of the splash shows its progress bar furthest along.
    kind, splash, splash_over = None, None, False
    while vm.elapsed() < args.timeout and kind != "login":
        frame = vm.screenshot()
        kind = frame.kind()
        if kind == "splash" and not splash_over:
            splash = frame
        elif splash:
            splash_over = True
        time.sleep(0.1)
    if kind != "login":
        return False, f"no login within {args.timeout}s"
    if splash:
        save("splash", splash)
    save("login", settle(vm, 10))

    # The login screen has `user` filled in and the password field focused;
    # the dev account's password is empty.
    vm.command("sendkey ret")
    time.sleep(2)
    save("desktop", settle(vm, 20, still=2.0))

    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s; saved {saved}"
    vm.type("user\n")
    time.sleep(3)
    for line in SCREENSHOT_SETUP:
        vm.type(line + "\n")
        time.sleep(1)
    # The user's shell is Nushell, which has no `&`; bash starts each program
    # in the background and keeps its process id for closing it.
    for name, command in SCREENSHOT_APPS:
        vm.type(f"bash -c '{command} >/dev/null 2>&1 & echo $! > /tmp/vm-test-app.pid'\n")
        save(name, settle(vm, 30, still=2.0))
        vm.type("bash -c 'kill $(cat /tmp/vm-test-app.pid)'\n")
        time.sleep(2)
    return True, f"saved {len(saved)} screenshots in {vm.out}: {saved}"


# Drivers the x86_64 desktop runs under `make qemu`'s defaults (q35, NVMe, e1000,
# xHCI, the firmware's framebuffer); each must keep running once the hardware
# right is limited to drivers.
LOCKDOWN_DRIVERS = ["pcid", "acpid", "nvmed", "e1000d", "xhcid", "vesad"]
# Schemes only a process holding the hardware right may name (security roadmap S13).
HARDWARE_SCHEMES = ["irq", "serio", "kernel.acpi", "pci", "acpi"]


def test_lockdown(vm, args):
    """Every driver works (login screen, keypress, DHCP, disk, driver processes) while root reaches no raw hardware."""
    kind = None
    while vm.elapsed() < args.timeout and kind != "login":
        kind = vm.screenshot().kind()
        time.sleep(0.2)
    if kind != "login":
        return False, f"no graphical login within {args.timeout}s"
    login = settle(vm, 10)
    login.save_png(vm.out / "login.png")
    # The login screen has `user` filled in and an empty password: Enter logs in.
    vm.command("sendkey ret")
    desktop = settle(vm, 30, still=2.0)
    desktop.save_png(vm.out / "desktop.png")
    problems = []
    if desktop.rgb == login.rgb:
        problems.append("Enter on the login screen changed nothing, so keyboard input is broken")

    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20):
        return False, "no password prompt for root"
    vm.type("password\n")
    time.sleep(3)

    def run(line, end, timeout=30):
        """Types `line` followed by an echo of `end`; returns the output before it, or None."""
        since = vm.mark()
        vm.type(f"{line}; echo {end}-$((40+2))\n")
        found = vm.wait_serial(rf"(?s)\n(.*){end}-42", timeout, since)
        return found.group(1) if found else None

    address = None
    for _ in range(10):
        output = run("cat /scheme/netcfg/ifaces/eth0/addr/list", "net") or ""
        address = re.search(r"\b10\.0\.2\.\d+", output)
        if address:
            break
        time.sleep(3)
    if not address:
        problems.append("eth0 has no address from QEMU's DHCP server")
    if "disk-42" not in (run("echo disk-$((40+2)) > /tmp/lockdown-disk; cat /tmp/lockdown-disk", "disk-done") or ""):
        problems.append("writing and reading back /tmp/lockdown-disk on the root file system failed")
    processes = run("ps", "ps") or ""
    (vm.out / "ps.txt").write_text(processes)
    missing = [driver for driver in LOCKDOWN_DRIVERS if driver not in processes]
    if missing:
        problems.append(f"drivers not running: {missing} (see {vm.out / 'ps.txt'})")

    listing = run("ls /scheme", "scheme-list")
    schemes = set(re.findall(r"[\w.-]+", listing or ""))
    if not listing or "memory" not in schemes:
        problems.append("`ls /scheme` printed no memory scheme")
    visible = [name for name in HARDWARE_SCHEMES if name in schemes]
    if visible:
        problems.append(f"root sees the device schemes {visible}")
    # bash opens the file for its redirection and prints the marker only if the open succeeded.
    opened = run("bash -c 'exec 3</scheme/memory/physical && echo physmem-opened-$((40+2))'", "physmem")
    if opened is None or "physmem-opened-42" in opened:
        problems.append("root opened /scheme/memory/physical")
    since = vm.mark()
    vm.type("lockdown-probe; echo probe-exit-$?\n")
    probe = vm.wait_serial(r"(?s)\n(lockdown-probe: .*)probe-exit-(\d+)", 60, since)
    if not probe:
        problems.append("lockdown-probe printed nothing; is config/lockdown.toml included?")
    else:
        (vm.out / "probe.txt").write_text(probe.group(1))
        if probe.group(2) != "0":
            failed = re.findall(r"lockdown-probe: FAIL (\S+):", probe.group(1))
            problems.append(f"lockdown-probe exited {probe.group(2)}; open routes: {failed}")

    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    return True, f"drivers work ({address.group(0)} from DHCP) and root reaches no raw hardware; see {vm.out}"


TESTS = {"boot": test_boot, "shells": test_shells, "screenshots": test_screenshots, "tpm": test_tpm,
         "lockdown": test_lockdown}

def test_scheme_create(vm, args):
    """Issue 7: `user`'s serial login may not open the scheme-creation capability; root's login, made next, may, and a program root starts under ferrion-run may not."""
    probe = "cat /scheme/namespace/scheme-creation-cap 2>&1 >/dev/null"
    results, transcript = {}, []
    def check(name, line):
        # The status is echoed inside bash: `user`'s login shell is not POSIX.
        since = vm.mark()
        vm.type(f"bash -c '{line}; echo sc-end-{name}-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)sc-end-{name}-(\d+)", 60, since)
        output = found.group(1) if found else vm.text(since)
        transcript.append(f"[{name}] $ {line}\n{output}exit {found.group(2) if found else 'none'}\n")
        results[name] = None if not found else "Permission denied" not in output

    # `user` logs in first: after root's Ion session had exited on the serial console, `user`'s
    # Nushell quit at once with "failed to take control of the terminal" (seen 2026-11-21).
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    check("user", probe)
    since = vm.mark()
    vm.type("exit\n")
    if not vm.wait_serial(r"login: ", args.timeout, since):
        return False, f"no login prompt after user's logout; see {vm.out}"
    # After a logout the serial log shows the login prompt drawn twice; type after the second.
    time.sleep(3)
    since = vm.mark()
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20, since):
        return False, f"no password prompt for root after user's logout; see {vm.out}"
    vm.type("password\n")
    if not vm.wait_serial(r"root:.*# ", 20, since):
        return False, f"no root shell after user's logout; see {vm.out}"
    check("root", probe)
    check("root-ferrion-run", "ferrion-run " + probe)
    (vm.out / "scheme-create.txt").write_text("".join(transcript))
    expected = {"user": False, "root": True, "root-ferrion-run": False}
    wrong = [f"{name} {'opened' if results[name] else 'was refused' if results[name] is False else 'gave no answer'}"
             for name in expected if results[name] != expected[name]]
    if wrong:
        return False, "; ".join(wrong) + f"; see {vm.out / 'scheme-create.txt'}"
    return True, "root's login opens the capability; under ferrion-run and as user it is refused"


TESTS["scheme-create"] = test_scheme_create
TESTS["nsmgr-bounds"] = test_nsmgr_bounds


def main():
    parser = argparse.ArgumentParser(
        description="Boot a FerrionOS image in QEMU and check it from the outside.",
        epilog="tests: " + "; ".join(f"{name}: {fn.__doc__}" for name, fn in TESTS.items()))
    parser.add_argument("tests", nargs="+", choices=TESTS, metavar="test")
    parser.add_argument("--arch", default="x86_64")
    parser.add_argument("--config", default="desktop")
    parser.add_argument("--repeat", type=int, default=1, help="boot this many times per test")
    parser.add_argument("--timeout", type=float, default=60, help="seconds a step may take")
    parser.add_argument("--out", type=Path, default=None, help="default: build/vm-test/<arch>/<config>")
    parser.add_argument("--make", action="append", default=[], metavar="VAR=VALUE",
                        help="extra make variable for `make qemu`, such as net=no; repeatable")
    if len(sys.argv) == 1:
        parser.print_help()
        sys.exit(2)
    args = parser.parse_args()
    out_root = args.out or ROOT / "build" / "vm-test" / args.arch / args.config
    failures = 0
    for name in args.tests:
        for run in range(1, args.repeat + 1):
            out = out_root / f"{name}-{run}"
            out.mkdir(parents=True, exist_ok=True)
            vm = VM(args, out, tpm=name == "tpm")
            try:
                ok, note = TESTS[name](vm, args)
            finally:
                vm.close()
            failures += not ok
            print(f"{'PASS' if ok else 'FAIL'} {name} {run}/{args.repeat}: {note}", flush=True)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
