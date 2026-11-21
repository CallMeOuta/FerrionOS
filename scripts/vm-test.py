#!/usr/bin/env python3
# Boots a FerrionOS image in QEMU through `make qemu` and checks it the way a
# person would: it watches the screen, reads the serial console and types.
#   boot    quiet boot: nothing but black, the splash and the graphical login
#           appear on screen, and the login arrives within the timeout
#   shells  logs in as `user` on the serial console and runs each shell the
#           x86_64 desktop offers
# Every run boots with -snapshot, so the image is never changed, and `make` is
# told the image is up to date, so a run never starts a build. Screenshots
# (PNG) and the serial log of each run go under --out. Run from the
# repository root. Python standard library only.

import argparse
import os
import re
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
# The average color of the graphical login screen, measured on the desktop image.
LOGIN_COLOR = (43, 95, 135)
BRIGHT = 97  # a green value at or above this counts as a lit pixel


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

    def mean(self):
        n = self.width * self.height
        return tuple(sum(self.rgb[c::3]) / n for c in range(3))

    def lit_box(self):
        """Count and bounding box (x0, y0, x1, y1) of the lit pixels."""
        to_flag = bytes(0 if v < BRIGHT else 1 for v in range(256))
        count, rows, x0, x1 = 0, [], self.width, -1
        for y in range(self.height):
            row = self.green[y * self.width:(y + 1) * self.width].translate(to_flag)
            n = row.count(1)
            if n:
                count += n
                rows.append(y)
                x0, x1 = min(x0, row.find(1)), max(x1, row.rfind(1))
        return count, (x0, rows[0], x1, rows[-1]) if rows else None

    def has_login_blue(self):
        """True when some row is as blue as the login background, as in a half-painted login."""
        stride = self.width * 3
        for y in range(0, self.height, 8):
            row = self.rgb[y * stride:(y + 1) * stride]
            if sum(row[2::3]) - sum(row[0::3]) > 60 * self.width:
                return True
        return False

    def kind(self):
        """black, pointer, splash, drawing, login, placeholder (QEMU's own message) or text."""
        if all(abs(m - c) <= 10 for m, c in zip(self.mean(), LOGIN_COLOR)):
            return "login"
        count, box = self.lit_box()
        if not count:
            return "black"
        x0, y0, x1, y1 = box
        if x1 - x0 < 32 and y1 - y0 < 32:
            return "pointer"  # the window server has started and drawn only its mouse pointer
        if (self.width, self.height) == (640, 480) and 0.44 * self.height <= y0 and y1 <= 0.52 * self.height:
            return "placeholder"  # "Guest has not initialized the display (yet)."
        # The splash draws only in the middle third of the screen; anything lit
        # outside it is text or an unexpected screen.
        if self.width / 3 <= x0 and x1 < self.width * 2 / 3 and self.height / 3 <= y0 and y1 < self.height * 2 / 3:
            return "splash"
        # A screenshot taken while the login is being painted: the text
        # console draws gray on black and never this blue.
        if self.has_login_blue():
            return "drawing"
        return "text"

    def save_png(self, path):
        stride = self.width * 3
        raw = b"".join(b"\x00" + self.rgb[y * stride:(y + 1) * stride] for y in range(self.height))

        def chunk(kind, body):
            return (len(body).to_bytes(4, "big") + kind + body
                    + zlib.crc32(kind + body).to_bytes(4, "big"))

        header = self.width.to_bytes(4, "big") + self.height.to_bytes(4, "big") + b"\x08\x02\x00\x00\x00"
        Path(path).write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
                               + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


class VM:
    """One QEMU run started by `make qemu`: serial on stdio, monitor on a socket."""

    def __init__(self, args, out):
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
        env = dict(os.environ, QEMUFLAGS=f"-display none -snapshot -monitor unix:{self.monitor_path},server=on,wait=off")
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
        self.monitor = self._connect_monitor()

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

    def close(self):
        try:
            self.monitor.sendall(b"quit\n")
            self.proc.wait(timeout=20)
        except (OSError, subprocess.TimeoutExpired):
            os.killpg(self.proc.pid, signal.SIGTERM)
            self.proc.wait(timeout=20)
        self.monitor.close()
        self.log.close()
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


TESTS = {"boot": test_boot, "shells": test_shells}


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
            vm = VM(args, out)
            try:
                ok, note = TESTS[name](vm, args)
            finally:
                vm.close()
            failures += not ok
            print(f"{'PASS' if ok else 'FAIL'} {name} {run}/{args.repeat}: {note}", flush=True)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
