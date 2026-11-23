#!/usr/bin/env python3
# Boots a FerrionOS image in QEMU through `make qemu` and checks it the way a
# person would: it watches the screen, reads the serial console and types.
#   boot    quiet boot: nothing but black, the splash and the graphical login
#           appear on screen, and the login arrives within the timeout
#   gpu-heads  logs in graphically on gpu=multi and checks both Orbital outputs
#           with screendump gpu0 0 and gpu0 1
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
#   driver-namespaces  presses Enter on the graphical login (input through
#           inputd), logs in as root on the serial console, reads
#           /scheme/sys/iostat and checks that every namespace handle each
#           driver holds can open only memory and pipe (ferrion-audiod: memory and
#           rand); lists the drivers not running at the snapshot as not exercised
#   rand-seed  checks random bytes and seed status across two boots each with
#           firmware RNG only, timing jitter only, and the host CPU RNG
#   ferrion-run  logs in as `user` on the serial console and checks that
#           `ferrion-run --no-network` and a profile keep a program off the
#           network, also through sudo, that a socket opened before the launch does not work in
#           the program, and that a socket or disk file as standard input or
#           output is refused; each denial is first tried without the launcher,
#           where it must succeed (x86_64 desktop)
#   disk-keys  on an image built with encrypt_disk (config disk-encryption-test):
#           a recovery key added on the host opens it at the bootloader, and after
#           `redoxfs-key change` in the guest and a reboot the old passphrase is
#           refused, the new one accepted and the old slot is in no header
#   disk-install  installs from that config's live image to a second disk with
#           the text installer, then starts the installed disk with its passphrase
#           and with the recovery key the installer printed
#   scheme-create  logs in as `user` on the serial console and checks that its
#           login may not open the scheme-creation capability, then as root:
#           root's login may, a program root starts under ferrion-run may not
#           (x86_64 desktop)
#   nsmgr-bounds  logs in as `user` on the serial console and runs
#           nsmgr-bounds-probe: fork requests whose name length runs past their
#           end get EINVAL and the namespace manager keeps answering (x86_64 desktop)
#   graphics-bounds  logs in as `user` on the serial console and runs
#           graphics-bounds-probe: dumb-buffer mappings past a buffer's end, sizes
#           beyond the display driver's limits and an unknown buffer number are
#           refused, and the driver still answers a fresh handle (x86_64 desktop)
#   scheme-short-read  logs in as root on the serial console and runs
#           scheme-short-read-probe: a scheme the probe serves answers its forked child's reads
#           with fewer bytes than asked, and every byte of the caller's buffer past
#           the reply keeps its contents, inside a page and across page boundaries
#           (root, since a `user` login may not create a scheme; x86_64 desktop)
#   event-poll  logs in as `user` on the serial console and runs event-poll-probe:
#           200 zero-timeout polls of empty pipes leave the kernel's event
#           subscriber count where it was (x86_64 desktop)
#   desktop-regions  compares the settled desktop with the flat first-run target's region probes
#   wayland-client  checks DD1 pixels, keyboard, frame median and independent cursor dispatch
#   pointer-latency checks the strict cursor maximum under 2 ms; use --repeat 10
#   cli-tools  checks sed, awk and gzip tar creation/extraction with -C on the desktop
#   sudo-notty  logs in as `user` on the serial console and runs sudo with no
#           terminal on any standard descriptor: no panic, and exit 0 because
#           `user`'s password is empty (x86_64 desktop)
#   resolver  checks hosts-file names as `user`, then captures DHCP option 6
#           and the actual DNS query destination on QEMU's net0 using desktop curl
#   update-today  logs in as root with a working network and runs `ferrion update`
#           and `pkg update`: neither fetches nor changes the kernel or initfs,
#           and the next start reaches the login (x86_64 desktop)
#   net-hardening  boots twice with a host-side peer on QEMU's network hub, the card
#           chosen by --make net= (e1000 by default, or virtio), and logs in as root:
#           DHCP replies for another request are ignored, TCP initial sequence numbers differ
#           between boots, do not step by a fixed amount within one and are not what
#           smoltcp's generator gives after the two before them, source ports do not count
#           up, 20 lookups take only the reply with their ID and question from the server's
#           address and port, frames of 1,514 to 9,000 bytes and reads of the driver's `mac`
#           past its end (`net-call-probe mac`) leave the driver running, and 40,000 ARP
#           senders grow the stack by less than 512 KiB (x86_64 desktop)
#   net-calls  boots with the same peer as DHCP and name server, logs in as `user` and
#           runs net-call-probe: calls on tcp and udp sockets whose buffers are too short or
#           whose sizes run past them are refused or cut to fit, and a lookup and a TCP
#           connection leave the guest before and after it (card from --make net=;
#           x86_64 desktop)
#   audio-baseline  boots four times with Intel HDA recorded by QEMU's wav backend
#           (make's own sound device off with audio=no), logs in as root and writes
#           6 s of 16-bit stereo made by `seq` to /scheme/audio at volume 100 and at
#           the default volume, each without and with two busy loops per CPU; the
#           host compares each recording with the same bytes frame by frame and
#           writes the numbers to audio-baseline.json
#   audio-ring  the same sound device: as root, `audio-probe ring-churn 20` opens
#           and closes the driver's shared ring 20 times, then the signal plays at
#           volume 100; passes when every round opens the ring, ihdad's descriptor
#           count in /scheme/sys/iostat returns, the signal is exact in one piece and
#           the console answers (x86_64 desktop)
#   audio-snoop  the same sound device, image of --config audio-snoop: without a grant a
#           record stream gets zeros and no error; with the test grant it hears the
#           loopback input's 440 Hz and none of another program's 880 Hz; a tap hears
#           that program only while a capture-service grant names it
# --check-wav FILE boots nothing: it runs the same checker on a recording of the
# rig's counter signal (plans/audio.md, "The rig"), or of the seq bytes with
# --signal seq, and exits 0 only when the signal is exact in one piece.
# Every run boots with -snapshot, so the image is never changed (disk-keys and
# disk-install boot copies in a temporary directory instead), and `make` is
# told the image is up to date, so a run never starts a build. Screenshots
# (PNG) and the serial log of each run go under --out. Run from the
# repository root. Python standard library only; firefox-cjk is BLOCKED without Pillow.
# 3D devices use EGL headless and a private Unix VNC socket; screenshots fall
# back to RFB when screendump reports no surface. --trace EVENT records QEMU
# events in qemu-trace.log, read by VM.trace(). Each run writes result.json with
# host versions, capture sources and PASS/FAIL/BLOCKED. Exit codes: 0/1/2;
# failures take priority over blocked runs. BLOCKED means a 3D host lacks the
# Linux 6.13 floor, the device properties or the EGL headless backend.
# Headless EGL uses the firmware primary GPU; QEMU_RENDER_NODE overrides it.
# QEMU_VK_DRIVER_FILES selects the Vulkan driver exposed through Venus.

import argparse
import base64
import contextlib
import enum
import json
import os
import re
import shutil
import shlex
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
if str(Path(__file__).resolve().parent) not in sys.path:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
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
        match = re.match(rb"P6\s+(\d+)\s+(\d+)\s+255\s", data)
        if not match:
            raise ValueError("not an 8-bit P6 image")
        width, height = int(match.group(1)), int(match.group(2))
        rgb = data[match.end():]
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


class Blocked(RuntimeError):
    """The host cannot run the requested graphics device."""


HANDLED_SIGNALS = (signal.SIGTERM, signal.SIGHUP)


class Interrupted(BaseException):
    def __init__(self, signum):
        self.signum = signum
        self.signal_name = signal.Signals(signum).name
        super().__init__(f"interrupted by {self.signal_name} ({signum})")


def command_output(command):
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=15)
        return result.stdout.strip() if result.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        return None


SETTING = "vm-test-setting:"


def qemu_settings(args, make=()):
    """Ask make so .config, environment and command-line overrides agree with the boot."""
    # Each value carries a prefix, so a line a makefile prints with $(info ...) is never taken for one.
    rule = (f'vm-test-settings: ; @printf "{SETTING}%s\\n" '
            '"$(QEMU)" "$(gpu)" "$(QEMU_VK_DRIVER_FILES)" "$(QEMU_RENDER_NODE)"')
    command = ["make", "-s", "--no-print-directory", "--eval", rule, "vm-test-settings",
               f"ARCH={args.arch}", f"CONFIG_NAME={args.config}", *args.make, *make]
    result = subprocess.run(command, cwd=ROOT, env=dict(os.environ, CI="1", COOKBOOK_LOGS="true"),
                            capture_output=True, text=True, timeout=30)
    lines = [line[len(SETTING):] for line in result.stdout.splitlines() if line.startswith(SETTING)]
    if result.returncode or len(lines) != 4:
        raise RuntimeError(f"cannot read QEMU settings: {result.stdout}{result.stderr}")
    return shlex.split(lines[0]), lines[1], lines[2], lines[3]


def boot_render_node():
    """Use the firmware's primary GPU for headless EGL on a host with multiple GPUs."""
    for card in sorted(Path("/sys/class/drm").glob("card[0-9]*")):
        boot_vga = card / "device/boot_vga"
        if boot_vga.exists() and boot_vga.read_text().strip() == "1":
            nodes = sorted((card / "device/drm").glob("renderD*"))
            if nodes:
                return "/dev/dri/" + nodes[0].name
    return ""


def host_versions(qemu):
    kernel = os.uname().release
    drivers = []
    for card in sorted(Path("/sys/class/drm").glob("card[0-9]*")):
        module = card / "device/driver/module"
        if not module.exists():
            continue
        version = module / "version"
        drivers.append({"card": card.name, "driver": module.resolve().name,
                        "version": version.read_text().strip() if version.exists() else kernel})
    return {"kernel": kernel, "system": sys.platform,
            "qemu": command_output([*qemu, "--version"]),
            "virglrenderer": command_output(["pkg-config", "--modversion", "virglrenderer"]),
            "mesa": command_output(["pkg-config", "--modversion", "gbm"]),
            "gpu_drivers": drivers, "vk_driver_files": os.environ.get("VK_DRIVER_FILES")}


def intel_host():
    try:
        return "GenuineIntel" in Path("/proc/cpuinfo").read_text()
    except OSError:
        return False


def kvm_available():
    return Path("/dev/kvm").exists()


def qemu_accepts(qemu, *options):
    """Whether this QEMU, on this kernel, starts with these options: paused, with no machine, then told to quit."""
    command = [*qemu, "-machine", "none", "-nodefaults", "-display", "none", "-S", "-qmp", "stdio", *options]
    try:
        result = subprocess.run(command, input='{"execute":"qmp_capabilities"}\n{"execute":"quit"}\n',
                                capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


def require_3d_host(qemu, gpu, arch):
    """BLOCKED is for a working host that lacks a feature; a QEMU that does not run is a FAIL."""
    if gpu not in ("venus", "virtio-gl"):
        return
    if command_output([*qemu, "--version"]) is None:
        raise RuntimeError(f"cannot run {shlex.join(qemu)} --version")
    version = re.match(r"(\d+)\.(\d+)", os.uname().release)
    if gpu == "venus" and (sys.platform != "linux" or not version or tuple(map(int, version.groups())) < (6, 13)):
        raise Blocked("Venus checks require Linux 6.13 or newer")
    displays = command_output([*qemu, "-display", "help"])
    if displays is None:
        raise RuntimeError("QEMU did not answer -display help")
    if "egl-headless" not in displays.splitlines():
        raise Blocked("QEMU lacks the egl-headless display backend")
    device = "virtio-vga-gl" if arch == "x86_64" else "virtio-gpu-gl"
    properties = command_output([*qemu, "-device", f"{device},help"])
    required = ("hostmem", "blob", "venus") if gpu == "venus" else ("max_outputs",)
    missing = [name for name in required if not properties or not re.search(rf"\b{name}=", properties)]
    if missing:
        raise Blocked(f"QEMU lacks {device} properties: {', '.join(missing)}")
    # mk/qemu.mk asks KVM to honor the guest's memory types for Venus on an Intel host.
    if (gpu == "venus" and arch == "x86_64" and intel_host() and kvm_available()
            and not qemu_accepts(qemu, "-accel", "kvm,honor-guest-pat=on")):
        raise Blocked("QEMU or the host kernel lacks KVM's honor-guest-pat, which Venus needs on an Intel host")


MAX_RESIZES_PER_FRAME = 16


class RFB:
    """Read raw RGB frames from QEMU's private, unauthenticated Unix VNC socket."""

    def __init__(self, sock):
        self.sock = sock
        self.deadline = None
        self.outstanding = 0
        if self.read(12) != b"RFB 003.008\n":
            raise RuntimeError("QEMU VNC must support RFB 3.8")
        self.sock.sendall(b"RFB 003.008\n")
        security = self.read(self.read(1)[0])
        if 1 not in security:
            raise RuntimeError("QEMU's private VNC socket must offer no authentication")
        self.sock.sendall(b"\x01")
        if self.read(4) != b"\0\0\0\0":
            raise RuntimeError("QEMU refused VNC security negotiation")
        self.sock.sendall(b"\x01")  # Shared session: do not evict another capture client.
        self.width, self.height = struct.unpack("!HH", self.read(4))
        self.read(16)  # Replace the server's pixel format with big-endian 0RGB below.
        self.read(struct.unpack("!I", self.read(4))[0])
        self.sock.sendall(b"\0\0\0\0" + struct.pack("!BBBBHHHBBB3x", 32, 24, 1, 1, 255, 255, 255, 16, 8, 0))
        self.sock.sendall(struct.pack("!BBHii", 2, 0, 2, 0, -223))  # Raw and DesktopSize.
        self.rgb = bytearray(self.width * self.height * 3)

    def read(self, count):
        data = bytearray()
        while len(data) < count:
            if self.deadline is not None:
                remaining = self.deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("no complete VNC frame before the deadline")
                self.sock.settimeout(min(10, remaining))
            try:
                chunk = self.sock.recv(count - len(data))
            except TimeoutError:
                if self.deadline is not None and time.monotonic() >= self.deadline:
                    raise RuntimeError("no complete VNC frame before the deadline") from None
                raise
            if not chunk:
                raise RuntimeError("QEMU closed VNC during a frame")
            data.extend(chunk)
        return bytes(data)

    def frame(self, timeout=60):
        """A whole frame. The server may send it in several updates, so every pixel must arrive in this call."""
        previous = self.sock.gettimeout()
        self.deadline = time.monotonic() + timeout
        resizes = 0
        try:
            while True:
                width, height = self.width, self.height
                if len(self.rgb) != width * height * 3:
                    self.rgb = bytearray(width * height * 3)
                missing = bytearray(b"\x01") * (width * height)
                left = width * height
                resized = False
                while (left or self.outstanding > 0) and not resized:
                    if self.outstanding == 0:
                        self.sock.sendall(struct.pack("!BBHHHH", 3, 0, 0, 0, width, height))
                        self.outstanding += 1
                    resized, covered = self._update(width, height, missing)
                    left -= covered
                if resized:
                    resizes += 1
                    if resizes > MAX_RESIZES_PER_FRAME:
                        raise RuntimeError(f"VNC resized more than {MAX_RESIZES_PER_FRAME} times in one frame")
                    continue
                assert self.outstanding == 0
                self.sock.settimeout(0)
                try:
                    pending = self.sock.recv(1, socket.MSG_PEEK | socket.MSG_DONTWAIT)
                except BlockingIOError:
                    pending = b""
                if pending:
                    self.sock.settimeout(previous)
                    resized, _ = self._update(width, height, missing)
                    if resized:
                        resizes += 1
                        if resizes > MAX_RESIZES_PER_FRAME:
                            raise RuntimeError(f"VNC resized more than {MAX_RESIZES_PER_FRAME} times in one frame")
                        continue
                    raise RuntimeError("unexpected VNC update after complete frame")
                return Frame(width, height, bytes(self.rgb))
        finally:
            self.deadline = None
            self.sock.settimeout(previous)

    def _update(self, width, height, missing):
        """Read up to one FramebufferUpdate; returns whether the screen resized, and how many pixels arrived."""
        while True:
            message = self.read(1)[0]
            if message == 2:  # Bell.
                continue
            if message == 3:  # ServerCutText.
                self.read(3)
                self.read(struct.unpack("!I", self.read(4))[0])
                continue
            if message != 0:
                raise RuntimeError(f"unexpected VNC message {message}")
            break
        self.read(1)
        resized = False
        saw_pixels = False
        covered = 0
        for _ in range(struct.unpack("!H", self.read(2))[0]):
            x, y, w, h, encoding = struct.unpack("!HHHHi", self.read(12))
            if encoding == -223:
                self.width, self.height = w, h
                resized = True
                continue
            if encoding != 0:
                raise RuntimeError(f"unexpected VNC encoding {encoding}")
            saw_pixels = True
            raw = self.read(w * h * 4)
            if resized:
                continue
            if x + w > width or y + h > height:
                raise RuntimeError("VNC rectangle exceeds the framebuffer")
            pixels = bytearray(w * h * 3)
            pixels[0::3], pixels[1::3], pixels[2::3] = raw[1::4], raw[2::4], raw[3::4]
            for row in range(h):
                start = (y + row) * width + x
                self.rgb[start * 3:(start + w) * 3] = pixels[row * w * 3:(row + 1) * w * 3]
                covered += missing[start:start + w].count(1)
                missing[start:start + w] = bytes(w)
        if not (resized and not saw_pixels):
            if self.outstanding <= 0:
                raise RuntimeError("unexpected VNC reply with no outstanding request")
            self.outstanding -= 1
        return resized, covered


SWTPM_START_S = 10
QUIT_WAIT_S = 20
TERM_WAIT_S = 20
KILL_WAIT_S = 10


class SwtpmPhase(enum.Enum):
    CTRL_RUNNING = "ctrl_running"
    STOPPED = "stopped"
    DATA_RUNNING = "data_running"
    CLOSED = "closed"


class Swtpm:
    """A software TPM 2.0 that QEMU reaches as a tpm-crb device, for one boot."""

    def __init__(self):
        self.dir = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-tpm-"))
        self.ctrl = self.dir / "ctrl.sock"
        self.proc = None
        self.phase = SwtpmPhase.STOPPED
        try:
            self._start(SwtpmPhase.CTRL_RUNNING, "--ctrl", f"type=unixio,path={self.ctrl}")
        except BaseException:
            self.close()
            raise
        self.qemu_flags = (f"-chardev socket,id=chrtpm,path={self.ctrl} -tpmdev emulator,id=tpm0,chardev=chrtpm "
                           "-device tpm-crb,tpmdev=tpm0")

    def _start(self, phase, *channels):
        assert self.phase != SwtpmPhase.CLOSED
        assert phase in (SwtpmPhase.CTRL_RUNNING, SwtpmPhase.DATA_RUNNING)
        if self.proc is not None and self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait(timeout=KILL_WAIT_S)
        self.proc = None
        self.phase = SwtpmPhase.STOPPED
        proc = subprocess.Popen(["swtpm", "socket", "--tpm2", "--tpmstate", f"dir={self.dir}", *channels],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        socket_path = Path(channels[-1].split("path=")[1])
        deadline = time.monotonic() + SWTPM_START_S
        while not socket_path.exists():
            if time.monotonic() > deadline or proc.poll() is not None:
                if proc.poll() is None:
                    proc.kill()
                proc.wait(timeout=KILL_WAIT_S)
                raise RuntimeError("swtpm did not start")
            time.sleep(0.05)
        self.proc = proc
        self.phase = phase
        return proc

    def read_pcrs(self, pcrs):
        """The SHA-256 PCRs as lowercase hex, read on the host once QEMU was killed.

        A QEMU that quits cleanly shuts swtpm down and its PCRs are lost; after a kill,
        swtpm stores its volatile state and a second swtpm resumes from it on a data
        socket that tpm2_pcrread can reach."""
        assert self.phase == SwtpmPhase.CTRL_RUNNING
        subprocess.run(["swtpm_ioctl", "--unix", str(self.ctrl), "-v"], check=True, capture_output=True)
        subprocess.run(["swtpm_ioctl", "--unix", str(self.ctrl), "-s"], check=True, capture_output=True)
        self.proc.wait(timeout=TERM_WAIT_S)
        data = self.dir / "data.sock"
        self._start(SwtpmPhase.DATA_RUNNING, "--flags", "not-need-init", "--server", f"type=unixio,path={data}",
                    "--ctrl", f"type=unixio,path={data}.ctrl")
        try:
            text = subprocess.run(["tpm2_pcrread", "-T", f"swtpm:path={data}",
                                   "sha256:" + ",".join(map(str, pcrs))],
                                  check=True, capture_output=True, text=True).stdout
        finally:
            subprocess.run(["swtpm_ioctl", "--unix", f"{data}.ctrl", "-s"], capture_output=True)
        return {int(i): v.lower() for i, v in re.findall(r"(\d+)\s*:\s*0x([0-9A-Fa-f]{64})", text)}

    def close(self):
        if self.phase == SwtpmPhase.CLOSED:
            return
        try:
            if self.proc is not None:
                if self.proc.poll() is None:
                    self.proc.kill()
                self.proc.wait(timeout=TERM_WAIT_S)
        finally:
            self.proc = None
            self.phase = SwtpmPhase.CLOSED
            shutil.rmtree(self.dir, ignore_errors=True)


PROCESS_TREE_LIMIT = 64


class VM:
    """One QEMU run started by `make qemu`: serial on stdio, monitor on a socket."""

    def __init__(self, args, out, tpm=False, qemu_flags="", make=(), snapshot=True):
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
        qemu, gpu, _, render_node = qemu_settings(args, make)
        require_3d_host(qemu, gpu, args.arch)
        self.is_3d = gpu in ("venus", "virtio-gl")
        self.capture_sources = set()
        self.last_capture_source = None
        self.trace_path = (out / "qemu-trace.log").resolve()
        self.trace_path.unlink(missing_ok=True)
        self._stack = contextlib.ExitStack()
        self.priority = None
        self.monitor = None
        self.vnc = None
        self.tpm = None
        try:
            self.scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-"))
            self._stack.callback(shutil.rmtree, self.scratch, True)
            self.monitor_path = self.scratch / "monitor.sock"
            self.vnc_path = self.scratch / "vnc.sock"
            if tpm:
                self.tpm = Swtpm()
                self._stack.callback(self.tpm.close)
            # Without -snapshot the guest writes to the disk; only for copies given by DISK= in `make`.
            flags = f"{'-snapshot ' if snapshot else ''}-monitor unix:{self.monitor_path},server=on,wait=off"
            if self.is_3d:
                flags += f" -vnc unix:{self.vnc_path}"
            events = getattr(args, "trace", [])
            if events:
                flags += " -D " + shlex.quote(str(self.trace_path))
                flags += "".join(" -trace " + shlex.quote(f"enable={event}") for event in events)
            if self.tpm:
                flags += " " + self.tpm.qemu_flags
            if qemu_flags:
                flags += " " + qemu_flags
            env = dict(os.environ, CI="1", COOKBOOK_LOGS="true", QEMUFLAGS=flags)
            if self.is_3d:
                # A desktop-wide EGL_PLATFORM=wayland overrides QEMU's headless GBM display.
                env.pop("EGL_PLATFORM", None)
            # `-o` takes each target exactly as make spells it, relative to the
            # repository root (an absolute path would not match): neither the image
            # nor the package repository behind it is ever rebuilt by a test.
            build = image.parent.relative_to(ROOT)
            command = ["make", "-s", "-o", str(build / "harddrive.img"), "-o", str(build / "repo.tag"), "qemu",
                       "-o", str(build / "redox-live.iso"),
                       f"ARCH={args.arch}", f"CONFIG_NAME={args.config}", *args.make, *make,
                       "QEMU_DISPLAY=egl-headless" if self.is_3d else "QEMU_DISPLAY=none"]
            if self.is_3d:
                command.append("QEMU_RENDER_NODE=" + (render_node or boot_render_node()))
            self.start = time.monotonic()
            self.log = open(out / "serial.log", "wb")
            self._stack.callback(self.log.close)
            self.serial = bytearray()
            self.lock = threading.Lock()
            self.typing = threading.Lock()
            self._cmd_seq = 0
            self.console_desynced = None
            self.reader = threading.Thread(target=self._read_serial, daemon=True)
            self._stack.callback(self._join_reader)
            self._stack.callback(self._close_sockets)
            self.proc = subprocess.Popen(command, cwd=ROOT, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                         stderr=subprocess.STDOUT, start_new_session=True)
            self._stack.callback(self._stop_qemu)
            self.reader.start()
            self.monitor = self._connect_monitor()
            if self.is_3d:
                self.vnc = self._connect_vnc()
                self.rfb = RFB(self.vnc)
        except BaseException:
            self.close()
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

    def _connect_vnc(self, timeout=60):
        # QEMU opens its monitor before its displays, so the VNC socket can appear later.
        deadline = time.monotonic() + timeout
        while True:
            if self.proc.poll() is not None:
                raise RuntimeError(f"make qemu exited with {self.proc.returncode}; see {self.out / 'serial.log'}")
            sock = socket.socket(socket.AF_UNIX)
            sock.settimeout(10)
            try:
                sock.connect(str(self.vnc_path))
                return sock
            except (FileNotFoundError, ConnectionRefusedError):
                sock.close()
                if time.monotonic() >= deadline:
                    raise RuntimeError("QEMU's VNC socket never appeared") from None
                time.sleep(0.05)

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

    def screenshot(self, head=None, device="gpu0"):
        path = self.scratch / "frame.ppm"
        path.unlink(missing_ok=True)
        target = f" {device} {head}" if head is not None else ""
        reply = self.command(f"screendump {path}{target}")
        if "no surface" in reply.lower() and self.is_3d and head in (None, 0):
            frame = self.rfb.frame()
            self.last_capture_source = "vnc"
            self.capture_sources.add("vnc")
            return frame
        if not path.exists():
            raise RuntimeError(f"screendump failed: {reply}")
        self.last_capture_source = "screendump"
        self.capture_sources.add("screendump")
        return Frame.from_ppm(path.read_bytes())

    def trace(self, event=None):
        """Read host trace lines, optionally selecting one event (enable it with --trace)."""
        lines = self.trace_path.read_text(errors="replace").splitlines() if self.trace_path.exists() else []
        return [line for line in lines if event is None or re.search(rf"\b{re.escape(event)}\b", line)]

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

    def _next_cmd_seq(self):
        if self.console_desynced is not None:
            raise RuntimeError(f"serial console desynced after unanswered command: {self.console_desynced}")
        self._cmd_seq += 1
        return self._cmd_seq

    def kill(self):
        """Stops QEMU at once, as pulling the plug would."""
        os.killpg(self.proc.pid, signal.SIGKILL)
        self.proc.wait(timeout=TERM_WAIT_S)

    def qemu_priority(self):
        """Returns the nice value and I/O class the host gave QEMU, or None when no QEMU process is found."""
        pending, visited = [str(self.proc.pid)], 0
        while pending and visited < PROCESS_TREE_LIMIT:
            pid = pending.pop()
            visited += 1
            try:
                if Path(f"/proc/{pid}/comm").read_text().startswith("qemu-system"):
                    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
                    return {"nice": int(fields[16]), "io": command_output(["ionice", "-p", pid])}
                pending.extend(Path(f"/proc/{pid}/task/{pid}/children").read_text().split())
            except OSError:
                continue
        return None

    def _wait_group(self, wait_s):
        deadline = time.monotonic() + wait_s
        if self.proc.poll() is None:
            try:
                self.proc.wait(timeout=wait_s)
            except subprocess.TimeoutExpired:
                return False
        while True:
            try:
                os.killpg(self.proc.pid, 0)
            except ProcessLookupError:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.05)

    def _stop_qemu(self):
        self.priority = self.qemu_priority()
        if self.proc.poll() is not None:
            try:
                os.killpg(self.proc.pid, 0)
            except ProcessLookupError:
                return
        elif self.monitor is not None:
            try:
                self.monitor.sendall(b"quit\n")
                if self._wait_group(QUIT_WAIT_S):
                    return
            except OSError:
                pass
        for sig, wait_s in ((signal.SIGTERM, TERM_WAIT_S), (signal.SIGKILL, KILL_WAIT_S)):
            try:
                os.killpg(self.proc.pid, sig)
            except ProcessLookupError:
                pass
            if self._wait_group(wait_s):
                return
        raise RuntimeError(f"QEMU process group {self.proc.pid} did not exit after SIGKILL")

    def _join_reader(self):
        if self.reader.ident is not None:
            self.reader.join(timeout=5)

    def _close_sockets(self):
        for name in ("vnc", "monitor"):
            sock = getattr(self, name, None)
            if sock is not None:
                setattr(self, name, None)
                sock.close()

    def close(self):
        if threading.current_thread() is not threading.main_thread():
            self._stack.close()
            return
        saved = {sig: signal.signal(sig, signal.SIG_IGN) for sig in HANDLED_SIGNALS}
        try:
            self._stack.close()
        finally:
            for sig, handler in saved.items():
                signal.signal(sig, handler)


def firmware_status_lines(frame):
    """True when `frame` shows the firmware's logo with lines of text above it.

    OVMF prints its own status lines ("BdsDxe: loading Boot0002 ...", "BdsDxe: starting ...") at
    the top of the screen above its logo on every boot, before the bootloader runs and clears the
    screen; a screenshot catches them now and then. Below the top eighth the frame must be the
    firmware's logo frame; text after the bootloader has cleared the screen still counts as text.
    """
    top = frame.width * (frame.height // 8) * 3
    return Frame(frame.width, frame.height, bytes(top) + frame.rgb[top:]).kind() == "firmware"


def test_boot(vm, args):
    """No text from power-on to the graphical login, and the login arrives in time."""
    seen = []
    while vm.elapsed() < args.timeout:
        frame = vm.screenshot()
        kind = frame.kind()
        if kind == "text" and firmware_status_lines(frame):
            kind = "firmware"
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


def test_gpu_heads(vm, args):
    """Orbital paints both outputs of one two-head virtio display device."""
    while vm.elapsed() < args.timeout:
        if vm.screenshot().kind() == "login":
            break
        time.sleep(0.2)
    else:
        for head in range(2):
            vm.screenshot(head=head).save_png(vm.out / f"head-{head}.png")
        return False, f"no graphical login within {args.timeout}s"
    vm.command("sendkey ret")
    time.sleep(2)
    deadline = time.monotonic() + args.timeout
    while True:
        frames = [vm.screenshot(head=head) for head in range(2)]
        kinds = [frame.kind() for frame in frames]
        painted = [kind != "placeholder" and frame.has_ground_row() for frame, kind in zip(frames, kinds)]
        if all(painted) or time.monotonic() >= deadline:
            break
        time.sleep(0.5)
    for head, frame in enumerate(frames):
        frame.save_png(vm.out / f"head-{head}.png")
    states = ["Orbital background" if kind == "black" and painted[head] else kind
              for head, kind in enumerate(kinds)]
    outputs = ", ".join(f"head {head}: {frame.width}x{frame.height} {states[head]}"
                        for head, frame in enumerate(frames))
    return all(painted), outputs + ("; both painted by Orbital" if all(painted) else "; an output is unpainted")


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
    if not enter_shell(vm, "user", r"(?:user:[^\r\n]*\$|bash-[0-9.]+\$|>) ", args.timeout):
        return False, f"no user shell prompt within {args.timeout}s"
    failed = []
    for line, pattern in SHELL_CHECKS:
        since = vm.mark()
        vm.type(line + "\n")
        if not vm.wait_serial(pattern, args.timeout, since):
            failed.append(line)
    if failed:
        return False, f"no expected output from: {failed}; see {vm.out / 'serial.log'}"
    return True, f"{len(SHELL_CHECKS)} shell checks passed"


def test_logout(vm, args):
    """Logs in as `user` on the serial console and exits; the login prompt returns and procmgr logs no refused SIGCHLD."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    vm.type("exit\n")
    if not vm.wait_serial(r"login: ", 60, since):
        return False, f"no login prompt after `exit`; see {vm.out / 'serial.log'}"
    time.sleep(2)
    refused = re.search(r"\[src/procmgr\.rs:\d+ ERROR\] failed to send SIGCHLD.*", vm.text())
    if refused:
        return False, f"procmgr refused the exit SIGCHLD: {refused.group(0).strip()!r}"
    return True, "the login prompt returned and no SIGCHLD was refused"


# How many times the ls-loop test runs `ls /scheme`. Ion signals each command it starts
# (SIGCONT), and the crash needs that signal to land while `ls` is starting. Before relibc's
# ferrion-sigentry-rcx.patch it crashed in 3 of 3 boots of the desktop with its display running,
# and about once in 1000 to 3000 runs without a display; use --repeat.
LS_LOOP_RUNS = 1000


def test_ls_loop(vm, args):
    """Logs in as `root` on the serial console and runs `ls /scheme` 1000 times (LS_LOOP_RUNS) from Ion; no run may crash."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20):
        return False, "no password prompt for root"
    vm.type("password\n")
    time.sleep(3)
    since = vm.mark()
    vm.type(f"for i in 1..={LS_LOOP_RUNS}; ls /scheme > /dev/null; end; echo ls-loop-$((40+2))\n")
    end = vm.wait_serial(r"ls-loop-42|ended by signal", 300, since)
    time.sleep(1)
    crash = re.search(r".*EXCEPTION RAISED.*NAME /usr/bin/ls.*", vm.text(since))
    if crash or (end and end.group(0) != "ls-loop-42"):
        fault = re.search(r"Page fault: .*", vm.text(since))
        found = (crash.group(0).strip() if crash else end.group(0)) + (f"; {fault.group(0).strip()}" if fault else "")
        return False, f"ls crashed: {found}; see {vm.out / 'serial.log'}"
    if not end:
        return False, f"the loop did not finish within 300s; see {vm.out / 'serial.log'}"
    return True, f"{LS_LOOP_RUNS} runs of `ls /scheme` without a crash"


def test_signal_handle(vm, args):
    """Logs in as `user` on the serial console and runs signal-handle-probe: a root child that sudo made in the probe's session refuses SIGTERM both from kill() and through a process handle."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    vm.type("signal-handle-probe\n")
    verdict = vm.wait_serial(r"signal-handle-probe: (REFUSED|ACCEPTED|INCONCLUSIVE)", 60, since)
    detail = re.search(r"signal-handle-probe: uid .*", vm.text(since))
    detail = detail.group(0).strip() if detail else "no result line"
    if not verdict:
        return False, f"signal-handle-probe printed no verdict; see {vm.out / 'serial.log'}"
    return verdict.group(1) == "REFUSED", f"{verdict.group(1)}: {detail}"


def test_dlopen_ctor(vm, args):
    """Runs a shared-library constructor that calls dlsym and nested dlopen/dlclose, checking initialized symbols and process exit (use --config firefox)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    # Bash expands the exit status; the echoed command cannot match the numeric marker.
    vm.type("bash -c 'dlopen-ctor; echo dlopen-ctor-exit-$?'\n")
    found = vm.wait_serial(r"(?s)\n(.*?)dlopen-ctor-exit-(\d+)\s", 30, since)
    output = found.group(1) if found else vm.text(since)
    status = int(found.group(2)) if found else None
    log = vm.out / "dlopen-ctor.txt"
    log.write_text(f"exit {status}\n{output}\n")
    if status is None:
        return False, f"constructor probe did not exit within 30s (possible loader deadlock); see {log}"
    if status != 0 or "dlopen-ctor: FAIL" in output:
        return False, f"constructor probe failed (exit {status}); see {log}"
    expected = ("constructor-dlsym", "nested-initialized-symbol", "nested-dlclose",
                "nested-destructor", "outer-initialized-symbol", "outer-dlclose")
    missing = [name for name in expected if f"dlopen-ctor: pass {name}" not in output]
    if missing or not re.search(r"dlopen-ctor: PASS\r?\n", output):
        return False, f"incomplete constructor results (missing {missing}); see {log}"
    return True, "constructor dlsym, nested dlopen/dlclose, initialized symbols and process exit passed"


def test_dlopen_tls(vm, args):
    """Loads two DSOs with distinct TLS objects, then checks independent null initialization and writable storage in a later pthread (use --config firefox)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    # Bash expands the exit status; the echoed command cannot match the numeric marker.
    vm.type("bash -c 'dlopen-tls; echo dlopen-tls-exit-$?'\n")
    found = vm.wait_serial(r"(?s)\n(.*?)dlopen-tls-exit-(\d+)\s", 30, since)
    output = found.group(1) if found else vm.text(since)
    status = int(found.group(2)) if found else None
    log = vm.out / "dlopen-tls.txt"
    log.write_text(f"exit {status}\n{output}\n")
    if status is None:
        return False, f"TLS probe did not exit within 30s; see {log}"
    if status != 0 or "dlopen-tls: FAIL" in output:
        return False, f"TLS probe failed (exit {status}); see {log}"
    expected = ("dlopen", "thread-tls-initial", "thread-tls-write")
    missing = [name for name in expected if f"dlopen-tls: pass {name}" not in output]
    if missing or not re.search(r"dlopen-tls: PASS\r?\n", output):
        return False, f"incomplete TLS results (missing {missing}); see {log}"
    return True, "dlopened library TLS was initialized, writable and readable in a later pthread"


def test_cond_clock(vm, args):
    """Checks that pthread condition variables honor realtime and monotonic deadline clocks by waiting for a helper's signal (use --config firefox)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    vm.type("bash -c 'cond-clock-probe; echo cond-clock-exit-$?'\n")
    found = vm.wait_serial(r"(?s)\n(.*?)cond-clock-exit-(\d+)\s", 30, since)
    output = found.group(1) if found else vm.text(since)
    status = int(found.group(2)) if found else None
    log = vm.out / "cond-clock.txt"
    log.write_text(f"exit {status}\n{output}\n")
    if status is None:
        return False, f"condition clock probe did not exit within 30s; see {log}"
    if status != 0 or "cond-clock-probe: FAIL" in output:
        return False, f"condition clock probe failed (exit {status}); see {log}"
    expected = ("realtime-signal", "monotonic-signal")
    missing = [name for name in expected if f"cond-clock-probe: pass {name}" not in output]
    if missing or not re.search(r"cond-clock-probe: PASS\r?\n", output):
        return False, f"incomplete condition clock results (missing {missing}); see {log}"
    return True, "realtime and monotonic condition deadlines both waited for their signals"


def user_probe(vm, args, probe, passes, timeout=60):
    """Logs in as `user` on the serial console and runs `probe`; passes when it exits 0, prints
    `<probe>: PASS` and `<probe>: pass <name>` for each name in `passes`, and no FAIL line."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    vm.type(f"bash -c '{probe}; echo {probe}-exit-$?'\n")
    found = vm.wait_serial(rf"(?s)\n(.*?){probe}-exit-(\d+)\s", timeout, since)
    output = found.group(1) if found else vm.text(since)
    status = int(found.group(2)) if found else None
    log = vm.out / f"{probe}.txt"
    log.write_text(f"exit {status}\n{output}\n")
    if status is None:
        return False, f"{probe} did not exit within {timeout}s; see {log}"
    missing = [name for name in passes if f"{probe}: pass {name}" not in output]
    if status != 0 or f"{probe}: FAIL" in output or missing or not re.search(rf"{probe}: PASS\r?\n", output):
        return False, f"{probe} failed (exit {status}, missing {missing}); see {log}"
    return True, None


def test_fork_wake(vm, args):
    """Threads blocked on a mutex and on a condition variable before fork() are woken by the parent afterwards (use --config firefox)."""
    ok, note = user_probe(vm, args, "fork-wake-probe", ("mutex", "cond"))
    return ok, note or "mutex and condition waiters blocked before fork() were woken"


def test_ipc_fd(vm, args):
    """Firefox's IPC primitives: shm_open with a read-only reopen, posix_fallocate, socketpair, SCM_RIGHTS that copy descriptors, MSG_CMSG_CLOEXEC, epoll, and fork+exec with shared memory mapped (use --config firefox)."""
    ok, note = user_probe(vm, args, "ipc-fd-probe", ())
    return ok, note or "shared memory, descriptor passing, epoll and fork+exec checks passed"


def test_directory_fsync(vm, args):
    """Syncs read-only directory descriptors after journal creation and unlink, including O_DIRECTORY (use --config firefox)."""
    ok, note = user_probe(vm, args, "directory-fsync-probe", ("read-only", "directory-flag", "invalid"))
    return ok, note or "directory fsync accepts both open modes and rejects an invalid descriptor"


def test_jit_signal(vm, args):
    """What SpiderMonkey's JIT asks of signals and memory: SIGSEGV and SIGILL handlers that read and change the machine context and resume, faults on PROT_NONE loads, non-executable and RW-to-RX code pages, a handler on a thread's alternate stack, and SIG_DFL after a handler (use --config firefox)."""
    passes = ("segv-store", "sigill-ud2", "segv-exec-nx", "wx-reserve", "wx-exec", "wx-store-faults",
              "wx-rw-not-exec", "wx-rewrite", "wx-decommit", "segv-load-prot-none", "segv-load-mprotect-none",
              "thread-altstack", "segv-repeat", "segv-default-after-handler")
    ok, note = user_probe(vm, args, "jit-signal-probe", passes)
    return ok, note or "fault handlers read and rewrote the machine context; W^X code pages and PROT_NONE behaved"


def test_fdtable_race(vm, args):
    """Three threads run dup2 and F_DUPFD_CLOEXEC while three others open, dup and close, 20000 rounds each; passes when none deadlocks on the descriptor table (use --config firefox)."""
    ok, note = user_probe(vm, args, "fdtable-race-probe", ("dup2-against-open",), timeout=120)
    return ok, note or "dup2 and F_DUPFD raced open, dup and close without a deadlock"


def test_fork_filetable(vm, args):
    """Five threads churn open, close, pipe2, F_DUPFD_CLOEXEC, dup2, MAP_SHARED mmap, SCM_RIGHTS and malloc while the main thread runs fork+exec and posix_spawn against a descriptor checker, then verifies no probe or checker context remains in KB (use --config desktop)."""
    ok, note = user_probe(vm, args, "fork-filetable-probe", ("fork-exec-churn", "spawn-churn"), timeout=240)
    since = vm.mark()
    vm.type("bash -c 'grep -E \"fork-filetable-(probe|checker)\" /scheme/sys/context || true; echo kb-check-done'\n")
    found = vm.wait_serial(r"(?s)\n(.*?)kb-check-done\s", 30, since)
    ctx_out = found.group(1) if found else vm.text(since)
    (vm.out / "fork-filetable-context.txt").write_text(ctx_out)
    kb_lines = [line.strip() for line in ctx_out.splitlines() if re.search(r"fork-filetable-(probe|checker)", line) and re.search(r"\bKB\b", line)]
    if not ok:
        if kb_lines:
            return False, f"{note}; stuck KB contexts: {kb_lines}"
        return False, note
    if kb_lines:
        return False, f"probe or checker left in KB after exit: {kb_lines}; see {vm.out / 'fork-filetable-context.txt'}"
    return True, "multithreaded fork+exec and posix_spawn under descriptor, mmap and malloc churn passed with zero leaks, errors or KB contexts"


def test_exec_handles(vm, args):
    """Checks that an exec'd or spawned program from a single-threaded or multithreaded parent, or through ferrion-run and ferrion-scoped, holds only the runtime handles exec gives it (use --config desktop)."""
    passes = ("fork-exec-1", "posix-spawn-1", "fork-exec-multi", "posix-spawn-multi", "direct-exec-multi")
    probe_ok, probe_note = user_probe(vm, args, "exec-handles-probe", passes, timeout=120)
    problems = []
    if not probe_ok:
        problems.append(f"probe: {probe_note}")

    def run_cmd(tag, line, timeout=60, sudo=False):
        since = vm.mark()
        vm.type(f"bash -c '{line}; echo eh-end-{tag}-$?'\n")
        end = rf"(?s)\n(.*)eh-end-{tag}-(\d+)"
        if sudo and vm.wait_serial(rf"\[sudo\] password|eh-end-{tag}-\d", timeout, since):
            if not re.search(rf"eh-end-{tag}-\d", vm.text(since)):
                vm.type("\n")
        found = vm.wait_serial(end, timeout, since)
        output = found.group(1) if found else vm.text(since)
        return (int(found.group(2)) if found else None), output

    launcher_log = []
    for label, cmd in (
        ("ferrion-run-plain", "ferrion-run --no-network /usr/bin/exec-handles-checker ferrion-run-plain 1 1"),
        ("ferrion-run-files", "mkdir -p /tmp/exec-handles-scope && ferrion-run --no-network --files /tmp/exec-handles-scope /usr/bin/exec-handles-checker ferrion-run-files 1 1"),
    ):
        rc, out = run_cmd(label, cmd, timeout=60)
        launcher_log.append(f"=== {label} rc={rc} ===\n{out}\n")
        if rc != 0 or "extra=0" not in out:
            problems.append(f"{label} failed (exit {rc})")
    (vm.out / "exec-handles-launchers.txt").write_text("".join(launcher_log))

    context_text = None
    for attempt in range(3):
        rc, out = run_cmd(
            f"context-{attempt}",
            "sudo sh -c \"sleep 12 && cat /scheme/sys/context > /tmp/eh-context.txt && cat /tmp/eh-context.txt\"",
            timeout=90,
            sudo=True,
        )
        if rc == 0 and re.search(r"(?m)^\s*\d+\s+", out):
            context_text = out
            break
    if context_text is None:
        problems.append("exec-handles context read failed")
    else:
        (vm.out / "exec-handles-context.txt").write_text(context_text)
        lingering = [
            line.strip()
            for line in context_text.splitlines()
            if "exec-handles-probe" in line
        ]
        if lingering:
            problems.append(f"exec-handles-probe contexts lingered after 12s ({len(lingering)}): {lingering[:4]}")

    iostat_text = None
    for attempt in range(3):
        rc, out = run_cmd(
            f"iostat-{attempt}",
            "sudo sh -c \"cat /scheme/sys/iostat > /tmp/eh-iostat.txt && cat /tmp/eh-iostat.txt\"",
            timeout=90,
            sudo=True,
        )
        if rc == 0 and re.search(r"(?m)^\d+: \S+$", out):
            iostat_text = out
            break
    if iostat_text is None:
        problems.append("ferrion-scope iostat read failed")
    else:
        (vm.out / "exec-handles-iostat.txt").write_text(iostat_text)
        upper_by_proc = {}
        upper_by_pid = {}
        pid_names = {}
        cur_pid = None
        cur_entries = None
        for raw_line in iostat_text.splitlines():
            line = raw_line.rstrip("\r")
            m_proc = IOSTAT_PROCESS.match(line)
            if m_proc:
                cur_pid = int(m_proc.group(1))
                proc_name = m_proc.group(2).rsplit("/", 1)[-1]
                cur_entries = []
                upper_by_proc.setdefault(proc_name, []).append((m_proc.group(1), cur_entries))
                pid_names.setdefault(cur_pid, set()).add(proc_name)
                upper_by_pid.setdefault(cur_pid, {})
                continue
            m_fd = IOSTAT_FD.match(line)
            if m_fd and m_fd.group(1) == "U" and cur_entries is not None:
                entry = (m_fd.group(2), m_fd.group(3), m_fd.group(4), m_fd.group(5).strip())
                cur_entries.append(entry)
                desc = (int(m_fd.group(3)), int(m_fd.group(4)))
                upper_by_pid[cur_pid].setdefault(desc, []).append((m_fd.group(2), m_fd.group(5).strip()))
        scoped_list = upper_by_proc.get("ferrion-scoped", [])
        proxy_list = upper_by_proc.get("ferrion-scope-proxy", [])
        if not scoped_list or not proxy_list:
            problems.append(f"missing iostat process (ferrion-scoped={len(scoped_list)}, ferrion-scope-proxy={len(proxy_list)})")
        else:
            scoped_descs = {}
            for _, entries in scoped_list:
                for slot, scheme_id, number, path in entries:
                    scoped_descs[(scheme_id, number)] = (slot, path)
            shared = []
            for pid, entries in proxy_list:
                for slot, scheme_id, number, path in entries:
                    if (scheme_id, number) in scoped_descs:
                        if path == "/scheme/initfs":
                            continue
                        s_slot, s_path = scoped_descs[(scheme_id, number)]
                        shared.append(f"proxy({pid}) U{slot}==scoped U{s_slot} desc=({scheme_id},{number}) path={path or s_path}")
            if shared:
                problems.append(f"ferrion-scope-proxy shares non-cwd upper handles with ferrion-scoped: {shared}")
        p1_handles = upper_by_pid.get(1, {})
        initfs_schemes = {
            s for (s, _), items in p1_handles.items()
            if any(p.startswith("/scheme/initfs") for _, p in items)
        }
        procmgr_schemes = {
            s for (s, _), items in p1_handles.items()
            if any(p.startswith("/scheme/proc/") or p == "/scheme/proc" for _, p in items)
        }
        boot_descs = ()
        if len(initfs_schemes) == 1 and len(procmgr_schemes) == 1:
            candidate = ((next(iter(procmgr_schemes)), 0), (next(iter(initfs_schemes)), 1))
            if all(desc in p1_handles for desc in candidate):
                boot_descs = candidate
        if not boot_descs:
            problems.append("boot descriptions not found")
        boot_holders_outside_init = []
        proc_holders = {}
        for pid, handles in sorted(upper_by_pid.items()):
            names = ",".join(sorted(pid_names[pid]))
            for desc, items in handles.items():
                if desc in boot_descs and pid not in (0, 1):
                    boot_holders_outside_init.append(f"pid {pid} ({names}) U{items[0][0]} desc={desc}")
                if any(
                    p.startswith("/scheme/proc/")
                    or p.startswith("/scheme/kernel.proc/")
                    or p in ("/scheme/proc", "/scheme/kernel.proc")
                    for _, p in items
                ):
                    proc_holders.setdefault(desc, []).append((pid, names, items[0][0]))
        if boot_holders_outside_init:
            problems.append(
                f"boot-wide upper handles held outside procmgr and init (PIDs 0 and 1) ({len(boot_holders_outside_init)}): {boot_holders_outside_init[:6]}"
            )
        shared_proc = []
        for desc, holders in sorted(proc_holders.items()):
            if len(holders) > 1 and not (desc in boot_descs and all(h[0] in (0, 1) for h in holders)):
                shared_proc.append(f"desc={desc} holders={holders[:6]}")
        if shared_proc:
            problems.append(f"shared proc upper handles across pids ({len(shared_proc)}): {shared_proc[:6]}")

    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    return True, "exec'd and spawned programs (1 and 9 threads, direct exec, ferrion-run, ferrion-scope, system-wide iostat) held only auxv runtime handles"


def test_nvme_queue(vm, args):
    """Runs nvme-queue-probe for 300 s (128 + (r % 64) 128 KiB records compressed to 4 KiB blocks), failing with /scheme/sys watcher dumps if no progress occurs for 20 s (use --config firefox)."""
    if not user_bash(vm, args.timeout):
        return False, "no user Bash prompt after login"

    def run(line, timeout):
        since = vm.mark()
        vm.type(f"{line}; echo NVQRC-$((40+2))=$?\n")
        found = vm.wait_serial(r"(?s)\n(.*?)NVQRC-42=(\d+)\s", timeout, since)
        return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))

    status, _ = run("unset MAIL MAILPATH MAILCHECK; " + GUEST_WATCH, 30)
    if status != 0 or not guest_watch(vm, "watch-before", "context syscall iostat", 120):
        return False, "the guest watcher did not start"

    duration = 300
    stall_limit = 20
    since = vm.mark()
    started = time.monotonic()
    status, _ = run(f"(nvme-queue-probe {duration}; echo NVMEQ-EXIT=$?) & true", 30)
    if status != 0:
        return False, f"failed to start nvme-queue-probe (status {status})"

    last_progress = time.monotonic()
    last_round = -1
    small_rounds = {}
    done_match = None
    exit_match = None
    seen_chars = 0
    deadline = started + duration + 120
    while time.monotonic() < deadline:
        chunk = vm.text(since)[seen_chars:]
        nl = chunk.rfind("\n")
        if nl >= 0:
            lines = chunk[:nl + 1]
            seen_chars += nl + 1
            for match in re.finditer(r"(?m)^NVMEQ (\d+)(?: (\d+))?$", lines):
                last_round = int(match.group(1))
                last_progress = time.monotonic()
            for match in re.finditer(r"(?m)^NVMEQ-SMALL (\d+) (\d+)(?: (\d+))?$", lines):
                small_rounds[match.group(1)] = int(match.group(2))
                last_progress = time.monotonic()
            done_match = done_match or re.search(r"(?m)^NVMEQ-DONE rounds=(\d+) elapsed_ms=(\d+)$", lines)
            exit_match = exit_match or re.search(r"(?m)^NVMEQ-EXIT=(\d+)$", lines)
            if exit_match:
                break
        if time.monotonic() - last_progress >= stall_limit:
            stalled_s = round(time.monotonic() - started, 1)
            guest_watch(vm, "watch-failed-1", "context syscall", 60)
            time.sleep(3)
            guest_watch(vm, "watch-failed-2", "context syscall", 60)
            guest_watch(vm, "watch-failed-iostat", "iostat", 60)
            results = {"stalled": True, "last_round": last_round, "small_rounds": small_rounds, "stalled_after_s": stalled_s}
            (vm.out / "nvme-queue.json").write_text(json.dumps(results, indent=2) + "\n")
            return False, f"stalled after {stalled_s}s at round {last_round} (small {small_rounds}, no progress for {stall_limit}s); see {vm.out}"
        time.sleep(0.5)

    elapsed_s = round(time.monotonic() - started, 1)
    exit_code = int(exit_match.group(1)) if exit_match else None
    if exit_code != 0 or not done_match:
        guest_watch(vm, "watch-failed-1", "context syscall", 60)
        time.sleep(3)
        guest_watch(vm, "watch-failed-2", "context syscall", 60)
        guest_watch(vm, "watch-failed-iostat", "iostat", 60)
        results = {"stalled": exit_code is None, "exit_code": exit_code, "last_round": last_round, "small_rounds": small_rounds, "elapsed_s": elapsed_s}
        (vm.out / "nvme-queue.json").write_text(json.dumps(results, indent=2) + "\n")
        return False, f"nvme-queue-probe failed (exit {exit_code}, last round {last_round}, small {small_rounds}, {elapsed_s}s); see {vm.out}"

    rounds = int(done_match.group(1))
    probe_ms = int(done_match.group(2))
    results = {"stalled": False, "rounds": rounds, "small_rounds": small_rounds, "probe_ms": probe_ms, "elapsed_s": elapsed_s}
    (vm.out / "nvme-queue.json").write_text(json.dumps(results, indent=2) + "\n")
    return True, f"completed {rounds} rounds (small {small_rounds}) in {probe_ms / 1000:.1f}s without stalling; see {vm.out}"


RING_WAKE_MAX_RUN_S = 240
RING_WAKE_STALL_LIMIT_S = 30
RING_WAKE_EXPECTED_ROUNDS = 2420
RING_WAKE_HOLD_WATCH_GAP_S = 3


def parse_watch_iostat(text):
    """Parses W:iostat lines from a guest_watch capture into {pid: {'name': str, 'fds': [(fd, path)]}}."""
    procs = {}
    current_pid = None
    for raw in text.splitlines():
        if not raw.startswith("W:iostat "):
            continue
        body = raw[len("W:iostat "):]
        hdr = re.match(r"^(\d+):\s+(.+)$", body)
        if hdr:
            current_pid = int(hdr.group(1))
            procs[current_pid] = {"name": hdr.group(2).strip(), "fds": []}
            continue
        if current_pid is None:
            continue
        fd_match = re.match(r"^\s*(\d+):\s+\d+\s+\d+\s+[0-9A-Fa-f]+:\s+(.+)$", body)
        if fd_match:
            procs[current_pid]["fds"].append((int(fd_match.group(1)), fd_match.group(2).strip()))
    return procs


def parse_watch_syscalls(text):
    """Parses W:syscall lines from a guest_watch capture into {pid: [syscall_str]}."""
    syscalls = {}
    current_pid = None
    for raw in text.splitlines():
        if not raw.startswith("W:syscall "):
            continue
        body = raw[len("W:syscall "):]
        hdr = re.match(r"^(\d+):\s+(.+)$", body)
        if hdr:
            current_pid = int(hdr.group(1))
            syscalls.setdefault(current_pid, [])
            continue
        if current_pid is not None and body.strip():
            syscalls[current_pid].append(body.strip())
    return syscalls


def find_second_nvmed(watch_text):
    """Identifies the unmounted second NVMe driver by the disk scheme it holds in iostat."""
    procs = parse_watch_iostat(watch_text)
    mounted_schemes = set()
    nvmed_schemes = []
    for pid, info in procs.items():
        if info["name"].endswith("/redoxfs"):
            for _, path in info["fds"]:
                match = re.search(r"(disk\.pci-[0-9A-Fa-f.:_-]+-nvme)", path)
                if match:
                    mounted_schemes.add(match.group(1))
        elif info["name"].endswith("/nvmed"):
            schemes = set()
            for _, path in info["fds"]:
                match = re.search(r"(disk\.pci-[0-9A-Fa-f.:_-]+-nvme)", path)
                if match:
                    schemes.add(match.group(1))
            if len(schemes) == 1:
                nvmed_schemes.append((pid, next(iter(schemes))))
    candidates = [(pid, scheme) for pid, scheme in nvmed_schemes if scheme not in mounted_schemes]
    if len(candidates) != 1:
        return None, None
    return candidates[0]


def test_ring_wake(vm, args):
    """Runs ring-wake-probe read-only on the second NVMe disk to verify that the disk queue worker never blocks the driver's executor on the wake pipe (use --config firefox)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, "no serial login"
    login_mark = vm.mark()
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20):
        return False, "no root password prompt"
    vm.type("password\n")
    if not vm.wait_serial(r"root:.*# ", 20, login_mark):
        return False, "serial shell did not become ready"
    vm.type("bash --norc --noprofile --noediting\n")
    time.sleep(1)

    def run(line, timeout):
        since = vm.mark()
        vm.type(f"{line}; echo RWRC-$((40+2))=$?\n")
        found = vm.wait_serial(r"(?s)\n(.*?)RWRC-42=(\d+)\s", timeout, since)
        return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))

    status, _ = run("unset MAIL MAILPATH MAILCHECK; " + GUEST_WATCH, 30)
    if status != 0 or not guest_watch(vm, "watch-before", "context syscall iostat", 120):
        return False, "the guest watcher did not start"

    second_nvmed_pid, disk_scheme = find_second_nvmed((vm.out / "watch-before.watch").read_text())
    if second_nvmed_pid is None or not disk_scheme:
        return False, f"could not identify the second NVMe disk from watch-before.watch; see {vm.out}"

    since = vm.mark()
    started = time.monotonic()
    status, _ = run(f"(ring-wake-probe /scheme/{disk_scheme}/1; echo RINGWAKE-EXIT=$?) & true", 30)
    if status != 0:
        return False, f"failed to start ring-wake-probe (status {status})"

    last_progress = time.monotonic()
    ticks_per_us = None
    gaps = []
    bad_cqes = []
    stuck_info = None
    done_info = None
    error_lines = []
    exit_code = None
    watched_failure = False
    seen_chars = 0
    deadline = started + RING_WAKE_MAX_RUN_S

    def capture_failure_dumps():
        guest_watch(vm, "watch-failed-1", "context syscall", 60)
        time.sleep(RING_WAKE_HOLD_WATCH_GAP_S)
        guest_watch(vm, "watch-failed-2", "context syscall", 60)

    while time.monotonic() < deadline:
        chunk = vm.text(since)[seen_chars:]
        nl = chunk.rfind("\n")
        if nl >= 0:
            lines = chunk[:nl + 1]
            seen_chars += nl + 1
            for match in re.finditer(r"RINGWAKE-TSC ticks_per_us=([0-9.]+)", lines):
                ticks_per_us = float(match.group(1))
                last_progress = time.monotonic()
            for match in re.finditer(
                r"(?m)^RINGWAKE-GAP gap_us=([0-9.]+) ok=(\d+)(?: bit_clear_a=(\d+))? bit_clear=(\d+)(?: bad_cqe=(\d+))? worst_us=(\d+)$",
                lines,
            ):
                gaps.append({
                    "gap_us": float(match.group(1)),
                    "ok": int(match.group(2)),
                    "bit_clear_a": int(match.group(3)) if match.group(3) is not None else 0,
                    "bit_clear": int(match.group(4)),
                    "bad_cqe": int(match.group(5)) if match.group(5) is not None else 0,
                    "worst_us": int(match.group(6)),
                })
                last_progress = time.monotonic()
            for match in re.finditer(
                r"(?m)^RINGWAKE-BADCQE round=(\d+) gap=([0-9.]+) status=(\d+) count=(\d+)$",
                lines,
            ):
                bad_cqes.append({
                    "round": int(match.group(1)),
                    "gap_us": float(match.group(2)),
                    "status": int(match.group(3)),
                    "count": int(match.group(4)),
                })
                last_progress = time.monotonic()
            for match in re.finditer(
                r"(?m)^RINGWAKE-STUCK round=(\d+) gap_us=([0-9.]+)(?: bit_clear_a=(\d+))? bit_clear=(\d+) stage=(\S+)(?: waited_a=(\d+))? waited_b=(\d+) got_a=(\d+) got_b=(\d+)$",
                lines,
            ):
                stuck_info = {
                    "round": int(match.group(1)),
                    "gap_us": float(match.group(2)),
                    "bit_clear_a": int(match.group(3)) if match.group(3) is not None else 0,
                    "bit_clear": int(match.group(4)),
                    "stage": match.group(5),
                    "waited_a": int(match.group(6)) if match.group(6) is not None else None,
                    "waited_b": int(match.group(7)),
                    "got_a": int(match.group(8)),
                    "got_b": int(match.group(9)),
                }
                last_progress = time.monotonic()
            for match in re.finditer(
                r"(?m)^RINGWAKE-DONE rounds=(\d+)(?: bit_clear_a=(\d+))? bit_clear=(\d+)(?: bad_cqe=(\d+))? worst_us=(\d+)$",
                lines,
            ):
                done_info = {
                    "rounds": int(match.group(1)),
                    "bit_clear_a": int(match.group(2)) if match.group(2) is not None else 0,
                    "bit_clear": int(match.group(3)),
                    "bad_cqe": int(match.group(4)) if match.group(4) is not None else 0,
                    "worst_us": int(match.group(5)),
                }
                last_progress = time.monotonic()
            for match in re.finditer(r"(?m)^RINGWAKE-ERROR (.+)$", lines):
                error_lines.append(match.group(1).strip())
                last_progress = time.monotonic()
            exit_match = re.search(r"(?m)^RINGWAKE-EXIT=(\d+)$", lines)
            if exit_match:
                exit_code = int(exit_match.group(1))
                break
            if stuck_info is not None and not watched_failure:
                capture_failure_dumps()
                watched_failure = True
                found_exit = vm.wait_serial(r"(?m)^RINGWAKE-EXIT=(\d+)$", 30, since)
                if found_exit:
                    exit_code = int(found_exit.group(1))
                break
        if time.monotonic() - last_progress >= RING_WAKE_STALL_LIMIT_S:
            capture_failure_dumps()
            watched_failure = True
            break
        time.sleep(0.2)

    elapsed_s = round(time.monotonic() - started, 1)
    bad_cqe_count = max(len(bad_cqes), done_info["bad_cqe"] if done_info else 0)
    ok = (
        stuck_info is None
        and exit_code == 0
        and done_info is not None
        and done_info["rounds"] == RING_WAKE_EXPECTED_ROUNDS
        and done_info["bit_clear"] > 0
        and bad_cqe_count == 0
        and not error_lines
    )
    if not ok:
        if not watched_failure:
            capture_failure_dumps()
        w1_path = vm.out / "watch-failed-1.watch"
        w2_path = vm.out / "watch-failed-2.watch"
        sc1 = parse_watch_syscalls(w1_path.read_text()) if w1_path.exists() else {}
        sc2 = parse_watch_syscalls(w2_path.read_text()) if w2_path.exists() else {}
        nvmed_sc1 = sc1.get(second_nvmed_pid, [])
        nvmed_sc2 = sc2.get(second_nvmed_pid, [])

        def blocked_reader_fds(calls):
            fds = set()
            for call in calls:
                match = re.match(r"^read\((\d+),\s*0x[0-9A-Fa-f]+,\s*1\)$", call)
                if match:
                    fds.add(int(match.group(1)))
            return fds

        common_read_fds = sorted(blocked_reader_fds(nvmed_sc1) & blocked_reader_fds(nvmed_sc2))
        outcome = "stuck" if stuck_info is not None else "bad_cqe" if bad_cqe_count > 0 else "error"
        results = {
            "passed": False,
            "outcome": outcome,
            "disk_scheme": disk_scheme,
            "second_nvmed_pid": second_nvmed_pid,
            "ticks_per_us": ticks_per_us,
            "stuck": stuck_info,
            "done": done_info,
            "bad_cqe": bad_cqe_count,
            "bad_cqes": bad_cqes,
            "errors": error_lines,
            "exit_code": exit_code,
            "gaps": gaps,
            "nvmed_syscall_1": nvmed_sc1,
            "nvmed_syscall_2": nvmed_sc2,
            "nvmed_blocked_reader_fds": common_read_fds,
            "elapsed_s": elapsed_s,
        }
        (vm.out / "ring-wake.json").write_text(json.dumps(results, indent=2) + "\n")
        if stuck_info is not None:
            return False, (
                f"stuck at round {stuck_info['round']} gap_us={stuck_info['gap_us']:.2f} "
                f"bit_clear_a={stuck_info['bit_clear_a']} bit_clear={stuck_info['bit_clear']} "
                f"waited_a={stuck_info['waited_a']} waited_b={stuck_info['waited_b']} "
                f"stage={stuck_info['stage']} "
                f"nvmed(pid={second_nvmed_pid}) syscall1={nvmed_sc1} syscall2={nvmed_sc2}; see {vm.out}"
            )
        if bad_cqe_count > 0:
            return False, (
                f"bad_cqe={bad_cqe_count} on {disk_scheme} (first={bad_cqes[:1]}, done={done_info}); see {vm.out}"
            )
        return False, (
            f"ring-wake-probe failed (exit={exit_code}, done={done_info}, errors={error_lines}, "
            f"nvmed_pid={second_nvmed_pid}, syscall1={nvmed_sc1}, syscall2={nvmed_sc2}); see {vm.out}"
        )

    results = {
        "passed": True,
        "outcome": "pass",
        "disk_scheme": disk_scheme,
        "second_nvmed_pid": second_nvmed_pid,
        "ticks_per_us": ticks_per_us,
        "rounds": done_info["rounds"],
        "bit_clear_a": done_info["bit_clear_a"],
        "bit_clear": done_info["bit_clear"],
        "bad_cqe": bad_cqe_count,
        "worst_us": done_info["worst_us"],
        "gaps": gaps,
        "elapsed_s": elapsed_s,
    }
    (vm.out / "ring-wake.json").write_text(json.dumps(results, indent=2) + "\n")
    return True, (
        f"completed {done_info['rounds']} rounds on {disk_scheme} "
        f"(bit_clear_a={done_info['bit_clear_a']}, bit_clear={done_info['bit_clear']}, "
        f"bad_cqe={bad_cqe_count}, worst={done_info['worst_us']}us) "
        f"in {elapsed_s}s; see {vm.out}"
    )


def test_fd_primitives(vm, args):
    """Runs the public eventfd/timerfd probe as user: counter and timer I/O, errors, poll/epoll, shared dup/fork state and flags across exec; signalfd is not implemented or tested."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    # The shell, not the probe, reports exit; the typed command cannot match it.
    vm.type("bash -c 'fd-primitives-probe; echo fd-primitives-exit-$?'\n")
    found = vm.wait_serial(r"(?s)\n(.*?)\nfd-primitives-exit-(\d+)\s", 60, since)
    output = found.group(1) if found else vm.text(since)
    status = int(found.group(2)) if found else None
    log = vm.out / "fd-primitives.txt"
    log.write_text(f"exit {status}\n{output}\n")
    if status is None:
        return False, f"descriptor probe did not exit within 60s; see {log}"
    if status != 0 or "fd-primitives-probe: FAIL" in output:
        return False, f"descriptor probe failed (exit {status}); see {log}"
    expected = (
        "eventfd-counter-errors", "eventfd-semaphore", "eventfd-poll-epoll",
        "eventfd-dup-fork-blocking", "timerfd-errors",
        "timerfd-monotonic-relative-absolute-readiness",
        "timerfd-realtime-relative-absolute-readiness",
        "timerfd-gettime-interval-disarm-rearm", "timerfd-dup-fork-blocking",
        "descriptor-flags-exec", "timerfd-alarm-quota", "timerfd-refresh-poll",
    )
    lines = output.splitlines()
    missing = [name for name in expected if f"fd-primitives-probe: pass {name}" not in lines]
    if missing or "fd-primitives-probe: PASS" not in lines:
        return False, f"incomplete descriptor results (missing {missing}); see {log}"
    return True, "eventfd/timerfd I/O, poll/epoll, dup/fork and flags across exec passed; signalfd deferred"


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


def test_graphics_bounds(vm, args):
    """Logs in as `user` on the serial console and runs graphics-bounds-probe: mappings past the end of a dumb buffer, buffers beyond the display driver's limits and an unknown buffer number are refused, a fresh display handle still creates and maps a buffer, and the display scheme is still listed afterwards."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    results = {}
    probe = " ".join(["graphics-bounds-probe", *args.display])
    for tag, line in (("probe", probe), ("after", 'ls /scheme | grep -q "^display[.]"')):
        since = vm.mark()
        # The status is echoed inside bash: `user`'s login shell is not POSIX.
        vm.type(f"bash -c '{line}; echo gb-end-{tag}-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)gb-end-{tag}-(\d+)", 120, since)
        results[tag] = (int(found.group(2)) if found else None, found.group(1) if found else vm.text(since))
    (vm.out / "graphics-bounds.txt").write_text("".join(f"[{tag}] exit {status}\n{output}\n"
                                                        for tag, (status, output) in results.items()))
    lines = re.findall(r"graphics-bounds-probe: (?:pass|FAIL) .*", results["probe"][1])
    detail = "; ".join(line.strip() for line in lines) or "no result line"
    if results["probe"][0] != 0:
        return False, f"probe exit {results['probe'][0]}: {detail}; see {vm.out / 'graphics-bounds.txt'}"
    if results["after"][0] != 0:
        return False, f"no display scheme in /scheme afterwards (exit {results['after'][0]}); see {vm.out}"
    return True, detail


def test_scheme_short_read(vm, args):
    """Logs in as root on the serial console (a `user` login may not open the scheme-creation capability) and runs scheme-short-read-probe: reads its own scheme answers to a forked child with fewer bytes than asked leave every byte of the caller's buffer past the reply unchanged, at offsets inside a page and across page boundaries, and the shell can still run a program afterwards."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    since = vm.mark()
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20, since):
        return False, f"no password prompt for root; see {vm.out}"
    vm.type("password\n")
    if not vm.wait_serial(r"root:.*# ", 20, since):
        return False, f"no root shell; see {vm.out}"
    results = {}
    for tag, line in (("probe", "scheme-short-read-probe"), ("after", "ls / >/dev/null")):
        since = vm.mark()
        vm.type(f"bash -c '{line}; echo ssr-end-{tag}-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)ssr-end-{tag}-(\d+)", 60, since)
        results[tag] = (int(found.group(2)) if found else None, found.group(1) if found else vm.text(since))
    log = vm.out / "scheme-short-read.txt"
    log.write_text("".join(f"[{tag}] exit {status}\n{output}\n" for tag, (status, output) in results.items()))
    lines = re.findall(r"scheme-short-read-probe: (?:pass|FAIL|PASS).*", results["probe"][1])
    failed = [line.strip() for line in lines if "FAIL" in line]
    passed = any(line.strip() == "scheme-short-read-probe: PASS" for line in lines)
    if results["probe"][0] != 0 or not passed:
        return False, f"probe exit {results['probe'][0]}: {'; '.join(failed) or 'no PASS line'}; see {log}"
    if results["after"][0] != 0:
        return False, f"the shell could not run `ls /` afterwards (exit {results['after'][0]}); see {log}"
    return True, f"{sum('pass ' in line for line in lines)} short-read cases left the caller's bytes untouched"


def test_event_poll(vm, args):
    """Logs in as `user` on the serial console and runs event-poll-probe: zero-timeout polls of 200 empty pipes, kept open, do not grow the kernel's event subscriber count in /scheme/sys/stat."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    # The status is echoed inside bash: `user`'s login shell is not POSIX.
    vm.type("bash -c 'event-poll-probe; echo ep-end-$?'\n")
    found = vm.wait_serial(r"(?s)\n(.*)ep-end-(\d+)", 60, since)
    status, output = (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))
    log = vm.out / "event-poll.txt"
    log.write_text(f"exit {status}\n{output}\n")
    line = re.search(r"event-poll-probe: (?:PASS|FAIL).*", output)
    detail = line.group(0).strip() if line else "no result line"
    if status != 0:
        return False, f"probe exit {status}: {detail}; see {log}"
    return True, detail


@dataclass(frozen=True)
class Answered:
    status: int
    out: str

    def __iter__(self):
        yield self.status
        yield self.out

    def __getitem__(self, index):
        return (self.status, self.out)[index]


@dataclass(frozen=True)
class NoAnswer:
    out: str

    def __iter__(self):
        yield None
        yield self.out

    def __getitem__(self, index):
        return (None, self.out)[index]


def refused(reply):
    return isinstance(reply, Answered) and reply.status != 0


def guest_run(vm, tag, line, timeout):
    """Runs a line through bash on the serial console; returns Answered(status, out) or NoAnswer(out)."""
    seq = vm._next_cmd_seq()
    since = vm.mark()
    # The status is echoed inside bash: `user`'s login shell is not POSIX.
    vm.type(f"bash -c '{line}; echo {tag}-{seq}-end-$?'\n")
    found = vm.wait_serial(rf"(?s)\n(.*){re.escape(tag)}-{seq}-end-(\d+)", timeout, since)
    if not found:
        vm.console_desynced = f"{tag}: {line}"
        return NoAnswer(vm.text(since))
    return Answered(int(found.group(2)), found.group(1))


def test_foreign_call(vm, args):
    """Linux layer LC1: as `user`, foreign-call-probe checks the kernel's foreign-call hook (bad registrations refused, registers, flags, XMM0 and FS/GS intact, per-thread, cleared by exec, native signals at the boundary), then `linux-run` runs linux-probe, a C-library-free Linux program checking FS, XMM and register crossings, Linux signal frames, the alternate stack, masking and clone. Both print their call costs."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    results = {}
    for tag, line in (("fcp", "foreign-call-probe"),
                      ("lp", "linux-run --native /usr/share/foreign-call-probe/linux-probe")):
        results[tag] = guest_run(vm, tag, line, 180)
    log = vm.out / "foreign-call.txt"
    log.write_text("".join(f"[{tag}] exit {status}\n{output}\n" for tag, (status, output) in results.items()))
    lines = re.findall(r"(?:foreign-call-probe|linux-probe): (?:pass|FAIL|bench) .*", results["fcp"][1] + results["lp"][1])
    failed = [line.strip() for line in lines if ": FAIL " in line]
    bench = "; ".join(line.split(": bench ", 1)[1].strip() for line in lines if ": bench " in line)
    problems = [f"{name} exit {results[tag][0]}" for tag, name in (("fcp", "foreign-call-probe"), ("lp", "linux-probe"))
                if results[tag][0] != 0]
    if problems or failed:
        return False, f"{', '.join(problems)}: {'; '.join(failed) or 'no FAIL line'}; see {log}"
    return True, f"{sum(': pass ' in line for line in lines)} checks passed; {bench}"


LINUX_HELLOS = ["hello-raw", "hello-musl-static", "hello-glibc-static", "hello-go"]


def test_linux_hello(vm, args):
    """Linux layer LC1: the four pinned static Linux hello programs (hash-checked in their recipe stage first) print `hello from linux`, write nothing to stderr and exit 0 under `linux-run`; run directly, as a negative control, none of them does that (a hang is killed after 5 s); a bare ELF32 header is refused with ENOEXEC. With --make QEMU_CPU=core2duo (no FSGSBASE) linux-run must instead refuse to start, saying why."""
    stage = ROOT / "recipes/tests/linux-hello-fixtures/target" / f"{args.arch}-unknown-redox" / "stage/usr/share/linux-hello"
    sums = subprocess.run(["sha256sum", "-c", "SHA256SUMS"], cwd=stage, capture_output=True, text=True)
    if sums.returncode != 0:
        return False, f"fixture hashes in {stage} do not match: {sums.stdout.strip()} {sums.stderr.strip()}"
    no_fsgsbase = any(m.replace(" ", "") == "QEMU_CPU=core2duo" for m in args.make)
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    d = "/usr/share/linux-hello"
    report, problems, log = [], [], []
    for name in LINUX_HELLOS:
        status, out = guest_run(vm, f"lr{len(log)}",
                                f"linux-run --native {d}/{name} >/tmp/lh.out 2>/tmp/lh.err; s=$?; "
                                "[ \"$(cat /tmp/lh.out; echo x)\" = \"$(printf \"hello from linux\\nx\")\" ] && o=same || o=differs; "
                                "[ -s /tmp/lh.err ] && e=written || e=empty; "
                                "echo lh-result $s $o $e; head -c 300 /tmp/lh.err; (exit $s)", 120)
        log.append(f"[linux-run {name}] exit {status}\n{out}\n")
        found = re.search(r"lh-result (\d+) (same|differs) (empty|written)", out)
        verdict = found.groups() if found else ("none", "none", "none")
        if no_fsgsbase:
            if "cannot run Linux programs on this machine" not in out or verdict[0] != "126":
                problems.append(f"{name}: no FSGSBASE refusal ({verdict})")
        elif verdict != ("0", "same", "empty"):
            problems.append(f"{name} under linux-run: status {verdict[0]}, stdout {verdict[1]}, stderr {verdict[2]}")
        report.append(f"{name} {verdict[0]}")
    if not no_fsgsbase:
        for name in LINUX_HELLOS:
            status, out = guest_run(vm, f"dx{len(log)}",
                                    f"{d}/{name} >/tmp/dx.out 2>&1 & p=$!; for i in $(seq 10); do sleep 0.5; "
                                    "kill -0 $p 2>/dev/null || break; done; kill -9 $p 2>/dev/null && k=killed || k=ended; "
                                    "wait $p; s=$?; [ \"$(cat /tmp/dx.out; echo x)\" = \"$(printf \"hello from linux\\nx\")\" ] "
                                    "&& o=same || o=differs; echo dx-result $s $o $k; head -c 300 /tmp/dx.out | od -c | head -5", 60)
            log.append(f"[direct {name}] exit {status}\n{out}\n")
            found = re.search(r"dx-result (\d+) (same|differs) (killed|ended)", out)
            if not found:
                problems.append(f"direct {name}: no result")
            elif found.group(1) == "0" and found.group(2) == "same":
                problems.append(f"direct {name} printed the expected text and exited 0: the control proves nothing")
            else:
                report.append(f"direct {name}: status {found.group(1)}, output {found.group(2)}, {found.group(3)}")
        status, out = guest_run(vm, "e32", "printf \"\\177ELF\\001\\001\\001\" >/tmp/e32; head -c 64 /dev/zero >>/tmp/e32; "
                                "linux-run --native /tmp/e32", 60)
        log.append(f"[elf32] exit {status}\n{out}\n")
        if status != 126 or "ENOEXEC" not in out:
            problems.append(f"ELF32 header: exit {status}, no ENOEXEC")
        else:
            report.append("ELF32 refused with ENOEXEC")
    (vm.out / "linux-hello.txt").write_text("".join(log))
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'linux-hello.txt'}"
    return True, "; ".join(report)


def test_linux_busybox(vm, args):
    """Linux layer LC2: Debian's static busybox (hash-checked in its recipe stage first) runs the LC2 script under `linux-run` (`busybox sh lc2.sh`: a loop, pipelines, a subshell, a here-document, file commands, a trap, background jobs) and its stdout, stderr and status equal what Linux gave; `stty` under busybox turns echo off and back through TCGETS/TCSETS on the serial terminal; `linux-probe lc2` checks asynchronous signals (spinning, blocked with and without SA_RESTART, tgkill to a spinning thread), a fault as SIGSEGV with a frame and as the default action, fork, vfork, wait4 statuses, execve of /proc/self/exe, pipe2 and dup2. Last, the launcher of `sh -c "sleep 30 & sleep 31"` is killed and the Linux processes still running are counted, which LC2's result records."""
    stage = ROOT / "recipes/tests/linux-busybox-fixtures/target" / f"{args.arch}-unknown-redox" / "stage/usr/share/linux-busybox"
    sums = subprocess.run(["sha256sum", "-c", "SHA256SUMS"], cwd=stage, capture_output=True, text=True)
    if sums.returncode != 0:
        return False, f"fixture hashes in {stage} do not match: {sums.stdout.strip()} {sums.stderr.strip()}"
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    d = "/usr/share/linux-busybox"
    report, problems, log = [], [], []
    same = lambda a, b: f"[ \"$(cat {a}; echo x)\" = \"$(cat {b}; echo x)\" ]"
    status, out = guest_run(vm, "bb", f"cd /tmp && EXPECT_UID=$UID linux-run --native {d}/busybox sh {d}/lc2.sh >/tmp/bb.out 2>/tmp/bb.err </dev/null; "
                            "echo $? >/tmp/bb.st; "
                            f"{same('/tmp/bb.out', d + '/expected.stdout')} && o=same || o=differs; "
                            f"{same('/tmp/bb.err', d + '/expected.stderr')} && e=same || e=differs; "
                            f"{same('/tmp/bb.st', d + '/expected.status')} && s=same || s=differs; "
                            "echo bb-result $o $e $s; echo ==out; cat /tmp/bb.out; echo ==err; head -c 1500 /tmp/bb.err; echo ==st; cat /tmp/bb.st", 300)
    log.append(f"[lc2.sh] exit {status}\n{out}\n")
    found = re.search(r"bb-result (same|differs) (same|differs) (same|differs)", out)
    verdict = found.groups() if found else ("none", "none", "none")
    if verdict != ("same", "same", "same"):
        problems.append(f"lc2.sh: stdout {verdict[0]}, stderr {verdict[1]}, status {verdict[2]}")
    else:
        report.append("lc2.sh output equals Linux's")
    status, out = guest_run(vm, "tty", f"linux-run --native {d}/busybox sh {d}/tty.sh", 120)
    log.append(f"[tty.sh] exit {status}\n{out}\n")
    if "echo off" not in out or "termios ok" not in out:
        problems.append("stty under busybox: no `echo off` and `termios ok`")
    else:
        report.append("termios round trip")
    status, out = guest_run(vm, "lp2", "linux-run --native /usr/share/foreign-call-probe/linux-probe lc2", 300)
    log.append(f"[linux-probe lc2] exit {status}\n{out}\n")
    lines = re.findall(r"linux-probe: (?:pass|FAIL) .*", out)
    failed = [line.strip() for line in lines if ": FAIL " in line]
    if status != 0 or failed or not lines:
        problems.append(f"linux-probe lc2 exit {status}: {'; '.join(failed) or 'no FAIL line'}")
    else:
        report.append(f"linux-probe lc2 {len(lines)} passed")
    status, out = guest_run(vm, "kl", f"linux-run --native {d}/busybox sh -c \"sleep 30 & sleep 31\" & l=$!; sleep 3; "
                            "echo kl-before $(grep -c -E \"/usr/bin/linux-run|/runtime/linux-\" /scheme/sys/context); kill -9 $l; sleep 3; "
                            "echo kl-after $(grep -c -E \"/usr/bin/linux-run|/runtime/linux-\" /scheme/sys/context); grep linux- /scheme/sys/context", 120)
    log.append(f"[kill launcher] exit {status}\n{out}\n")
    found = re.search(r"kl-before (\d+).*kl-after (\d+)", out, re.S)
    report.append(f"killed launcher: linux-run processes {found.group(1)} before, {found.group(2)} after" if found
                  else "killed launcher: no count")
    (vm.out / "linux-busybox.txt").write_text("".join(log))
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'linux-busybox.txt'}"
    return True, "; ".join(report)


def test_linux_dynamic(vm, args):
    """Linux layer LC3: as `user`, `linux-run` starts a Linux session (linux-sessiond gives it the pinned Debian root read-only and a memory file system) and runs Debian's dash on the LC3 fixture (hash-checked in its recipe stage first), fed on standard input: the dynamic glibc hello, the four-thread program, a posix_spawn of a missing file that must report ENOENT, the session's checks (no DISPLAY, no /tmp/.X11-unix, no /lib/ld-linux.so.2, a write into /usr refused) and the core tools script over 30 tools with find, grep, tar and gzip. Its stdout, stderr and status must equal what Linux gave. Then, from the native login, `ls /scheme` must show neither `linux-root` nor `linux-mem`."""
    stage = ROOT / "recipes/tests/linux-lc3-fixtures/target" / f"{args.arch}-unknown-redox" / "stage/usr/share/linux-lc3"
    sums = subprocess.run(["sha256sum", "-c", "--ignore-missing", "SHA256SUMS"], cwd=stage, capture_output=True, text=True)
    if sums.returncode != 0:
        return False, f"fixture hashes in {stage} do not match: {sums.stdout.strip()} {sums.stderr.strip()}"
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    d = "/usr/share/linux-lc3"
    same = lambda a, b: f"[ \"$(cat {a}; echo x)\" = \"$(cat {b}; echo x)\" ]"
    status, out = guest_run(vm, "ld", "cd /tmp && linux-run /bin/dash -c "
                            "\"mkdir /tmp/f && tar -xf - -C /tmp/f && /bin/dash /tmp/f/lc3.sh\" "
                            f"<{d}/fixtures.tar >/tmp/ld.out 2>/tmp/ld.err; echo $? >/tmp/ld.st; "
                            f"{same('/tmp/ld.out', d + '/expected.stdout')} && o=same || o=differs; "
                            f"{same('/tmp/ld.err', d + '/expected.stderr')} && e=same || e=differs; "
                            f"{same('/tmp/ld.st', d + '/expected.status')} && s=same || s=differs; "
                            "echo ld-result $o $e $s; echo ==out; cat /tmp/ld.out; echo ==err; head -c 3000 /tmp/ld.err; "
                            "echo ==st; cat /tmp/ld.st; echo ==schemes; ls /scheme", 600)
    (vm.out / "linux-dynamic.txt").write_text(out)
    found = re.search(r"ld-result (same|differs) (same|differs) (same|differs)", out)
    verdict = found.groups() if found else ("none", "none", "none")
    problems = []
    if verdict != ("same", "same", "same"):
        problems.append(f"lc3 script: stdout {verdict[0]}, stderr {verdict[1]}, status {verdict[2]}")
    schemes = out.split("==schemes", 1)[-1]
    if re.search(r"^linux-(root|mem)$", schemes, re.M):
        problems.append("a session file system is listed in the native /scheme")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'linux-dynamic.txt'}"
    return True, "dynamic hello, threads, posix_spawn ENOENT, session checks and core tools equal Linux's; root not in native /scheme"


def test_linux_python(vm, args):
    """Linux layer LC4: as `user`, `linux-run` (a Linux session by default) runs Debian's dash on the LC4 fixture (hash-checked in its recipe stage first), fed on standard input: Debian's python3 from the root runs a one-liner, then lc4.py (a thread serving a loopback TCP socket with sendto/recvfrom, recvmsg, sendmmsg and socket options; epoll and poll; mremap; pwrite, fsync, fdatasync, dup, close_range, symlink, listxattr; fcntl and SQLite locks that hold across processes; a child process), then `python3 -m venv` and an offline `pip install` of the six wheel, and the venv's Python imports six. Its stdout, stderr and status must equal what Linux gave. Then the launcher of a session running `sh -c "sleep 30 & sleep 31"` is killed with SIGKILL, and no process of the session may remain."""
    stage = ROOT / "recipes/tests/linux-lc4-fixtures/target" / f"{args.arch}-unknown-redox" / "stage/usr/share/linux-lc4"
    sums = subprocess.run(["sha256sum", "-c", "--ignore-missing", "SHA256SUMS"], cwd=stage, capture_output=True, text=True)
    if sums.returncode != 0:
        return False, f"fixture hashes in {stage} do not match: {sums.stdout.strip()} {sums.stderr.strip()}"
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    d = "/usr/share/linux-lc4"
    same = lambda a, b: f"[ \"$(cat {a}; echo x)\" = \"$(cat {b}; echo x)\" ]"
    problems, report, log = [], [], []
    status, out = guest_run(vm, "py", "cd /tmp && linux-run /bin/dash -c "
                            "\"mkdir /tmp/f && tar -xf - -C /tmp/f && /bin/dash /tmp/f/lc4.sh\" "
                            f"<{d}/fixtures.tar >/tmp/py.out 2>/tmp/py.err; echo $? >/tmp/py.st; "
                            f"{same('/tmp/py.out', d + '/expected.stdout')} && o=same || o=differs; "
                            f"{same('/tmp/py.err', d + '/expected.stderr')} && e=same || e=differs; "
                            f"{same('/tmp/py.st', d + '/expected.status')} && s=same || s=differs; "
                            "echo py-result $o $e $s; echo ==out; cat /tmp/py.out; echo ==err; head -c 3000 /tmp/py.err; "
                            "echo ==st; cat /tmp/py.st", 900)
    log.append(f"[lc4.sh] exit {status}\n{out}\n")
    found = re.search(r"py-result (same|differs) (same|differs) (same|differs)", out)
    verdict = found.groups() if found else ("none", "none", "none")
    if verdict != ("same", "same", "same"):
        problems.append(f"lc4 script: stdout {verdict[0]}, stderr {verdict[1]}, status {verdict[2]}")
    else:
        report.append("python3, venv and pip output equals Linux's")
    status, out = guest_run(vm, "kl", "linux-run /bin/dash -c \"sleep 30 & sleep 31\" & l=$!; sleep 5; "
                            "echo kl-before $(grep -c -E \"/usr/bin/linux-run|/runtime/linux-\" /scheme/sys/context); kill -9 $l; sleep 3; "
                            "echo kl-after $(grep -c -E \"/usr/bin/linux-run|/runtime/linux-\" /scheme/sys/context); grep linux- /scheme/sys/context", 120)
    log.append(f"[kill launcher] exit {status}\n{out}\n")
    found = re.search(r"kl-before (\d+).*kl-after (\d+)", out, re.S)
    if not found or int(found.group(1)) < 2 or int(found.group(2)) != 0:
        problems.append("killed launcher: " + (f"linux-run processes {found.group(1)} before, {found.group(2)} after" if found else "no count"))
    else:
        report.append(f"killed launcher took its session's {found.group(1)} processes with it")
    (vm.out / "linux-python.txt").write_text("".join(log))
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'linux-python.txt'}"
    return True, "; ".join(report)


def test_linux_net(vm, args):
    """Linux layer LC5: as `user`, `linux-run` runs Debian's dash on the LC5 fixture (hash-checked in its recipe stage first) twice. Without the network grant, curl and Python's urllib fail to reach https://example.com/ and a connect to an Internet address fails with ENETUNREACH; with `linux-run --network`, both fetch the page over HTTPS and print its title, and a non-blocking connect reports SO_ERROR 0. Both runs also check eventfd and socket options. Each run's stdout, stderr and status must equal what Linux gave in Podman over the same root."""
    stage = ROOT / "recipes/tests/linux-lc5-fixtures/target" / f"{args.arch}-unknown-redox" / "stage/usr/share/linux-lc5"
    sums = subprocess.run(["sha256sum", "-c", "--ignore-missing", "SHA256SUMS"], cwd=stage, capture_output=True, text=True)
    if sums.returncode != 0:
        return False, f"fixture hashes in {stage} do not match: {sums.stdout.strip()} {sums.stderr.strip()}"
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    d = "/usr/share/linux-lc5"
    same = lambda a, b: f"[ \"$(cat {a}; echo x)\" = \"$(cat {b}; echo x)\" ]"
    problems, report, log = [], [], []
    for mode, flag in (("offline", ""), ("online", "--network ")):
        status, out = guest_run(vm, mode[:3], f"cd /tmp && linux-run {flag}/bin/dash -c "
                                f"\"mkdir /tmp/f && tar -xf - -C /tmp/f && /bin/dash /tmp/f/lc5.sh {mode}\" "
                                f"<{d}/fixtures.tar >/tmp/{mode}.out 2>/tmp/{mode}.err; echo $? >/tmp/{mode}.st; "
                                f"{same(f'/tmp/{mode}.out', f'{d}/{mode}.stdout')} && o=same || o=differs; "
                                f"{same(f'/tmp/{mode}.err', f'{d}/{mode}.stderr')} && e=same || e=differs; "
                                f"{same(f'/tmp/{mode}.st', f'{d}/{mode}.status')} && s=same || s=differs; "
                                f"echo {mode}-result $o $e $s; echo ==out; cat /tmp/{mode}.out; echo ==err; head -c 3000 /tmp/{mode}.err; "
                                f"echo ==st; cat /tmp/{mode}.st", 600)
        log.append(f"[{mode}] exit {status}\n{out}\n")
        found = re.search(rf"{mode}-result (same|differs) (same|differs) (same|differs)", out)
        verdict = found.groups() if found else ("none", "none", "none")
        if verdict != ("same", "same", "same"):
            problems.append(f"{mode}: stdout {verdict[0]}, stderr {verdict[1]}, status {verdict[2]}")
        else:
            report.append(f"{mode} output equals Linux's")
    (vm.out / "linux-net.txt").write_text("".join(log))
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'linux-net.txt'}"
    return True, "; ".join(report)


def test_sudo_notty(vm, args):
    """Issue 10: as `user` on the serial console, `sudo true` with standard input, output and error off the terminal does not panic and exits 0, since `user`'s password is empty (config/dev-accounts.toml)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    # Run in bash: `user`'s login shell is not POSIX. The end marker is computed so the typed line never matches it.
    vm.type("bash -c 'sudo true > /tmp/x 2>&1 < /dev/null; echo rc=$?; cat /tmp/x; echo sudo-notty-end-$((40+2))'\n")
    found = vm.wait_serial(r"(?s)\nrc=(\d+)\n(.*)sudo-notty-end-42", 60, since)
    output = vm.text(since)
    (vm.out / "sudo-notty.txt").write_text(output)
    if not found:
        return False, f"no exit status within 60s; see {vm.out / 'sudo-notty.txt'}"
    status, said = int(found.group(1)), found.group(2).strip()
    if "panicked" in said:
        return False, f"sudo panicked (exit {status}): {said}"
    if status != 0:
        return False, f"sudo refused although `user`'s password is empty (exit {status}): {said or 'no output'}"
    return True, "sudo true ran without a terminal and exited 0" + (f": {said}" if said else "")


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


GTK_SESSION_WAIT = 300
GTK_DISPLAY_WAIT = 360


def test_gtk_window(vm, args, *, backend="orbital"):
    """Checks a GTK3 Orbital window's blue background, white label and timed exit (use --config firefox-window)."""
    if backend == "orbital":
        while vm.elapsed() < args.timeout:
            if vm.screenshot().kind() == "login":
                break
            time.sleep(0.1)
        else:
            return False, "no graphical login"
        vm.command("sendkey ret")
        settle(vm, 20)
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, "no serial login"
    timings = {"serial_login_s": round(vm.elapsed(), 1)}
    login_mark = vm.mark()
    vm.type("root\n" if backend == "wayland" else "user\n")
    if backend == "wayland":
        if not vm.wait_serial(r"(?i)password: ", 20):
            return False, "no root password prompt"
        vm.type("password\n")
    prompt = r"root:.*# " if backend == "wayland" else r"(?m)^> "
    if not vm.wait_serial(prompt, 20, login_mark):
        return False, "serial shell did not become ready"
    since = vm.mark()
    vm.type("bash -c 'echo GTK-SHELL-$((40+2))'\n")
    if not vm.wait_serial(r"\nGTK-SHELL-42\s", 20, since):
        return False, "no serial shell"

    def record():
        (vm.out / "gtk-window-timings.json").write_text(json.dumps(timings, indent=2) + "\n")

    # Serial logins do not inherit the compositor's graphical environment.
    if backend == "wayland":
        since = vm.mark()
        vm.type("bash -c 'until test -f /tmp/gtk-window-session; do sleep 1; done; echo GTK-SESSION-$((40+2))'\n")
        if not vm.wait_serial(r"\nGTK-SESSION-42\s", GTK_SESSION_WAIT, since):
            record()
            return False, f"cosmic-comp wrote no session file within {GTK_SESSION_WAIT}s of the shell"
        timings["session_file_s"] = round(vm.elapsed(), 1)
        environment = ". /tmp/gtk-window-session; GDK_BACKEND=wayland "
    else:
        environment = "GDK_BACKEND=orbital ORBITAL_DISPLAY=/scheme/orbital "
    since, started = vm.mark(), time.monotonic()
    vm.type(f"bash -c '{environment}gtk-window-probe; echo GTKRC-$((40+2))=$?'\n")
    opened = vm.wait_serial(r"GTK-WINDOW: display=(\S+) type=(\S+)", GTK_DISPLAY_WAIT, since)
    if not opened:
        record()
        vm.screenshot().save_png(vm.out / "gtk-window-failed.png")
        return False, f"GTK did not open a display within {GTK_DISPLAY_WAIT}s; see serial.log"
    timings["display_s"] = round(time.monotonic() - started, 1)
    record()
    if opened.group(2) != {"orbital": "GdkOrbitalDisplay", "wayland": "GdkWaylandDisplay"}[backend]:
        return False, f"wrong GTK backend: {opened.group(2)}"

    # Find the independent fixture color, then check its rectangle and the
    # contrasting label inside it. A blank GTK shell or unpainted buffer fails.
    deadline = time.monotonic() + 6
    # Orbital's compositor rounds colors down; nesting adds another blend.
    blue = {bytes((r, g, b)) for r in range(0x24 - 2, 0x24 + 1)
            for g in range(0x68 - 2, 0x68 + 1) for b in range(0xAC - 2, 0xAC + 1)}
    matched, counts = False, None
    while time.monotonic() < deadline:
        frame = vm.screenshot()
        points = [i // 3 for i in range(0, len(frame.rgb), 3)
                  if frame.rgb[i:i + 3] in blue]
        if points:
            xs = [p % frame.width for p in points]
            ys = [p // frame.width for p in points]
            left, top, right, bottom = min(xs), min(ys), max(xs), max(ys)
            width, height = right - left + 1, bottom - top + 1
            white = sum(min(frame.rgb[(y * frame.width + x) * 3:(y * frame.width + x) * 3 + 3]) >= 253
                        for y in range(top + height // 4, top + 3 * height // 4)
                        for x in range(left + width // 8, left + 7 * width // 8))
            counts = {"blue_pixels": len(points), "white_label_pixels": white,
                      "bounds": [left, top, right, bottom]}
            # GTK's default size is a request: client-side decorations can
            # enlarge it for the title and inset the colored area by a border.
            matched = (width >= 350 and height >= 190 and len(points) > 65000
                       and len(points) > width * height * 0.9 and white > 100)
            if matched:
                break
        time.sleep(0.1)
    frame.save_png(vm.out / "gtk-window.png")
    (vm.out / "gtk-window-pixels.json").write_text(json.dumps(counts, indent=2) + "\n")
    exited = vm.wait_serial(r"\nGTKRC-42=(\d+)\s", 15, since)
    if not matched:
        return False, f"GTK background/label pixels differ: {counts}; see gtk-window.png"
    if not exited or exited.group(1) != "0" or "GTK-WINDOW: closed" not in vm.text(since):
        return False, "GTK painted but did not close normally; see serial.log"
    closed = settle(vm, 5)
    closed.save_png(vm.out / "gtk-window-closed.png")
    remaining = sum(closed.rgb[i:i + 3] in blue for i in range(0, len(closed.rgb), 3))
    if remaining > 100:
        return False, f"GTK window left {remaining} blue pixels after exit"
    return True, f"{backend} GTK window painted and closed: {counts}; {timings}; see gtk-window.png"


def test_gtk_window_wayland(vm, args):
    """Checks GTK's Wayland window inside the nested cosmic-comp session (use --config firefox-wayland)."""
    return test_gtk_window(vm, args, backend="wayland")


def run_wayland_client(args, out, *, pointer_latency=False, desktop_columns=False):
    """Checks the DD1 pixman compositor with a cursor-capable virtio display and QMP input."""
    with tempfile.TemporaryDirectory(prefix="ferrion-dd-qmp-") as directory:
        path = Path(directory) / "qmp.sock"
        boot_load = os.getloadavg()
        monitor = None
        if desktop_columns:
            from desktop_load import HostLoadMonitor

            monitor = HostLoadMonitor(out)
        vm = None
        try:
            vm = VM(args, out, qemu_flags=f"-qmp unix:{path},server=on,wait=off", make=("gpu=virtio",))
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(args.timeout)
                connection.connect(str(path))
                with connection.makefile("rwb") as stream:
                    greeting = json.loads(stream.readline())
                    if "QMP" not in greeting:
                        raise RuntimeError("QEMU sent no QMP greeting")

                    def qmp(command, arguments=None):
                        stream.write(json.dumps({"execute": command, "arguments": arguments or {}}).encode() + b"\n")
                        stream.flush()
                        while True:
                            reply = json.loads(stream.readline())
                            if "error" in reply:
                                raise RuntimeError(f"QMP {command}: {reply['error']}")
                            if "return" in reply:
                                return reply["return"]

                    qmp("qmp_capabilities")
                    if desktop_columns:
                        return test_desktop_columns(vm, args, qmp, boot_load)
                    ok, note = test_wayland_client(vm, args, qmp, boot_load, pointer_latency=pointer_latency)
                    if not pointer_latency and out.name == f"wayland-client-{args.repeat}":
                        measurements = [
                            json.loads((out.parent / f"wayland-client-{run}" / "measurements.json").read_text())
                            for run in range(1, args.repeat + 1)
                        ]
                        completions = sum(r["pointers_completed_during_present"] for r in measurements)
                        (out.parent / "wayland-client-series.json").write_text(json.dumps({
                            "boots": args.repeat, "pointers_completed_during_present": completions,
                            "independent_cursor_dispatch": completions > 0,
                        }, indent=2) + "\n")
                        if not completions:
                            return False, note + "; no cursor operation completed inside a frame interval in this series"
                        note += f"; series has {completions} cursor completions inside frame intervals"
                    return ok, note
        finally:
            if vm is not None:
                vm.close()
            if monitor is not None:
                monitor.close()

def run_pointer_latency(args, out):
    return run_wayland_client(args, out, pointer_latency=True)



def test_wayland_client(vm, args, qmp, boot_load, *, pointer_latency=False):
    """Checks a native-VT pixman Wayland client and input while a slow client renders."""
    import statistics
    import math

    problems = []
    host_load = [{"phase": "boot", "elapsed_s": 0, "load_average": boot_load}]
    injections = []

    def sample_load(phase):
        sample = {"phase": phase, "elapsed_s": vm.elapsed(), "load_average": os.getloadavg()}
        host_load.append(sample)
        return sample

    for marker in ("DD1_COMPOSITOR_READY", "DD1_CLIENT_READY", "DD1_SLOW_CLIENT_READY"):
        if not vm.wait_serial(marker, args.timeout):
            problems.append(f"missing {marker}")
            break
    sample_load("clients-ready")
    if not problems:
        for key in "abc":
            vm.command(f"sendkey {key}")
            if not vm.wait_serial(rf"DD1_KEY {key}\b", args.timeout):
                problems.append(f"key {key} did not reach the client")
        # The guest selects vmmouse's absolute device; HMP relative motion does
        # not reach it. QMP sends absolute coordinates through the real device.
        for step in range(100):
            injections.append({"step": step, "x": 1000 + step * 100, "y": 1000 + step * 100,
                               "host": sample_load("pointer-injection")})
            qmp("input-send-event", {"events": [
                {"type": "abs", "data": {"axis": "x", "value": 1000 + step * 100}},
                {"type": "abs", "data": {"axis": "y", "value": 1000 + step * 100}},
            ]})
            time.sleep(0.05)
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            text = vm.text()
            if len(re.findall(r"DD1_FRAME client=normal ", text)) >= 60:
                break
            time.sleep(0.1)
    frame = settle(vm, 5)
    frame.save_png(vm.out / "wayland-client.png")
    if (frame.width, frame.height) != (1280, 800):
        problems.append(f"display is {frame.width}x{frame.height}, expected 1280x800")
    for name, expected, points in (
        ("normal", bytes.fromhex("e68e0d"), ((100, 120), (380, 320))),
        ("slow", bytes.fromhex("3a9fd8"), ((540, 120), (820, 320))),
    ):
        for x, y in points:
            if x >= frame.width or y >= frame.height:
                problems.append(f"{name} probe ({x},{y}) outside display")
                continue
            offset = (y * frame.width + x) * 3
            actual = frame.rgb[offset:offset + 3]
            if any(abs(a - b) > 2 for a, b in zip(actual, expected)):
                problems.append(f"{name} ({x},{y}) #{actual.hex()}, expected #{expected.hex()}")
    text = vm.text()
    sample_load("capture")
    commits = {(client, int(sequence)): int(timestamp)
               for client, sequence, timestamp in re.findall(
                   r"DD1_CLIENT_COMMIT client=(normal|slow) frame=(\d+) monotonic_ns=(\d+)", text)}
    frames = []
    for client, sequence, dispatch, timestamp, present, started in re.findall(
            r"DD1_FRAME client=(normal|slow) frame=(\d+) dispatch_to_present_us=(\d+) monotonic_ns=(\d+) present_us=(\d+) present_started_us=(\d+)", text):
        committed = commits.get((client, int(sequence)))
        if committed is None or committed > int(timestamp):
            problems.append(f"{client} frame {sequence} lacks an ordered client commit timestamp")
            continue
        frames.append({"client": client, "frame": int(sequence), "commit_ns": committed,
                       "present_ns": int(timestamp), "commit_to_present_us": (int(timestamp) - committed) / 1000,
                       "dispatch_to_present_us": int(dispatch), "present_us": int(present),
                       "present_started_us": int(started)})
    pointers = [{"read_us": int(read), "completed_us": int(done), "elapsed_us": int(delay),
                 "x": int(x), "y": int(y)}
                for read, done, delay, x, y in re.findall(
                    r"DD1_POINTER read_us=(\d+) completed_us=(\d+) elapsed_us=(\d+) x=(-?\d+) y=(-?\d+)", text)]
    present_intervals = {(f["present_started_us"], f["present_us"]) for f in frames}
    for index, pointer in enumerate(pointers):
        pointer["completed_during_present"] = any(
            start <= pointer["read_us"] <= pointer["completed_us"] < end for start, end in present_intervals)
        if len(pointers) == len(injections):
            pointer["injection_step"] = index
    normal_frames = [f["commit_to_present_us"] for f in frames if f["client"] == "normal"]
    median = statistics.median(normal_frames) if normal_frames else None
    if len(normal_frames) < 60:
        problems.append(f"only {len(normal_frames)} presented normal-client commits, expected at least 60")
    if median is not None and median > 16_700:
        problems.append(f"commit-to-present median {median}us exceeds 16700us")
    if len(pointers) != 100:
        problems.append(f"{len(pointers)} cursor movements, expected exactly 100")
    if not pointers:
        problems.append("no pointer timing samples")
    pointer_problems = (["cursor read-to-plane ioctl reached or exceeded 2000us"]
                        if any(p["elapsed_us"] >= 2000 for p in pointers) else [])
    slow_frames = len(re.findall(r"DD1_FRAME_SUBMITTED client=slow frame=", text))
    if slow_frames < 2:
        problems.append("slow client did not continue rendering")
    def timing_summary(samples):
        ordered = sorted(samples)
        return ({"p50": statistics.median(ordered), "p99": ordered[math.ceil(len(ordered) * 0.99) - 1],
                 "max": ordered[-1]} if ordered else None)
    functional_problems = problems.copy()
    if pointer_latency:
        problems.extend(pointer_problems)

    result = {"renderer": "pixman", "transport": "rust", "width": frame.width, "height": frame.height,
              "frames": frames, "pointers": pointers, "frame_median_us": median,
              "slow_frames": slow_frames, "problems": problems,
              "check": "pointer-latency" if pointer_latency else "wayland-client",
              "functional_problems": functional_problems, "pointer_latency_problems": pointer_problems,
              "host_cpu_count": os.cpu_count(), "host_load": host_load, "pointer_injections": injections,
              "frame_us": timing_summary(normal_frames),
              "pointer_us": timing_summary(p["elapsed_us"] for p in pointers),
              "pointers_completed_during_present": sum(p["completed_during_present"] for p in pointers),
              "timing_scope": "client commit to display handoff via CLOCK_MONOTONIC; input read to cursor ioctl completion, not scanout"}
    (vm.out / "measurements.json").write_text(json.dumps(result, indent=2) + "\n")
    note = (f"cursor max {result['pointer_us']['max']}us across {len(pointers)} samples"
            if pointer_latency and pointers else f"pixels and abc match; median {median}us")
    return not problems, "; ".join(problems) if problems else f"{note}; see {vm.out}"


def test_shm_reuse(vm, args):
    """Checks fresh shared-file contents after repeated mapping, unmapping and unlinking."""
    if not root_shell(vm, args.timeout):
        return False, "serial root shell unavailable"
    status, output = run_status(vm, "shm-reuse-probe", max(args.timeout, 120))
    (vm.out / "probe.txt").write_text(output)
    passed = len(re.findall(r"shm-reuse-probe: PASS ", output))
    return status == 0 and passed == 8, f"{passed}/8 shared mapping cases pass; exit {status}; see {vm.out / 'probe.txt'}"


def test_desktop_columns(vm, args, qmp, boot_load):
    """Compares three windows in both layouts, recording timing with corners off/on."""
    from desktop_columns import check
    return check(vm, args, qmp, boot_load, root_shell, run_status)


def run_desktop_columns(args, out):
    """Checks ferrion-wm's layouts and per-frame timing; use --config wm-columns --repeat 10."""
    return run_wayland_client(args, out, desktop_columns=True)


def run_desktop_columns_timing(args, out):
    """Collects --repeat native desktop boots below --host-load-limit; exits BLOCKED when the wait budget expires."""
    from desktop_load import run_gated

    return run_gated(args, out, run_desktop_columns, Blocked)


def run_display_handoff(args, out):
    """Runs 60 rapid VNC display handoffs with continuous QMP pointer motion; use --config wm-columns."""
    from display_handoff import run_display_handoff as check

    return check(args, out, VM, root_shell, run_status)


def test_shm_read_back(vm, args):
    """Reads through a descriptor what a shared mapping wrote, while mapped and after munmap; size must stay."""
    if not root_shell(vm, args.timeout):
        return False, "serial root shell unavailable"
    status, output = run_status(vm, "shm-reuse-probe read-back", max(args.timeout, 120))
    (vm.out / "probe.txt").write_text(output)
    passed = len(re.findall(r"shm-reuse-probe: PASS read-back ", output))
    return status == 0 and passed == 4, f"{passed}/4 read-back cases pass; exit {status}; see {vm.out / 'probe.txt'}"


def test_desktop_regions(vm, args):
    """Compares the desktop's region probes with flat/first-run.svg within two RGB steps."""
    from desktop_fixtures import compare_probes, load_regions

    probes = [(name, region["probes"]) for name, region in load_regions("first-run.svg").items()
              if region["probes"]]

    while vm.elapsed() < args.timeout:
        if vm.screenshot().kind() == "login":
            break
        time.sleep(0.1)
    else:
        return False, f"no graphical login within {args.timeout}s"
    settle(vm, 10)
    vm.command("sendkey ret")
    settle(vm, 20, still=2.0)
    frame = vm.screenshot()
    frame.save_png(vm.out / "desktop-regions.png")
    differing = 0
    for name, points in probes:
        differences = compare_probes(frame, points)
        differing += bool(differences)
        print(f"{'FAIL' if differences else 'PASS'} {name}: "
              + ("; ".join(differences) if differences else f"{len(points)} probes match"), flush=True)
    return differing == 0, f"{differing}/{len(probes)} regions differ; see {vm.out / 'desktop-regions.png'}"


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


# Drivers that enter the null namespace once they hold their handles, with the schemes their namespace
# handles may name (ferrion-driver-null-namespace.patch of base). Matched against the basename of
# each process in /scheme/sys/iostat.
NULL_NAMESPACE = {"memory", "pipe"}
DRIVER_NAMESPACES = {name: NULL_NAMESPACE for name in [
    "pcid", "nvmed", "inputd", "ihdad", "usbhidd", "fbcond", "fbbootlogd",
    "usbscsid", "virtio-gpud", "usbhubd", "rtcd", "redoxerd", "usbctl"]}
DRIVER_NAMESPACES["ferrion-audiod"] = {"memory", "rand"}
# Running on the x86_64 desktop under `make qemu` once the login appears; the others start only with
# their device (a USB HID device, disk or hub, which `make qemu` does not attach, virtio-gpu,
# redoxerd's config), exit after one job (rtcd) or are a command (usbctl).
DRIVER_NAMESPACES_RUNNING = ["pcid", "nvmed", "inputd", "ihdad", "fbcond", "fbbootlogd", "ferrion-audiod"]
# Keep their namespace: the drivers they start register schemes in it.
DRIVER_NAMESPACE_SPAWNERS = ["pcid-spawner", "hwd", "xhcid"]
IOSTAT_PROCESS = re.compile(r"^(\d+): (\S+)$")
IOSTAT_FD = re.compile(r"^(U)?\s+(\d+):\s+(\d+)\s+(\d+)\s+[0-9A-F]+: (.*)$")
NAMESPACE_REPORT = re.compile(r"^namespace:hardware=([01]);schemes=([^;]*);$")


def read_iostat(vm, tag="iostat"):
    """Reads /scheme/sys/iostat in a root shell; returns its text, or None."""
    # cat of /scheme/sys/iostat sometimes faults; the copy in /tmp is read once it is whole.
    for attempt in range(3):
        since = vm.mark()
        vm.type(f"cat /scheme/sys/iostat > /tmp/iostat.txt && cat /tmp/iostat.txt; echo {tag}-{attempt}-$((40+2))\n")
        found = vm.wait_serial(rf"(?s)\n(.*){tag}-{attempt}-42", 120, since)
        if found and re.search(r"(?m)^\d+: \S+$", found.group(1)):
            return found.group(1)
    return None


def iostat_processes(iostat):
    """Maps each process basename in an iostat text to a list of (pid, [(fd, scheme id, fpath)])."""
    processes = {}
    current = None
    for line in iostat.splitlines():
        line = line.rstrip()
        header = IOSTAT_PROCESS.match(line)
        if header:
            current = []
            processes.setdefault(header.group(2).rsplit("/", 1)[-1], []).append((header.group(1), current))
            continue
        row = IOSTAT_FD.match(line)
        if row and current is not None:
            current.append(((row.group(1) or "") + row.group(2), row.group(3), row.group(5)))
    return processes


def test_driver_namespaces(vm, args):
    """Every namespace handle a running driver holds names only memory and pipe (ferrion-audiod: memory and rand), read from /scheme/sys/iostat; Enter on the login screen still reaches the desktop."""
    kind = None
    while vm.elapsed() < args.timeout and kind != "login":
        kind = vm.screenshot().kind()
        time.sleep(0.2)
    if kind != "login":
        return False, f"no graphical login within {args.timeout}s"
    login = settle(vm, 10)
    login.save_png(vm.out / "login.png")
    vm.command("sendkey ret")
    desktop = settle(vm, 30, still=2.0)
    desktop.save_png(vm.out / "desktop.png")
    problems = []
    if desktop.rgb == login.rgb:
        problems.append("Enter on the login screen changed nothing, so input through inputd is broken")

    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20):
        return False, "no password prompt for root"
    vm.type("password\n")
    time.sleep(3)

    iostat = read_iostat(vm)
    if iostat is None:
        return False, "/scheme/sys/iostat printed no process"
    (vm.out / "iostat.txt").write_text(iostat)
    processes = iostat_processes(iostat)
    namespace_ids = {scheme for threads in processes.values() for _, rows in threads
                     for _, scheme, path in rows if path.startswith("namespace:")}
    if not namespace_ids:
        problems.append("no handle in /scheme/sys/iostat reports a namespace (base without ferrion-driver-null-namespace.patch?)")

    checked, not_exercised = [], []
    for driver, allowed in DRIVER_NAMESPACES.items():
        if driver not in processes:
            if driver in DRIVER_NAMESPACES_RUNNING:
                problems.append(f"{driver} is not running")
            else:
                not_exercised.append(driver)
            continue
        for pid, rows in processes[driver]:
            handles = [(fd, path) for fd, scheme, path in rows if scheme in namespace_ids]
            if not handles and namespace_ids:
                problems.append(f"{driver} (pid {pid}) holds no namespace handle")
            for fd, path in handles:
                report = NAMESPACE_REPORT.match(path)
                if not report:
                    problems.append(f"{driver} (pid {pid}) fd {fd}: unreadable namespace report {path!r}")
                    continue
                schemes = set(filter(None, report.group(2).split(",")))
                if schemes != allowed:
                    problems.append(f"{driver} (pid {pid}) fd {fd} names {sorted(schemes)}, expected {sorted(allowed)}")
        checked.append(driver)
    spawners = [name for name in DRIVER_NAMESPACE_SPAWNERS if name in processes]

    note = (f"checked {checked}; not exercised (not running at the snapshot): {not_exercised}; "
            f"spawners keeping their namespace by design, running: {spawners}; see {vm.out}")
    if problems:
        return False, "; ".join(problems) + "; " + note
    return True, note


# Network schemes config/login-namespace.toml leaves out of `netless`'s namespace.
NETWORK_SCHEMES = ["ip", "icmp", "tcp", "udp"]


def test_login_namespace(vm, args):
    """On the serial console `netless` (password) sees no network scheme and `user` (no password) sees them (needs --config login-namespace)."""
    seen = {}
    since = 0
    for account, password in (("netless", "netless-test"), ("user", None)):
        if not vm.wait_serial(r"login: ", args.timeout, since):
            return False, f"no serial login prompt for {account} within {args.timeout}s"
        vm.type(f"{account}\n")
        if password:
            if not vm.wait_serial(r"(?i)password: ", 20):
                return False, f"no password prompt for {account}"
            vm.type(f"{password}\n")
        time.sleep(3)
        since = vm.mark()
        vm.type("ls /scheme; echo schemes-$((40+2))\n")
        listing = vm.wait_serial(r"(?s)\n(.*)schemes-42", 30, since)
        if not listing:
            return False, f"`ls /scheme` as {account} printed nothing; see {vm.out / 'serial.log'}"
        (vm.out / f"schemes-{account}.txt").write_text(listing.group(1))
        names = set(re.findall(r"[\w.-]+", listing.group(1)))
        seen[account] = [name for name in NETWORK_SCHEMES if name in names]
        since = vm.mark()
        vm.type("exit\n")
    problems = []
    if seen["netless"]:
        problems.append(f"netless, logged in with its password, sees {seen['netless']}")
    if seen["user"] != NETWORK_SCHEMES:
        problems.append(f"user sees only {seen['user']} of {NETWORK_SCHEMES}")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    return True, f"netless sees no network scheme and user sees {NETWORK_SCHEMES}; see {vm.out}"


# The developer tasks the dev-parity test tries, in order. Each task is a list of
# steps: (bash line, regex its output must match or None, seconds it may take).
# A task stops at its first failed step. The steps work in the guest's
# DEV_PARITY_DIR, as `user`, and use no credentials: the AI tools are installed
# and asked for their version only.
DEV_PARITY_DIR = "/tmp/dev-parity"
DEV_PARITY_TASKS = {
    "git": [
        ("git --version", r"git version \d", 30),
        (f"git clone --depth 1 https://github.com/octocat/Hello-World.git {DEV_PARITY_DIR}/hello-world", None, 300),
        (f"git -C {DEV_PARITY_DIR}/hello-world log --oneline -1", r"(?m)^[0-9a-f]{7,} ", 30),
    ],
    "rust": [
        ("rustc --version && cargo --version", r"cargo \d+\.\d+", 60),
        (f"cd {DEV_PARITY_DIR} && cargo new --vcs none hello-cargo", None, 120),
        (f"cargo build --offline --manifest-path {DEV_PARITY_DIR}/hello-cargo/Cargo.toml", None, 1200),
        (f"{DEV_PARITY_DIR}/hello-cargo/target/debug/hello-cargo", r"Hello, world!", 60),
    ],
    "python": [
        ("python3 --version", r"Python 3\.\d+", 30),
        (f"echo 'print(\"script\", 6 * 7)' > {DEV_PARITY_DIR}/hello.py && python3 {DEV_PARITY_DIR}/hello.py",
         r"script 42", 60),
        (f"python3 -m venv {DEV_PARITY_DIR}/venv", None, 300),
        (f"{DEV_PARITY_DIR}/venv/bin/pip install --no-input six==1.17.0", r"Successfully installed six-1\.17\.0", 600),
        (f"{DEV_PARITY_DIR}/venv/bin/python -c 'import six; print(\"six\", six.__version__)'", r"six 1\.17\.0", 60),
    ],
    "node": [
        ("node --version && npm --version", r"v\d+\.\d+\.\d+\s+\d+\.\d+\.\d+", 60),
        ("node -e 'console.log(\"node\", 6 * 7)'", r"node 42", 60),
        (f"mkdir -p {DEV_PARITY_DIR}/npm-small && cd {DEV_PARITY_DIR}/npm-small && npm install is-number@7.0.0",
         r"added 1 package", 600),
        (f"node -e 'console.log(\"is-number\", require(\"{DEV_PARITY_DIR}/npm-small/node_modules/is-number\")(6 * 7))'",
         r"is-number true", 60),
    ],
    # Connecting to an SSH server is not tested, by the maintainer's choice.
    "ssh": [
        ("ssh -V", r"OpenSSH_\d", 30),
        (f"ssh-keygen -q -t ed25519 -N '' -C dev-parity -f {DEV_PARITY_DIR}/id_ed25519 && "
         f"ssh-keygen -l -f {DEV_PARITY_DIR}/id_ed25519.pub", r"256 SHA256:\S+ dev-parity \(ED25519\)", 120),
    ],
    "helix": [
        ("hx --version", r"helix \d", 60),
        ("hx --health", r"(?i)\blanguage\b", 300),
    ],
    "claude-code": [
        (f"npm install -g --prefix {DEV_PARITY_DIR}/npm-global @anthropic-ai/claude-code", r"added \d+ package", 900),
        (f"{DEV_PARITY_DIR}/npm-global/bin/claude --version", r"\d+\.\d+\.\d+ \(Claude Code\)", 120),
    ],
    "codex": [
        (f"npm install -g --prefix {DEV_PARITY_DIR}/npm-global @openai/codex", r"added \d+ package", 900),
        (f"{DEV_PARITY_DIR}/npm-global/bin/codex --version", r"codex-cli \d", 120),
    ],
}
# Typed into bash once, before the tasks: quiet progress output and a fresh work folder.
DEV_PARITY_SETUP = (
    "export NO_COLOR=1 CARGO_TERM_COLOR=never CARGO_TERM_PROGRESS_WHEN=never PIP_DISABLE_PIP_VERSION_CHECK=1 "
    "npm_config_progress=false npm_config_fund=false npm_config_audit=false npm_config_update_notifier=false; "
    f"rm -rf {DEV_PARITY_DIR}; mkdir -p {DEV_PARITY_DIR} && cd {DEV_PARITY_DIR}"
)


def test_dev_parity(vm, args):
    """Logs in as `user` and tries each developer task in DEV_PARITY_TASKS (git, Rust, Python, Node.js, SSH, Helix, Claude Code, Codex), recording PASS or FAIL with a reason per task; fails only when the login fails (use --config dev-parity)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    # Whatever the user's login shell, the steps run in bash, without line editing.
    vm.type("bash --norc --noprofile --noediting\n")
    time.sleep(1)
    since = vm.mark()
    vm.type("echo dev-parity-bash-$BASH_VERSION\n")
    if not vm.wait_serial(r"dev-parity-bash-\d", 30, since):
        return False, f"bash did not start after the login; see {vm.out / 'serial.log'}"

    def run(line, timeout):
        """Types `line`; returns (exit status or None on a timeout, output, seconds taken)."""
        since, start = vm.mark(), time.monotonic()
        vm.type(f"{line}; echo DPRC-$((40+2))=$?\n")
        found = vm.wait_serial(r"(?s)\n(.*?)DPRC-42=(\d+)\s", timeout, since)
        took = time.monotonic() - start
        if found:
            return int(found.group(2)), found.group(1), took
        return None, vm.text(since).partition("\n")[2], took

    def shell_back():
        """After a timeout: Ctrl-C, then True once bash answers again."""
        vm.type("\x03")
        time.sleep(1)
        since = vm.mark()
        vm.type("echo DPSYNC-$((40+2))\n")
        return vm.wait_serial(r"DPSYNC-42", 60, since) is not None

    status, setup, _ = run(DEV_PARITY_SETUP, 60)
    address = None
    for _ in range(10):
        _, output, _ = run("cat /scheme/netcfg/ifaces/eth0/addr/list", 30)
        address = re.search(r"\b10\.0\.2\.\d+", output)
        if address:
            break
        time.sleep(3)
    network = f"eth0 {address.group(0)}" if address else "eth0 has no address from QEMU's DHCP server"
    (vm.out / "setup.txt").write_text(f"$ {DEV_PARITY_SETUP}\nexit status {status}\n{setup}\n{network}\n")

    results = []
    lost = None  # the task after which bash stopped answering
    for task, steps in DEV_PARITY_TASKS.items():
        log, reason = [], None
        if lost:
            reason = f"not run: bash did not answer after {lost} timed out"
        for number, (line, pattern, timeout) in enumerate(steps, 1):
            if reason:
                break
            status, output, took = run(line, timeout)
            log.append(f"$ {line}\n{output}\n[exit status {status}, {took:.0f}s]\n")
            last = next((text.strip() for text in reversed(output.splitlines()) if text.strip()), "no output")
            if status is None:
                reason = f"step {number} timed out after {timeout}s: {last}"
                if not shell_back():
                    lost = task
            elif status != 0:
                reason = f"step {number} exited {status}: {last}"
            elif pattern and not re.search(pattern, output):
                reason = f"step {number} printed no match for {pattern}: {last}"
        verdict = "FAIL" if reason else "PASS"
        if not reason:
            reason = f"all {len(steps)} steps passed" if len(steps) > 1 else "its step passed"
            if task == "ssh":
                reason += "; connecting to a server not tested, by the maintainer's choice"
        reason = " ".join(reason.split())[:160]
        (vm.out / f"{task}.txt").write_text("".join(log) or reason + "\n")
        results.append((task, verdict, reason))
        print(f"DEVPARITY {task} {verdict} {reason}", flush=True)

    width = max(len(task) for task, _, _ in results)
    table = "".join(f"{task:<{width}}  {verdict}  {reason}\n" for task, verdict, reason in results)
    (vm.out / "summary.txt").write_text(f"{network}\n{table}")
    print(table, end="", flush=True)
    passed = sum(verdict == "PASS" for _, verdict, _ in results)
    return True, f"ran {len(results)} tasks, {passed} PASS; {network}; see {vm.out / 'summary.txt'}"


# The pages the firefox-headless test renders, in order: (name, URL, seconds the render may take).
# Software rendering with the JIT off is slow, so the renders get minutes.
FIREFOX_DIR = "/tmp/firefox-headless"
FIREFOX_PAGES = [
    ("local", f"file://{FIREFOX_DIR}/page.html", 600),
    ("https", "https://example.com/", 900),
]


def enter_shell(vm, command, prompt, timeout):
    since = vm.mark()
    vm.type(command + "\n")
    return bool(vm.wait_serial(prompt, timeout, since))


def user_bash(vm, timeout):
    if not vm.wait_serial(r"login: ", timeout):
        return False
    if not enter_shell(vm, "user", r"(?:user:[^\r\n]*\$|bash-[0-9.]+\$|>) ", timeout):
        return False
    return enter_shell(vm, "bash --norc --noprofile --noediting", r"bash-[0-9.]+\$ ", timeout)


def x2apic_boot(args, out, expected_entries, fixture=None):
    out.mkdir(parents=True, exist_ok=True)
    initial_cpus = len(expected_entries) - 1
    make = (f"QEMU_SMP={initial_cpus},maxcpus=258,sockets=2,cores=129,threads=1",
            "QEMU_MACHINE=q35,kernel-irqchip=split", "QEMU_MEM=2048")
    flags = "-device intel-iommu,intremap=on,eim=on -device host-x86_64-cpu,socket-id=1,core-id=0,thread-id=0"
    if fixture is not None:
        flags += " -acpitable file=" + shlex.quote(str(fixture.resolve()))
    vm = VM(args, out, make=make, qemu_flags=flags)
    try:
        if not root_shell(vm, args.timeout):
            return False, "no root prompt after x2APIC boot"
        if not enter_shell(vm, "bash --norc --noprofile --noediting", r"bash-[0-9.]+# ", args.timeout):
            return False, "no root Bash prompt"
        dump = vm.scratch / "acpi-memory.bin"
        vm.command(f'pmemsave 0x78000000 0x8000000 "{dump}"')
        memory = dump.read_bytes()
        dump.unlink()
        memory_base = 0x78000000
        max_table_size = 65536

        def sdt_at(address):
            start = address - memory_base
            if not 0 <= start <= len(memory) - 36:
                raise ValueError("ACPI table lies outside captured firmware memory")
            size = struct.unpack_from("<I", memory, start + 4)[0]
            if not 36 <= size <= max_table_size or start + size > len(memory):
                raise ValueError("invalid ACPI table size")
            table = memory[start:start + size]
            if sum(table) & 255:
                raise ValueError("invalid ACPI table checksum")
            return table

        tables = set()
        offset = 0
        max_candidates = 4096
        for _ in range(max_candidates):
            offset = memory.find(b"RSD PTR ", offset)
            if offset < 0:
                break
            rsdp = memory[offset:offset + 36]
            offset += 8
            if len(rsdp) != 36 or rsdp[15] < 2 or sum(rsdp) & 255 or sum(rsdp[:20]) & 255:
                continue
            xsdt = sdt_at(struct.unpack_from("<Q", rsdp, 24)[0])
            if xsdt[:4] != b"XSDT" or (len(xsdt) - 36) % 8:
                return False, "invalid firmware XSDT"
            selected = {}
            for entry in range(36, len(xsdt), 8):
                table = sdt_at(struct.unpack_from("<Q", xsdt, entry)[0])
                if table[:4] == b"APIC":
                    selected[table[10:24]] = table
            if len(selected) != 1:
                return False, f"expected one MADT identity, found {len(selected)}"
            tables.update(selected.values())
        else:
            return False, "RSDP memory search exceeded candidate limit"
        del memory
        if len(tables) != 1:
            return False, f"expected one selected firmware MADT, found {len(tables)}"
        madt = tables.pop()
        if fixture is not None and madt != fixture.read_bytes():
            return False, "firmware did not select the injected MADT"
        (out / "madt.bin").write_bytes(madt)
        enabled = {}
        offset = 44
        while offset < len(madt):
            kind, size = madt[offset:offset + 2]
            if size < 2 or offset + size > len(madt):
                return False, "invalid MADT entry length"
            if kind == 0 and size == 8:
                apic_id, entry_flags = madt[offset + 3], struct.unpack_from("<I", madt, offset + 4)[0]
                if entry_flags & 1:
                    enabled[apic_id] = kind
            elif kind == 9 and size == 16:
                apic_id, entry_flags = struct.unpack_from("<II", madt, offset + 4)
                if entry_flags & 1 and apic_id != 0xFFFFFFFF:
                    enabled[apic_id] = kind
            offset += size
        if enabled != expected_entries:
            return False, f"unexpected enabled MADT processors: {enabled}"
        since = vm.mark()
        vm.type("cat /scheme/sys/cpu; echo X2APIC-$((40+2))\n")
        found = vm.wait_serial(r"(?s)CPUs: (\d+).*?X2APIC-42", args.timeout, since)
        if not found:
            return False, "CPU count missing"
        count = int(found.group(1))
        nmi_ok = True
        if fixture is not None:
            for apic_id in expected_entries:
                lapic = vm.command(f"info lapic {apic_id}")
                (out / f"lapic-{apic_id}.txt").write_text(lapic)
                lint0 = re.search(r"LVT0\s+0x([0-9a-fA-F]+)", lapic)
                lint1 = re.search(r"LVT1\s+0x([0-9a-fA-F]+)", lapic)
                nmi_ok &= bool(lint0 and lint1 and int(lint1.group(1), 16) == 0x400
                               and (int(lint0.group(1), 16) == 0xA400) == (apic_id == 256))
        (out / "cpu-count.json").write_text(json.dumps({
            "expected": len(expected_entries), "online": count,
            "nmi_ok": nmi_ok if fixture is not None else None,
            "make": make, "qemu_flags": flags,
        }) + "\n")
        note = f"{count}/{len(expected_entries)} processors online"
        if fixture is not None:
            note += f"; NMI {'pass' if nmi_ok else 'fail'}"
        return count == len(expected_entries) and nmi_ok, note
    finally:
        vm.close()


def test_x2apic_cpus(args, out):
    """Boot native wide topology, type-9-only, and duplicate/disabled MADT entries."""
    native_out = out / "native"
    results = [x2apic_boot(args, native_out, {0: 0, 256: 9})]
    source = native_out / "madt.bin"
    if not source.exists():
        return results[0]
    native = source.read_bytes()
    non_cpu = bytearray()
    offset = 44
    while offset < len(native):
        kind, size = native[offset:offset + 2]
        if size < 2 or offset + size > len(native):
            return False, "invalid native MADT entry length"
        if kind not in (0, 4, 9, 10):
            non_cpu += native[offset:offset + size]
        offset += size
    for name, duplicate in (("type9-only", False), ("duplicate-disabled", True)):
        processors = bytearray()
        ids = (0, 1, 256) if duplicate else (0, 256)
        if duplicate:
            processors += struct.pack("<BBBBI", 0, 8, 1, 1, 1)
        for apic_id in ids:
            processors += struct.pack("<BBHIII", 9, 16, 0, apic_id, 1, apic_id)
        processors += struct.pack("<BBHIII", 9, 16, 0, 0xFFFFFFFF, 1, 1000)
        processors += struct.pack("<BBHIII", 9, 16, 0, 257, 0, 1001)
        processors += struct.pack("<BBHIB3x", 10, 12, 0, 0xFFFFFFFF, 1)
        processors += struct.pack("<BBHIB3x", 10, 12, 15, 256, 0)
        table = bytearray(native[:44]) + processors + non_cpu
        struct.pack_into("<I", table, 4, len(table))
        table[9] = 0
        table[9] = -sum(table) & 255
        fixture = out / f"{name}.bin"
        fixture.write_bytes(table)
        results.append(x2apic_boot(args, out / name, dict.fromkeys(ids, 9), fixture))
    return all(ok for ok, _ in results), "; ".join(
        f"{name}: {note}" for name, (_, note) in zip(
            ("native", "type9-only", "duplicate-disabled"), results))


def test_firefox_headless(vm, args):
    """Logs in as `user` and has `firefox --headless --screenshot` render a local page and then an HTTPS page to PNG, which travels back over the serial console (use --config firefox)."""
    if not user_bash(vm, args.timeout):
        return False, "no user Bash prompt after login"

    def run(line, timeout):
        """Types `line`; returns (exit status or None on a timeout, output)."""
        since = vm.mark()
        vm.type(f"{line}; echo FFRC-$((40+2))=$?\n")
        found = vm.wait_serial(r"(?s)\n(.*?)FFRC-42=(\d+)\s", timeout, since)
        if found:
            return int(found.group(2)), found.group(1)
        return None, vm.text(since).partition("\n")[2]

    def png_pixels(data):
        """Returns (width, height, distinct byte values in the image data) of a PNG, or None if it is not one."""
        if not data.startswith(b"\x89PNG\r\n\x1a\n"):
            return None
        width, height = int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")
        idat, at = b"", 8
        while at + 8 <= len(data):
            size, kind = int.from_bytes(data[at:at + 4], "big"), data[at + 4:at + 8]
            if kind == b"IDAT":
                idat += data[at + 8:at + 8 + size]
            at += 12 + size
        try:
            return width, height, len(set(zlib.decompress(idat)))
        except zlib.error:
            return None

    status, _ = run(f"rm -rf {FIREFOX_DIR}; mkdir -p {FIREFOX_DIR}/profile && cd {FIREFOX_DIR} && "
                    "printf '%s\\n' '<!doctype html><title>FerrionOS</title>' "
                    "'<body style=\"background:#fff;font:48px sans-serif\"><h1>Hello from FerrionOS</h1></body>' "
                    "> page.html", 30)
    if status != 0:
        return False, f"could not write the local page (status {status}); see {vm.out / 'serial.log'}"
    _, version = run("firefox --version", 120)
    (vm.out / "version.txt").write_text(version)
    status, _ = run(f"MOZ_HEADLESS=1 firefox --headless --no-remote --profile {FIREFOX_DIR}/profile "
                    f"--screenshot {FIREFOX_DIR}/warmup.png about:blank > {FIREFOX_DIR}/warmup.log 2>&1", 600)
    if status != 0:
        return False, f"profile warmup failed (status {status}); see {vm.out / 'serial.log'}"

    results = []
    for name, url, timeout in FIREFOX_PAGES:
        line = (f"MOZ_HEADLESS=1 firefox --headless --no-remote --profile {FIREFOX_DIR}/profile "
                f"--window-size=800,600 --screenshot {FIREFOX_DIR}/{name}.png {url} > {FIREFOX_DIR}/{name}.log 2>&1")
        status, _ = run(line, timeout)
        if status is None:
            vm.type("\x03")
            time.sleep(2)
        _, log = run(f"tail -n 40 {FIREFOX_DIR}/{name}.log", 30)
        (vm.out / f"{name}.log").write_text(f"$ {line}\nexit status {status}\n{log}")
        since = vm.mark()
        vm.type(f"echo FFPNG-BEGIN; base64 -w 0 {FIREFOX_DIR}/{name}.png; echo; echo FFPNG-END\n")
        found = vm.wait_serial(r"FFPNG-BEGIN\s*\n([A-Za-z0-9+/=\s]*?)FFPNG-END", 300, since)
        image = None
        if found:
            try:
                image = base64.b64decode("".join(found.group(1).split()), validate=True)
            except ValueError:
                image = None
        pixels = png_pixels(image) if image else None
        if pixels:
            (vm.out / f"{name}.png").write_bytes(image)
        if status is None:
            reason = f"timed out after {timeout}s"
        elif status != 0:
            reason = f"firefox exited {status}"
        elif not pixels:
            reason = "no PNG came back"
        elif pixels[2] < 3:
            reason = f"{pixels[0]}x{pixels[1]} PNG of one color"
        else:
            reason = None
        results.append((name, reason or f"{pixels[0]}x{pixels[1]} PNG, {pixels[2]} distinct byte values"))
        print(f"FIREFOX {name} {'FAIL' if reason else 'PASS'} {results[-1][1]}", flush=True)
        if reason:
            return False, f"{name} ({url}): {reason}; see {vm.out / f'{name}.log'}"
    return True, "; ".join(f"{name}: {note}" for name, note in results) + f"; inspect the PNGs in {vm.out}"


def test_firefox_start(vm, args):
    """Times the first font match and three local screenshots, reusing one initially empty profile (use --config firefox)."""
    if not user_bash(vm, args.timeout):
        return False, "no user Bash prompt after login"
    directory = "/tmp/firefox-start"
    since = vm.mark()
    vm.type(f"mkdir -p {directory}/profile; printf '%s\\n' "
            "'<!doctype html><title>FerrionOS</title><body style=\"background:#fff;font:48px sans-serif\">"
            "<h1>Hello from FerrionOS</h1></body>' "
            f"> {directory}/page.html; echo FFSREADY-$((40+2))\n")
    if not vm.wait_serial(r"FFSREADY-42", 30, since):
        return False, "could not prepare the startup page"
    measurements = {}
    since, started = vm.mark(), time.monotonic()
    vm.type("FC_DEBUG=16 fc-match sans-serif; echo FFSFONT-$((40+2))=$?\n")
    matched = vm.wait_serial(r"FFSFONT-42=(\d+)", 120, since)
    measurements["fontconfig_seconds"] = round(time.monotonic() - started, 3)
    font_log = vm.text(since)
    (vm.out / "fontconfig.log").write_text(font_log)
    if not matched or matched.group(1) != "0":
        return False, "first fc-match failed"
    problems = []
    if "FcCacheTimeValid" not in font_log or "FcDirCacheWriteDir" in font_log:
        problems.append("first font match did not use only prebuilt caches")
    for label in ("cold", "warm", "third"):
        since, started = vm.mark(), time.monotonic()
        vm.type(f"MOZ_HEADLESS=1 firefox --headless --no-remote --profile {directory}/profile "
                f"--window-size=800,600 --screenshot {directory}/{label}.png "
                f"file://{directory}/page.html; echo FFSEND-$((40+2))=$?\n")
        saved = vm.wait_serial(rf"Screenshot saved to: {directory}/{label}\.png", args.timeout, since)
        if saved:
            measurements[f"{label}_seconds"] = round(time.monotonic() - started, 3)
        exited = vm.wait_serial(r"FFSEND-42=(\d+)", args.timeout, since) if saved else None
        log = vm.text(since)
        (vm.out / f"{label}.log").write_text(log)
        if not saved or not exited or exited.group(1) != "0":
            (vm.out / "timings.json").write_text(json.dumps(measurements, indent=2) + "\n")
            return False, f"{label}: screenshot or successful exit missing"
        if "UnknownError" in log or "IndexedDB" in log and "error" in log.lower():
            problems.append(f"{label}: IndexedDB error")
        since = vm.mark()
        vm.type(f"echo FFSPNG-BEGIN; base64 -w 0 {directory}/{label}.png; echo; echo FFSPNG-END\n")
        found = vm.wait_serial(r"FFSPNG-BEGIN\s*\n([A-Za-z0-9+/=\s]*?)FFSPNG-END", 300, since)
        image = base64.b64decode("".join(found.group(1).split()), validate=True) if found else b""
        if not image.startswith(b"\x89PNG\r\n\x1a\n"):
            problems.append(f"{label}: missing PNG")
        else:
            (vm.out / f"{label}.png").write_bytes(image)
    (vm.out / "timings.json").write_text(json.dumps(measurements, indent=2) + "\n")
    note = "; ".join(f"{name}={seconds:.3f}s" for name, seconds in measurements.items())
    return not problems, note + ("; " + "; ".join(problems) if problems else "")


FIREFOX_JS_SCRIPT = """
function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
function workload() {
  var sum = fib(27);
  var arr = new Array(200000);
  for (var r = 0; r < 20; r++) {
    for (var i = 0; i < arr.length; i++) arr[i] = (i * r) ^ (i >> 3);
    for (var i = 0; i < arr.length; i++) sum = (sum + arr[i]) | 0;
  }
  var objs = [];
  for (var i = 0; i < 200000; i++) objs.push({x: i, y: i * 2});
  for (var k = 0; k < objs.length; k++) sum = (sum + objs[k].x + objs[k].y) | 0;
  var text = "";
  for (var i = 0; i < 50000; i++) text += String.fromCharCode(97 + i % 26);
  sum = (sum + text.length + text.charCodeAt(12345)) | 0;
  var sorted = objs.slice(0, 50000).sort(function (p, q) { return q.x - p.x; });
  return (sum + sorted[0].x) | 0;
}
function wasm() {
  var out = {present: typeof WebAssembly};
  if (typeof WebAssembly !== "object") return out;
  var bytes = new Uint8Array([0,97,115,109,1,0,0,0,1,19,4,96,2,127,127,1,127,96,1,127,1,127,96,1,127,0,96,0,0,3,5,4,0,1,2,3,5,3,1,0,1,7,29,4,3,97,100,100,0,0,4,108,111,97,100,0,1,5,115,116,111,114,101,0,2,4,116,114,97,112,0,3,10,31,4,7,0,32,0,32,1,106,11,7,0,32,0,40,2,0,11,9,0,32,0,65,7,54,2,0,11,3,0,0,11]);
  var e = new WebAssembly.Instance(new WebAssembly.Module(bytes)).exports;
  out.add = e.add(2, 3);
  e.store(8);
  out.inBounds = e.load(8);
  function attempt(f) { try { return "returned " + f(); } catch (err) { return err.constructor.name + ": " + err.message; } }
  out.loadOob = attempt(function () { return e.load(65536); });
  out.loadFar = attempt(function () { return e.load(0x40000000); });
  out.storeOob = attempt(function () { return e.store(65536); });
  out.unreachable = attempt(function () { return e.trap(); });
  return out;
}
var start = performance.now();
var checksum = 0;
for (var round = 0; round < 5; round++) checksum = (checksum + workload()) | 0;
var result = {ms: Math.round(performance.now() - start), checksum: checksum, wasm: wasm()};
var line = "FXJS " + JSON.stringify(result);
document.title = "JS " + result.ms + " ms";
document.getElementById("out").textContent = "JavaScript workload: " + result.ms + " ms, checksum " + checksum + ", WebAssembly: " + result.wasm.present;
dump(line + "\\n");
console.log(line);
"""
FIREFOX_JS_CHECKSUM = 758815123
FIREFOX_JS_PAGES = [
    ("js", "file:///tmp/firefox-js/page.html", "800,600", 900, True),
    ("https", "https://example.com/", "800,600", 900, True),
    ("kernelbench", "https://kernelbench.com/", "1280,900", 1800, False),
]
FIREFOX_KB_DELAYS = (3, 8, 15)


def firefox_kb_delayed(seconds):
    """A page that frames https://kernelbench.com/ and then keeps its own process busy for `seconds`, which holds
    `--screenshot` back while the framed page, in its own process, loads and runs its scripts."""
    return ('<!doctype html><body style="margin:0"><iframe src="https://kernelbench.com/" '
            'style="border:0;width:1280px;height:900px;display:block"></iframe>'
            f'<script>var end = Date.now() + {seconds} * 1000; while (Date.now() < end) {{}}</script></body>')


def png_summary(data):
    """Returns (width, height, distinct byte values in the image data) of a PNG, or None if it is not one."""
    if not data.startswith(b"\x89PNG\r\n\x1a\n"):
        return None
    width, height = int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")
    idat, at = b"", 8
    while at + 8 <= len(data):
        size, kind = int.from_bytes(data[at:at + 4], "big"), data[at + 4:at + 8]
        if kind == b"IDAT":
            idat += data[at + 8:at + 8 + size]
        at += 12 + size
    try:
        return width, height, len(set(zlib.decompress(idat)))
    except zlib.error:
        return None


def test_firefox_js(vm, args):
    """Firefox runs a fixed JavaScript workload in a local page and prints its time and checksum, compiles and runs WebAssembly whose out-of-bounds loads and stores and `unreachable` trap as RuntimeErrors (the fault handlers the JIT installs), then screenshots https://example.com/; also records the wall time of a screenshot of https://kernelbench.com/ (use --config firefox)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    for wait in (60, 180):
        since = vm.mark()
        vm.type("bash --norc --noprofile --noediting\n")
        if vm.wait_serial(r"bash-[0-9.]+\$ ", wait, since):
            break
    else:
        return False, "no bash prompt after login"

    def run(line, timeout):
        since = vm.mark()
        vm.type(f"{line}; echo FXJSRC-$((40+2))=$?\n")
        found = vm.wait_serial(r"(?s)\n(.*?)FXJSRC-42=(\d+)\s", timeout, since)
        return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))

    status, _ = run("unset MAIL MAILPATH MAILCHECK; exec 8< <(sleep 100000); " + GUEST_WATCH, 30)
    if status != 0 or not guest_watch(vm, "watch-before", "context syscall iostat", 120):
        return False, "the guest watcher did not start"

    def fetch_png(name):
        since = vm.mark()
        vm.type(f"echo FXJSPNG-BEGIN; base64 -w 0 {directory}/{name}.png; echo; echo FXJSPNG-END\n")
        found = vm.wait_serial(r"FXJSPNG-BEGIN\s*\n([A-Za-z0-9+/=\s]*?)FXJSPNG-END", 300, since)
        image = None
        if found:
            try:
                image = base64.b64decode("".join(found.group(1).split()), validate=True)
            except ValueError:
                image = None
        pixels = png_summary(image) if image else None
        if pixels:
            (vm.out / f"{name}.png").write_bytes(image)
        return image, pixels

    def guarded(command, timeout, name):
        line = (f"{command} > {directory}/{name}.log 2>&1 & fx=$!; n=0; "
                f"while kill -0 $fx 2>/dev/null && [ $n -lt {timeout} ]; do read -t 1 -u 8; n=$((n+1)); done; "
                "if kill -0 $fx 2>/dev/null; then echo FXHUNG; false; else wait $fx; fi")
        started = time.monotonic()
        status, output = run(line, timeout + 120)
        seconds = round(time.monotonic() - started, 1)
        if status is None or re.search(r"(?m)^FXHUNG\s*$", output):
            if status is None:
                vm.type("\x03\n")
                time.sleep(1)
            guest_watch(vm, "watch-failed-1", "context syscall", 60)
            time.sleep(3)
            guest_watch(vm, "watch-failed-2", "context syscall", 60)
            guest_watch(vm, "watch-failed-iostat", "iostat", 60)
            samples = []
            for _ in range(3):
                samples.append(vm.command("info registers -a"))
                time.sleep(2)
            (vm.out / f"{name}.cpus").write_text("\n=====\n".join(samples))
            run("kill -9 $fx 2>/dev/null; wait $fx 2>/dev/null", 30)
            return None, seconds
        return status, seconds

    directory = "/tmp/firefox-js"
    page = ('<!doctype html><meta charset="utf-8"><title>JS</title>'
            '<body style="background:#fff;font:32px sans-serif"><h1 id="out">running</h1>'
            f"<script>{FIREFOX_JS_SCRIPT}</script></body>")
    prefs = ('user_pref("browser.dom.window.dump.enabled", true);\n'
             'user_pref("devtools.console.stdout.content", true);\n'
             'user_pref("dom.max_script_run_time", 0);\n')
    encoded_page = base64.b64encode(page.encode()).decode()
    encoded_prefs = base64.b64encode(prefs.encode()).decode()
    status, _ = run(f"rm -rf {directory}; mkdir -p {directory}/profile && "
                    f"printf '%s' '{encoded_page}' | base64 -d > {directory}/page.html && "
                    f"printf '%s' '{encoded_prefs}' | base64 -d > {directory}/profile/user.js", 60)
    if status != 0:
        return False, f"could not write the page and profile (status {status})"
    firefox = f"MOZ_HEADLESS=1 firefox --headless --no-remote --profile {directory}/profile"
    status, seconds = guarded(f"{firefox} --screenshot {directory}/warmup.png about:blank", 600, "warmup")
    results = {"warmup": {"status": status, "seconds": seconds}}
    if status != 0:
        (vm.out / "firefox-js.json").write_text(json.dumps(results, indent=2) + "\n")
        return False, f"profile warmup failed (status {status})"

    problems = []
    for name, url, size, timeout, required in FIREFOX_JS_PAGES:
        line = f"{firefox} --window-size={size} --screenshot {directory}/{name}.png {url}"
        status, seconds = guarded(line, timeout, name)
        _, log = run(f"grep -a FXJS {directory}/{name}.log; grep -a -v '^\\[INFO' {directory}/{name}.log | head -n 80", 90)
        (vm.out / f"{name}.log").write_text(f"$ {line}\nexit status {status}, {seconds}s\n{log}")
        image, pixels = fetch_png(name)
        entry = {"url": url, "status": status, "seconds": seconds, "png": list(pixels) if pixels else None}
        if status is None:
            reason = f"timed out after {timeout}s"
        elif status != 0:
            reason = f"firefox exited {status}"
        elif not pixels or pixels[2] < 3:
            reason = "no PNG, or a PNG of one color"
        else:
            reason = None
        if name == "js":
            page_result = None
            for report in re.finditer(r"FXJS (\{.*\})", log):
                try:
                    page_result = json.loads(report.group(1))
                    break
                except ValueError:
                    continue
            entry["page"] = page_result
            wasm = (page_result or {}).get("wasm", {})
            traps = [wasm.get(key, "") for key in ("loadOob", "loadFar", "storeOob", "unreachable")]
            if not page_result:
                reason = reason or "the page printed no FXJS line"
            elif page_result.get("checksum") != FIREFOX_JS_CHECKSUM:
                reason = reason or f"checksum {page_result.get('checksum')}, want {FIREFOX_JS_CHECKSUM}"
            elif wasm.get("present") != "object":
                reason = reason or f"no WebAssembly (typeof {wasm.get('present')}): the JIT is off"
            elif wasm.get("add") != 5 or wasm.get("inBounds") != 7:
                reason = reason or f"WebAssembly computed add={wasm.get('add')} load={wasm.get('inBounds')}"
            elif not all(trap.startswith("RuntimeError") for trap in traps):
                reason = reason or f"WebAssembly traps: {traps}"
        entry["problem"] = reason
        results[name] = entry
        timing = f"{seconds}s" + (f", workload {entry['page']['ms']} ms" if entry.get("page") else "")
        print(f"FIREFOX-JS {name} {'FAIL' if reason else 'PASS'} {timing}" + (f": {reason}" if reason else ""),
              flush=True)
        if reason and required:
            problems.append(f"{name}: {reason}")

    for delay in FIREFOX_KB_DELAYS:
        name = f"kernelbench-delayed-{delay}"
        encoded = base64.b64encode(firefox_kb_delayed(delay).encode()).decode()
        run(f"printf '%s' '{encoded}' | base64 -d > {directory}/{name}.html", 60)
        line = (f"{firefox} --window-size=1280,900 --screenshot {directory}/{name}.png "
                f"file://{directory}/{name}.html")
        status, seconds = guarded(line, 600, name)
        _, log = run(f"grep -a -i 'error\\|kernelbench' {directory}/{name}.log | tail -n 40", 60)
        (vm.out / f"{name}.log").write_text(f"$ {line}\nexit status {status}, {seconds}s\n{log}")
        _, pixels = fetch_png(name)
        results[name] = {"status": status, "seconds": seconds, "png": list(pixels) if pixels else None}
        print(f"FIREFOX-JS {name} {results[name]}", flush=True)
    (vm.out / "firefox-js.json").write_text(json.dumps(results, indent=2) + "\n")
    summary = "; ".join(f"{name} {results[name]['seconds']}s" for name, *_ in FIREFOX_JS_PAGES)
    if results["js"].get("page"):
        summary = f"workload {results['js']['page']['ms']} ms; " + summary
    return not problems, summary + ("; " + "; ".join(problems) if problems else "") + f"; see {vm.out}"


FIREFOX_CJK = [
    ("zh-CN", "SC", "中文汉字骨直"),
    ("zh-TW", "TC", "中文漢字骨直"),
    ("zh-HK", "HK", "香港漢字骨直"),
    ("ja", "JP", "日本語あいう"),
    ("ko", "KR", "한국어한글말"),
]


def test_firefox_cjk(vm, args):
    """Firefox draws Chinese, Japanese and Korean text with the image's Noto Sans CJK, each glyph matching the host's rendering of the same pinned font, and fc-match picks each language's region (use --config firefox)."""
    try:
        from PIL import Image, ImageDraw, ImageFont
    except ImportError:
        raise Blocked("firefox-cjk draws its host reference with Pillow, which this host lacks")

    font_path = ROOT / "recipes/fonts/noto-sans-cjk/source/Variable/OTC/NotoSansCJK-VF.otf.ttc"
    if not font_path.is_file():
        raise Blocked("fetch noto-sans-cjk first; the CJK check needs its pinned font as a host reference")
    faces = subprocess.check_output(
        ["fc-scan", "--format", "%{family[0]}|%{index}\\n", str(font_path)], text=True)
    indices = {family: int(index) for family, index in
               (line.rsplit("|", 1) for line in faces.splitlines()) if int(index) < 65536}
    reference = Image.new("RGB", (800, 600), "white")
    drawing = ImageDraw.Draw(reference)
    html = ['<!doctype html><meta charset="utf-8"><title>CJK</title><body style="margin:0;background:white">']
    for row, (lang, region, text) in enumerate(FIREFOX_CJK):
        font = ImageFont.truetype(str(font_path), 48, index=indices[f"Noto Sans CJK {region}"])
        font.set_variation_by_axes([400])
        html.append(f'<canvas id="c{row}" lang="{lang}" width="800" height="96" style="display:block"></canvas>')
        html.append(f'<script>var c=document.getElementById("c{row}").getContext("2d");'
                    'c.font="48px sans-serif";c.fillStyle="black";')
        for column, char in enumerate(text):
            x = 8 + column * 64
            drawing.text((x, row * 96 + 64), char, font=font, fill="black", anchor="ls")
            html.append(f'c.fillText("\\u{ord(char):04x}",{x},64);')
        html.append('</script>')
    reference.save(vm.out / "cjk-reference.png")
    encoded = base64.b64encode("".join(html).encode()).decode()
    if not user_bash(vm, args.timeout):
        return False, "no user Bash prompt after login"

    def run(line, timeout):
        since = vm.mark()
        vm.type(f"{line}; echo CJKRC-$((40+2))=$?\n")
        found = vm.wait_serial(r"(?s)\n(.*?)CJKRC-42=(\d+)\s", timeout, since)
        return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))

    directory = "/tmp/firefox-cjk"
    status, _ = run(f"mkdir -p {directory}/profile; printf '%s' '{encoded}' | base64 -d > {directory}/page.html", 30)
    if status != 0:
        return False, "could not write CJK page"
    matches, failures = [], []
    for lang, region, _ in FIREFOX_CJK:
        status, result = run(f"fc-match --format '%{{family[0]}}\\n' 'sans-serif:lang={lang}'", 60)
        matches.append(f"{lang}: {result.strip()}")
        if status != 0 or f"Noto Sans CJK {region}" not in result.splitlines():
            failures.append(f"{lang}: wrong fontconfig family")
    (vm.out / "fc-match.txt").write_text("\n".join(matches) + "\n")
    status, _ = run(f"MOZ_HEADLESS=1 firefox --headless --no-remote --profile {directory}/profile "
                    f"--window-size=800,600 --screenshot {directory}/cjk.png file://{directory}/page.html "
                    f"> {directory}/firefox.log 2>&1", 600)
    if status is None:
        vm.type("\x03")
        time.sleep(2)
    _, log = run(f"tail -n 40 {directory}/firefox.log", 30)
    (vm.out / "firefox.log").write_text(log)
    since = vm.mark()
    vm.type(f"echo CJKPNG-BEGIN; base64 -w 0 {directory}/cjk.png; echo; echo CJKPNG-END\n")
    found = vm.wait_serial(r"CJKPNG-BEGIN\s*\n([A-Za-z0-9+/=\s]*?)CJKPNG-END", 300, since)
    if not found:
        return False, f"no CJK screenshot; firefox exit {status}"
    image_path = vm.out / "cjk.png"
    image_path.write_bytes(base64.b64decode("".join(found.group(1).split()), validate=True))
    actual = Image.open(image_path).convert("L")
    expected = reference.convert("L")
    if actual.size != expected.size or status != 0:
        return False, f"CJK screenshot {actual.size}, firefox exit {status}"
    scores = []
    for row, (lang, _, text) in enumerate(FIREFOX_CJK):
        for column, char in enumerate(text):
            box = (column * 64, row * 96, (column + 1) * 64, (row + 1) * 96)
            wanted = {(i % 64, i // 64) for i, value in enumerate(expected.crop(box).tobytes()) if value < 128}
            got = {(i % 64, i // 64) for i, value in enumerate(actual.crop(box).tobytes()) if value < 128}
            best = 0
            for dx in range(-2, 3):
                for dy in range(-2, 3):
                    shifted = {(x + dx, y + dy) for x, y in got}
                    union = wanted | shifted
                    best = max(best, len(wanted & shifted) / len(union) if union else 0)
            scores.append({"lang": lang, "glyph": char, "intersection_over_union": round(best, 4)})
            if best < 0.70:
                failures.append(f"{lang} {char}: glyph overlap {best:.3f} < 0.70")
    (vm.out / "cjk-glyphs.json").write_text(json.dumps(scores, ensure_ascii=False, indent=2) + "\n")
    return not failures, "; ".join(failures) if failures else "all CJK glyphs match the host reference; all regional fc-match families pass"


FIREFOX_WINDOW_DIR = "/tmp/firefox-window"
FIREFOX_WINDOW_PAGES = {"first": ("1f8a4c", "First page"), "second": ("c2185b", "Second page")}
FIREFOX_WINDOW_KEYS = {" ": "spc", "-": "minus", ".": "dot", "/": "slash", ":": "shift-semicolon", "\n": "ret"}
FIREFOX_WINDOW_UNITS = {"KB": 1 / 1024, "MB": 1, "GB": 1024}
FIREFOX_WINDOW_CHROME_MIN_COLORS = 8
FIREFOX_ADDR_FOCUS_WAIT_S = 15
FIREFOX_ADDR_FOCUS_ATTEMPTS = 2
FIREFOX_ADDR_FOCUS_POLL_S = 0.2
FIREFOX_ADDR_BAR_TOP_OFFSET = 48
FIREFOX_ADDR_BAR_BOTTOM_OFFSET = 8
FIREFOX_ADDR_BAR_SIDE_MARGIN = 120
FIREFOX_ADDR_SELECTED_MIN_PIXELS = 500
GUEST_WATCH = ("exec 7>&1; coproc GWATCH { exec 1>&7 2>&7; cd /scheme/sys || exit; "
               "while read -r tag files; do echo \"WATCH-BEGIN $tag\"; for f in $files; do "
               "while IFS= read -r l; do echo \"W:$f $l\"; done < \"$f\"; done; echo \"WATCH-END $tag\"; done; }")
FIREFOX_WINDOW_PREFS = [
    'user_pref("datareporting.policy.firstRunURL", "");',
    'user_pref("datareporting.policy.dataSubmissionEnabled", false);',
    'user_pref("browser.startup.homepage_override.mstone", "ignore");',
    'user_pref("browser.aboutwelcome.enabled", false);',
    'user_pref("browser.shell.checkDefaultBrowser", false);',
    'user_pref("ui.prefersReducedMotion", 1);',
]


def firefox_window_used(run):
    """Returns the guest's memory in use in MB as `free` reads it from /scheme/memory, or None when it cannot be read."""
    _, output = run("free | awk '/^Mem:/ {print \"FWUSED\", $3}'", 30)
    found = re.search(r"FWUSED (\d+)", output)
    return round(int(found.group(1)) / 1024, 1) if found else None


def firefox_window_page(frame, color):
    """Returns the bounds and pixel count of `color` on `frame`, or None when it covers no page-sized area."""
    wanted = bytes.fromhex(color)
    near = {bytes((r, g, b)) for r in range(wanted[0] - 2, wanted[0] + 3)
            for g in range(wanted[1] - 2, wanted[1] + 3) for b in range(wanted[2] - 2, wanted[2] + 3)}
    points = [i // 3 for i in range(0, len(frame.rgb), 3) if frame.rgb[i:i + 3] in near]
    if not points:
        return None
    xs = [p % frame.width for p in points]
    ys = [p // frame.width for p in points]
    left, top, right, bottom = min(xs), min(ys), max(xs), max(ys)
    if right - left < 300 or bottom - top < 200 or len(points) < (right - left + 1) * (bottom - top + 1) * 0.8:
        return None
    return {"bounds": [left, top, right, bottom], "pixels": len(points)}


def firefox_window_chrome(frame, page):
    """Counts the distinct colors in the band above the page area, where Firefox draws its tabs and address bar."""
    left, top, right, _ = page["bounds"]
    if top < 40:
        return 0
    return len({frame.rgb[(y * frame.width + x) * 3:(y * frame.width + x) * 3 + 3]
                for y in range(top - 40, top) for x in range(left, right + 1)})


def firefox_window_addr_band(page):
    """Computes the address-bar selection search band above a validated Firefox page."""
    left, top, right, _ = page["bounds"]
    assert left < right and top >= FIREFOX_ADDR_BAR_TOP_OFFSET
    band_top = top - FIREFOX_ADDR_BAR_TOP_OFFSET
    band_bottom = top - FIREFOX_ADDR_BAR_BOTTOM_OFFSET
    band_left = min(left + FIREFOX_ADDR_BAR_SIDE_MARGIN, right)
    band_right = max(right - FIREFOX_ADDR_BAR_SIDE_MARGIN, band_left)
    assert band_left < band_right and band_top < band_bottom
    return band_left, band_top, band_right, band_bottom


def firefox_window_addr_selected(frame, band):
    """Reports whether Firefox's address bar inside `band` shows the blue selection highlight."""
    band_left, band_top, band_right, band_bottom = band
    assert 0 <= band_left < band_right <= frame.width and 0 <= band_top < band_bottom <= frame.height
    count = 0
    for y in range(band_top, band_bottom):
        row = y * frame.width
        for x in range(band_left, band_right):
            offset = (row + x) * 3
            r, g, b = frame.rgb[offset], frame.rgb[offset + 1], frame.rgb[offset + 2]
            if b > 170 and r < 100 and g < 170:
                count += 1
                if count >= FIREFOX_ADDR_SELECTED_MIN_PIXELS:
                    return True
    return False


def guest_watch(vm, tag, files, timeout):
    """Asks the GUEST_WATCH coprocess to print `files` of /scheme/sys, opened relative to its working directory so the namespace manager is never asked, and saves them as <tag>.watch."""
    since = vm.mark()
    vm.type(f"echo {tag} {files} >&${{GWATCH[1]}}\n")
    found = vm.wait_serial(rf"(?s)WATCH-BEGIN {tag}\r?\n(.*?)WATCH-END {tag}", timeout, since)
    text = found.group(1) if found else vm.text(since)
    (vm.out / f"{tag}.watch").write_text(text)
    return bool(found)


def test_firefox_window(vm, args):
    """Opens Firefox in cosmic-comp's nested Wayland session on a colored local page, finds the page and Firefox's toolbar in a screenshot, types a second local page's address in the address bar and finds its color, then loads https://example.com/ the same way (use --config firefox-wayland)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, "no serial login"
    login_mark = vm.mark()
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20):
        return False, "no root password prompt"
    vm.type("password\n")
    if not vm.wait_serial(r"root:.*# ", 20, login_mark):
        return False, "serial shell did not become ready"
    vm.type("bash --norc --noprofile --noediting\n")
    time.sleep(1)

    def run(line, timeout):
        since = vm.mark()
        vm.type(f"{line}; echo FWRC-$((40+2))=$?\n")
        found = vm.wait_serial(r"(?s)\n(.*?)FWRC-42=(\d+)\s", timeout, since)
        return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))

    def keys(text):
        for char in text:
            vm.command(f"sendkey {FIREFOX_WINDOW_KEYS.get(char) or char}")
            time.sleep(0.05)

    def wait_page(color, timeout):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            frame = vm.screenshot()
            page = firefox_window_page(frame, color)
            if page:
                return frame, page
            time.sleep(0.2)
        return vm.screenshot(), None

    pages = " ".join(
        f"&& printf '%s' '<!doctype html><title>{title}</title><body style=\"margin:0;background:#{color}\">"
        f"<h1 style=\"color:#fff;font:64px sans-serif;margin:40px\">{title}</h1></body>' > {FIREFOX_WINDOW_DIR}/{name}.html"
        for name, (color, title) in FIREFOX_WINDOW_PAGES.items())
    prefs = " ".join(f"'{pref}'" for pref in FIREFOX_WINDOW_PREFS)
    status, _ = run(f"rm -rf {FIREFOX_WINDOW_DIR}; mkdir -p {FIREFOX_WINDOW_DIR}/profile {pages} "
                    f"&& printf '%s\\n' {prefs} > {FIREFOX_WINDOW_DIR}/profile/user.js", 30)
    if status != 0:
        return False, f"could not write the test pages (status {status})"
    status, _ = run("until test -f /tmp/gtk-window-session; do sleep 1; done; . /tmp/gtk-window-session; unset MOZ_HEADLESS",
                    GTK_SESSION_WAIT)
    if status != 0:
        return False, f"cosmic-comp wrote no session file within {GTK_SESSION_WAIT}s of the shell"
    session_file = vm.elapsed()
    used_before = firefox_window_used(run)
    status, _ = run("unset MAIL MAILPATH MAILCHECK; " + GUEST_WATCH, 30)
    if status != 0 or not guest_watch(vm, "watch-before", "context syscall iostat", 120):
        return False, "the guest watcher did not start"

    def wait_first_page(color, timeout):
        deadline = time.monotonic() + timeout
        first_seen = None
        last_page = None
        last_chrome = 0
        frame = None
        while time.monotonic() < deadline:
            frame = vm.screenshot()
            page = firefox_window_page(frame, color)
            if page:
                if first_seen is None:
                    first_seen = time.monotonic() - started
                chrome = firefox_window_chrome(frame, page)
                last_page, last_chrome = page, chrome
                if chrome >= FIREFOX_WINDOW_CHROME_MIN_COLORS:
                    return frame, page, first_seen, time.monotonic() - started, chrome
            time.sleep(0.2)
        return frame or vm.screenshot(), last_page, first_seen, None, last_chrome

    first_color = FIREFOX_WINDOW_PAGES["first"][0]
    started = time.monotonic()
    status, _ = run(f"GDK_BACKEND=wayland MOZ_ENABLE_WAYLAND=1 firefox --no-remote --profile {FIREFOX_WINDOW_DIR}/profile "
                    f"file://{FIREFOX_WINDOW_DIR}/first.html > {FIREFOX_WINDOW_DIR}/firefox.log 2>&1 & true", 30)
    frame, page, first_paint, first_chrome_s, first_chrome_colors = wait_first_page(first_color, args.timeout)
    if first_paint is None:
        first_paint = time.monotonic() - started
    frame.save_png(vm.out / "firefox-window-first.png")
    results = {"session_file_s": round(session_file, 1), "first_paint_s": round(first_paint, 1),
               "first_chrome_s": round(first_chrome_s, 1) if first_chrome_s is not None else None,
               "first": page, "used_before_mb": used_before}

    def finish(ok, note):
        if not ok:
            guest_watch(vm, "watch-failed-1", "context syscall", 60)
            time.sleep(3)
            guest_watch(vm, "watch-failed-2", "context syscall", 60)
            guest_watch(vm, "watch-failed-iostat", "iostat", 60)
        answered, log = run(f"tail -n 60 {FIREFOX_WINDOW_DIR}/firefox.log", 30)
        (vm.out / "firefox.log").write_text(log)
        if ok and answered is None:
            guest_watch(vm, "watch-silent", "context syscall", 60)
            ok, note = False, f"the guest stopped answering after the checks ({note}); see serial.log"
        results["used_with_firefox_mb"] = firefox_window_used(run)
        _, memory = run("ps | awk 'NR == 1 || (/firefox|cosmic-comp/ && !seen[$1]++)'", 60)
        (vm.out / "memory.txt").write_text(memory)
        results["memory"] = [line.split() for line in memory.splitlines() if "/" in line]
        firefox = [float(row[8]) * FIREFOX_WINDOW_UNITS[row[9]] for row in results["memory"]
                   if len(row) == 13 and row[9] in FIREFOX_WINDOW_UNITS and row[12].endswith("firefox")]
        results["firefox_allocated_mb"] = round(sum(firefox), 1)
        used = results["used_with_firefox_mb"]
        results["firefox_used_mb"] = round(used - used_before, 1) if used is not None and used_before is not None else None
        (vm.out / "firefox-window.json").write_text(json.dumps(results, indent=2) + "\n")
        run("pkill firefox; sleep 2", 30)
        return ok, (f"{note}; memory in use {used_before} MB before Firefox and {used} MB with it "
                    f"(+{results['firefox_used_mb']} MB); its {len(firefox)} processes hold "
                    f"{results['firefox_allocated_mb']} MB of allocated address space")

    if not page:
        return finish(False, f"no Firefox page of #{first_color} after {first_paint:.0f}s; see firefox-window-first.png")
    results["first_chrome_colors"] = first_chrome_colors
    if first_chrome_s is None or results["first_chrome_colors"] < FIREFOX_WINDOW_CHROME_MIN_COLORS:
        return finish(False, f"page painted but no Firefox toolbar above it: {results}")
    print(f"FIREFOX first page painted after {first_paint:.1f}s (chrome {first_chrome_s:.1f}s): {page}", flush=True)

    def focus_address_bar(tag, band):
        assert FIREFOX_ADDR_FOCUS_ATTEMPTS > 0 and FIREFOX_ADDR_FOCUS_WAIT_S > 0
        started = time.monotonic()
        frame = None
        for attempt in range(1, FIREFOX_ADDR_FOCUS_ATTEMPTS + 1):
            vm.command("sendkey ctrl-l")
            attempt_deadline = time.monotonic() + FIREFOX_ADDR_FOCUS_WAIT_S
            while time.monotonic() < attempt_deadline:
                frame = vm.screenshot()
                if firefox_window_addr_selected(frame, band):
                    return True, round(time.monotonic() - started, 2), attempt, None
                time.sleep(FIREFOX_ADDR_FOCUS_POLL_S)
        elapsed = round(time.monotonic() - started, 2)
        shot = f"firefox-window-focus-{tag}-{elapsed:.1f}s.png"
        (frame or vm.screenshot()).save_png(vm.out / shot)
        return False, elapsed, FIREFOX_ADDR_FOCUS_ATTEMPTS, shot

    second_color = FIREFOX_WINDOW_PAGES["second"][0]
    addr_band = firefox_window_addr_band(page)
    results["address_focus_s"] = {}
    results["address_focus_attempts"] = {}
    time.sleep(5)
    focused, focus_s, focus_attempts, focus_shot = focus_address_bar("second", addr_band)
    results["address_focus_s"]["second"] = focus_s
    results["address_focus_attempts"]["second"] = focus_attempts
    if not focused:
        return finish(False, f"address bar did not select after Ctrl+L for second page ({focus_s}s, {focus_attempts} attempts); see {focus_shot}")
    typed = time.monotonic()
    keys(f"file://{FIREFOX_WINDOW_DIR}/second.html\n")
    frame, page = wait_page(second_color, args.timeout)
    results["second_load_s"] = round(time.monotonic() - typed, 1)
    results["second"] = page
    frame.save_png(vm.out / "firefox-window-second.png")
    if not page:
        return finish(False, f"typing the second page's address did not show #{second_color}; see firefox-window-second.png")
    print(f"FIREFOX second page loaded after {results['second_load_s']}s (focus {focus_s}s, attempt {focus_attempts}): {page}", flush=True)

    focused, focus_s, focus_attempts, focus_shot = focus_address_bar("https", addr_band)
    results["address_focus_s"]["https"] = focus_s
    results["address_focus_attempts"]["https"] = focus_attempts
    if not focused:
        return finish(False, f"address bar did not select after Ctrl+L for example.com ({focus_s}s, {focus_attempts} attempts); see {focus_shot}")
    typed = time.monotonic()
    keys("https://example.com/\n")
    deadline = time.monotonic() + 600
    while time.monotonic() < deadline and firefox_window_page(vm.screenshot(), second_color):
        time.sleep(0.5)
    frame = settle(vm, 60, still=5)
    results["https_load_s"] = round(time.monotonic() - typed, 1)
    frame.save_png(vm.out / "firefox-window-https.png")
    left, top, right, bottom = results["second"]["bounds"]
    area = [frame.rgb[(y * frame.width + x) * 3:(y * frame.width + x) * 3 + 3]
            for y in range(top, bottom + 1, 4) for x in range(left, right + 1, 4)]
    light = sum(min(pixel) >= 200 for pixel in area)
    results["https"] = {"distinct_colors": len(set(area)), "light_share": round(light / len(area), 2)}
    if firefox_window_page(frame, second_color) or results["https"]["light_share"] < 0.5 or len(set(area)) < 3:
        return finish(False, f"https://example.com/ did not replace the second page: {results['https']}; see firefox-window-https.png")
    return finish(True, f"first paint {first_paint:.1f}s (chrome {first_chrome_s:.1f}s), toolbar colors {results['first_chrome_colors']}, "
                        f"address_focus_s {results['address_focus_s']['second']}/{results['address_focus_s']['https']} "
                        f"(attempts {results['address_focus_attempts']['second']}/{results['address_focus_attempts']['https']}), "
                        f"second page {results['second_load_s']}s, example.com {results['https_load_s']}s; see {vm.out}")


# `make qemu` appends -cpu host after QEMUFLAGS, so a -cpu option there is overridden.
RAND_NO_CPU = "-global host-x86_64-cpu.rdrand=off -global host-x86_64-cpu.rdseed=off"
RAND_FIRMWARE = RAND_NO_CPU + " -device virtio-rng-pci"


def test_rand_seed(vm, args):
    """Two boots differ with firmware RNG only, jitter only, and the host CPU RNG; status distinguishes seeded from jitter-only output (x86_64 KVM)."""
    # OVMF supplies EFI_RNG_PROTOCOL with RDRAND or virtio-rng, but not with neither.
    # Compare before checking status so an older image fails on repeated output, not a missing file.
    cases = [("firmware", RAND_FIRMWARE, True), ("jitter", RAND_NO_CPU, False), ("cpu", "", True)]
    notes = []
    for case, flags, seeded in cases:
        samples, statuses, logs = [], [], []
        for boot in range(2):
            current = vm
            if case != "firmware" or boot != 0:
                out = vm.out / f"{case}-{boot + 1}"
                out.mkdir(parents=True, exist_ok=True)
                current = VM(args, out, qemu_flags=flags)
            try:
                if not current.wait_serial(r"login: ", args.timeout):
                    return False, f"{case} boot {boot + 1}: no serial login"
                current.type("root\n")
                if not current.wait_serial(r"(?i)password: ", 20):
                    return False, f"{case} boot {boot + 1}: no password prompt"
                current.type("password\n")
                if not current.wait_serial(r"root:.*# ", 20):
                    return False, f"{case} boot {boot + 1}: root shell did not start"
                since = current.mark()
                current.type("od -An -tx1 -N64 /scheme/rand; echo rand-bytes-$((40+2))\n")
                found = current.wait_serial(r"(?s)(.*?)\nrand-bytes-42", 20, since)
                rows = re.findall(r"(?m)^((?:[ \t]+[0-9a-f]{2})+)[ \t]*$", found.group(1)) if found else []
                sample = bytes.fromhex(" ".join(rows))
                if len(sample) != 64:
                    return False, f"{case} boot {boot + 1}: did not read 64 random bytes"
                samples.append(sample)
                (current.out / "random.txt").write_text(sample.hex() + "\n")
                since = current.mark()
                current.type("cat /scheme/rand/status; echo rand-status-$((40+2))\n")
                found = current.wait_serial(r"(?s)(.*?)\nrand-status-42", 20, since)
                status = found.group(1) if found else ""
                statuses.append(status)
                (current.out / "random-status.txt").write_text(status)
                logs.append(current.text())
                current.kill()
            finally:
                if current is not vm:
                    current.close()
        if samples[0] == samples[1]:
            return False, f"{case}: identical 64 random bytes across boots: {samples[0].hex()}"
        expected = "yes" if seeded else "no"
        for boot, (status, log) in enumerate(zip(statuses, logs), 1):
            if not re.search(rf"(?m)^seeded: {expected}$", status):
                return False, f"{case} boot {boot}: expected seeded: {expected}"
            sources = re.search(r"(?m)^sources: (.*)$", status)
            names = sources.group(1).split() if sources else []
            if case != "cpu" and any(name in names for name in ("rdseed", "rdrand")):
                return False, f"{case} boot {boot}: CPU RNG was not disabled"
            if case == "firmware" and "firmware" not in names:
                return False, f"{case} boot {boot}: firmware seed missing"
            if case == "cpu" and not any(name in names for name in ("rdseed", "rdrand")):
                return False, f"{case} boot {boot}: host CPU supplied no seed"
            message = ("randd: seeded from a hardware or firmware source:" if seeded
                       else "randd: NOT SEEDED from a hardware or firmware source")
            if message not in log:
                return False, f"{case} boot {boot}: seed log missing"
        notes.append(f"{case}: two boots differ, seeded: {expected}")
    return True, "; ".join(notes)


def test_ferrion_cli(vm, args):
    """Root runs ferrion status and shell list; search refuses with no package source (exit 6); with a source on the host naming one package of this build's repo/, signed by the build's key, search, install and remove work and the installed program runs (x86_64 desktop)."""
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-repo-"))
    try:
        server, seen = package_fixture_server(args, scratch)
        try:
            return ferrion_cli_checks(vm, args, server.server_address[1], seen)
        finally:
            server.shutdown()
            server.server_close()
    finally:
        shutil.rmtree(scratch)


# A package source served from the host for ferrion-cli, so the check reaches no outside server:
# one package of this build's own repo/, which the desktop image does not install and which
# depends on nothing, with an index naming its hash and the build's public key, the key that
# signed it and every package of the image. QEMU's user network takes the guest's connections to
# 10.0.2.2 to the host's 127.0.0.1.
FIXTURE_PACKAGE, FIXTURE_PROGRAM = "gnu-make", "/usr/bin/make"


def package_fixture_server(args, scratch):
    import functools
    import http.server
    import tomllib

    target = f"{args.arch}-unknown-redox"
    source = ROOT / "repo" / target
    head = source / f"{FIXTURE_PACKAGE}.toml"
    if not head.exists():
        sys.exit(f"vm-test: {head.relative_to(ROOT)} is missing; cook it first: make r.{FIXTURE_PACKAGE}")
    repo = scratch / target
    repo.mkdir()
    shutil.copy(head, repo)
    shutil.copy(source / f"{FIXTURE_PACKAGE}.pkgar", repo)
    blake3 = tomllib.loads(head.read_text())["blake3"]
    (repo / "repo.toml").write_text(f'[packages]\n{FIXTURE_PACKAGE} = "{blake3}"\n')
    shutil.copy(ROOT / "build" / "id_ed25519.pub.toml", scratch)
    seen = []

    class Handler(http.server.SimpleHTTPRequestHandler):
        def do_GET(self):
            seen.append(self.path)
            super().do_GET()

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(scratch)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, seen


def ferrion_cli_checks(vm, args, port, seen):
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20):
        return False, "no password prompt for root"
    since = vm.mark()
    vm.type("password\n")
    if not vm.wait_serial(r"root:[^\r\n]*# ", 30, since):
        return False, "root Ion prompt did not appear after authentication"
    since = vm.mark()
    vm.type("bash --norc --noprofile --noediting\n")
    if not vm.wait_serial(r"bash-[0-9.]+# ", 30, since):
        return False, "bash prompt did not appear after root login"

    def run(line, timeout=60):
        since = vm.mark()
        vm.type(f"{line}; echo FCLI-END-$((40+2))=$?\n")
        found = vm.wait_serial(r"(?s)\n(.*?)FCLI-END-42=(\d+)\s", timeout, since)
        return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))

    def check_json(command, timeout=60, expect=0):
        code, text = run(f"ferrion --json {command}", timeout)
        (vm.out / (command.split()[0] + ".txt")).write_text(text)
        records = [line.strip() for line in text.splitlines() if line.lstrip().startswith("{")]
        if code != expect or len(records) != 1:
            raise ValueError(f"{command}: exit {code}, expected {expect} and one JSON record; see {vm.out}")
        record = json.loads(records[0])
        if (record.get("schema_version") != 1 or record.get("ok") is not (expect == 0)
                or record.get("exit_code") != code):
            raise ValueError(f"{command}: invalid envelope")
        expected = "shell list" if command == "shell list" else command.split()[0]
        if record.get("command") != expected:
            raise ValueError(f"{command}: wrong command in envelope")
        return record["data"]

    try:
        status = check_json("status")
        if not status.get("version"):
            raise ValueError("status: missing FerrionOS version")
        for key in ("verified_boot", "measured_boot", "lockdown", "disk_encryption", "seeded_randomness"):
            if not isinstance(status.get(key), dict) or not status[key].get("state"):
                raise ValueError(f"status: missing {key} observation")
        if status["measured_boot"]["state"] == "unknown":
            raise ValueError("root status: attest-statement returned no verdict")
        shells = check_json("shell list")["shells"]
        if {shell["name"] for shell in shells} != {"bash", "fish", "ion", "nushell", "zsh"}:
            raise ValueError("shell list: the five offered shells are not listed")
        if not all(shell["installed"] for shell in shells):
            raise ValueError("shell list: an offered shell is not executable")
        # dhcpd starts asynchronously. Wait for its published lease, so the refusal below happens
        # with a working network.
        code, network = run(
            'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); '
            'while [[ -z "$server" || "$server" == 9.9.9.9 ]] && (( SECONDS < deadline )); do '
            'sleep 0.1; server=$(cat /etc/net/dns); done; '
            'echo "configured DNS: $server"; [[ -n "$server" && "$server" != 9.9.9.9 ]]', 70)
        (vm.out / "network.txt").write_text(network)
        if code != 0:
            raise ValueError("boot DHCP did not publish a lease DNS server")
        if check_json(f"search {FIXTURE_PACKAGE}", expect=6).get("package_source") != "none":
            raise ValueError("search: the refusal does not say package_source none")
        code, _ = run(f"test ! -e {FIXTURE_PROGRAM}")
        if code != 0:
            raise ValueError(f"{FIXTURE_PROGRAM} is already in the image; refusing to remove an image package")
        code, _ = run(f"echo http://10.0.2.2:{port} > /etc/pkg.d/90_vm_test")
        if code != 0:
            raise ValueError("could not add the host's package source")
        try:
            results = check_json(f"search {FIXTURE_PACKAGE}", 180)
            if FIXTURE_PACKAGE not in results["matches"]:
                raise ValueError(f"search: {FIXTURE_PACKAGE} was not found in the host's source")
            check_json(f"install {FIXTURE_PACKAGE} --yes", 180)
            code, _ = run(f"test -x {FIXTURE_PROGRAM} && {FIXTURE_PROGRAM} --version")
            if code != 0:
                raise ValueError(f"install: {FIXTURE_PROGRAM} was not installed as a working executable")
            check_json(f"remove {FIXTURE_PACKAGE} --yes", 180)
            code, _ = run(f"test ! -e {FIXTURE_PROGRAM}")
            if code != 0:
                raise ValueError(f"remove: {FIXTURE_PROGRAM} is still present")
        finally:
            run("rm -f /etc/pkg.d/90_vm_test")
    except (ValueError, KeyError, TypeError) as error:
        return False, f"{error}; host source served {seen}"
    return True, (f"status and shells parsed; search refused with no source; from the host's source "
                  f"{FIXTURE_PACKAGE} was found, installed, ran and was removed ({len(seen)} requests)")


UPDATE_BOOT_FILES = "/usr/lib/boot/kernel /usr/lib/boot/initfs"


def test_update_today(vm, args):
    """As root with a working network, ferrion update exits 6 saying the image has no package source and pkg update fails without fetching; the kernel's and initfs's hashes are unchanged, the guest opens no TCP connection and sends no DNS query meanwhile, and the next start reaches the login (x86_64 desktop)."""
    capture = vm.out / "update.pcap"
    if not root_shell(vm, args.timeout):
        return False, "no root shell on the serial console"
    status, network = run_status(
        vm, "bash -c 'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); "
            "while [[ -z \"$server\" || \"$server\" == 9.9.9.9 ]] && (( SECONDS < deadline )); do "
            "sleep 0.1; server=$(cat /etc/net/dns); done; echo \"configured DNS: $server\"; "
            "[[ -n \"$server\" && \"$server\" != 9.9.9.9 ]]'", 70)
    (vm.out / "network.txt").write_text(network)
    if status != 0:
        return False, "boot DHCP did not publish a lease DNS server, so the network is not up"
    _, sources = run_status(vm, "bash -c 'grep -rv \"^#\" /etc/pkg.d; true'")
    before_status, before = run_status(vm, f"sha256sum {UPDATE_BOOT_FILES}")
    if before_status != 0:
        return False, f"sha256sum failed: {before}"
    # Packets are cut at 256 bytes: headers and DNS names are all the check reads.
    reply = vm.command(f"object_add filter-dump,id=ferrion-update,netdev=net0,maxlen=256,file={capture.resolve()}")
    if "error" in reply.lower():
        return False, f"could not capture net0: {reply}"
    try:
        # Today's update can run for many minutes; the disk is a snapshot, so letting it run harms nothing.
        update_status, update = run_status(vm, "ferrion --json update --yes", 1800)
        pkg_status, pkg = run_status(vm, "pkg update --yes", 1800)
    finally:
        vm.command("object_del ferrion-update")
    after_status, after = run_status(vm, f"sha256sum {UPDATE_BOOT_FILES}")
    (vm.out / "update.txt").write_text(
        f"/etc/pkg.d sources:\n{sources}\n[ferrion --json update --yes] exit {update_status}\n{update}\n"
        f"[pkg update --yes] exit {pkg_status}\n{pkg}\n[before]\n{before}\n[after] exit {after_status}\n{after}\n")
    hashes = lambda text: sorted(re.findall(r"(?m)^([0-9a-f]{64}) +\S*/usr/lib/boot/\w+", text))
    changed = len(hashes(before)) != 2 or hashes(before) != hashes(after)

    # Guest-opened TCP connections (SYN without ACK) and DNS questions while the commands ran.
    raw = capture.read_bytes() if capture.exists() else b""
    order = {b"\xd4\xc3\xb2\xa1": "little", b"\xa1\xb2\xc3\xd4": "big"}.get(raw[:4])
    if order is None or int.from_bytes(raw[20:24], order) != 1:
        return False, "update capture is not an Ethernet pcap"
    connections, queries, pos = set(), set(), 24
    while pos + 16 <= len(raw):
        size = int.from_bytes(raw[pos + 8:pos + 12], order)
        frame, pos = raw[pos + 16:pos + 16 + size], pos + 16 + size
        if len(frame) < 34 or frame[12:14] != b"\x08\x00" or frame[14] >> 4 != 4:
            continue
        ip = frame[14:]
        ihl, destination = (ip[0] & 15) * 4, socket.inet_ntoa(ip[16:20])
        body = ip[ihl:]
        if ip[9] == 6 and len(body) >= 14 and body[13] & 0x12 == 0x02:
            connections.add(f"{destination}:{int.from_bytes(body[2:4], 'big')}")
        elif ip[9] == 17 and len(body) >= 20 and int.from_bytes(body[2:4], "big") == 53:
            labels, cursor = [], 20
            while cursor < len(body) and 0 < body[cursor] < 64:
                labels.append(body[cursor + 1:cursor + 1 + body[cursor]].decode(errors="replace"))
                cursor += body[cursor] + 1
            queries.add(".".join(labels))
    wire = f"TCP connections opened: {sorted(connections)}; DNS questions: {sorted(queries)}"
    (vm.out / "update-wire.txt").write_text(wire + "\n")

    # The next start: refused by verified boot, or the login.
    since = vm.mark()
    vm.command("system_reset")
    restart = vm.wait_serial(r"does not match the signed manifest|login: ", args.timeout, since)
    restart = restart.group(0) if restart else f"neither the login nor a refusal within {args.timeout}s"

    record = next((json.loads(line) for line in update.splitlines() if line.lstrip().startswith("{")), {})
    said = (update_status == 6 and record.get("exit_code") == 6
            and record.get("data", {}).get("package_source") == "none"
            and str(record.get("error", "")).startswith("This image has no package source yet"))
    pkg_said = re.search(r"(?m)^.*(?:not found|error).*$", pkg)
    note = (f"ferrion update exit {update_status}, pkg update exit {pkg_status} "
            f"({pkg_said.group(0).strip() if pkg_said else 'no reason printed'}); boot files "
            f"{'CHANGED' if changed else 'unchanged'}; {wire}; next start: {restart.strip()}; see {vm.out}")
    ok = said and bool(pkg_said) and not changed and not connections and not queries and restart == "login: "
    return ok, note


def update_index_fixtures(args, scratch):
    release_bin = ROOT / "target" / "release" / "ferrion-release"
    if not release_bin.is_file():
        sys.exit("vm-test: target/release/ferrion-release is missing; build it with cargo build --release --bin ferrion-release")
    target = f"{args.arch}-unknown-redox"
    pkg_file = ROOT / "repo" / target / f"{FIXTURE_PACKAGE}.pkgar"
    if not pkg_file.is_file():
        sys.exit(f"vm-test: {pkg_file.relative_to(ROOT)} is missing; cook it first: make r.{FIXTURE_PACKAGE}")

    def call(*cmd):
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        if proc.returncode != 0:
            raise RuntimeError(f"{cmd} failed ({proc.returncode}): {proc.stderr.strip()}")
        return proc.stdout.strip()

    key_dir = scratch / "keys"
    other_key_dir = scratch / "other-keys"
    pubkey = call(str(release_bin), "keygen", "--out-dir", str(key_dir))
    call(str(release_bin), "keygen", "--out-dir", str(other_key_dir))
    key_pem = str(key_dir / "release.key")
    other_pem = str(other_key_dir / "release.key")

    def build_site(name, release, serial, signed, prev_root=None, channel=None):
        out = scratch / name
        cmd = [
            str(release_bin), "build",
            "--repo", str(ROOT / "repo"),
            "--out", str(out),
            "--arch", args.arch,
            "--release", release,
            "--serial", str(serial),
            "--signed", signed,
            "--fresh-days", "45",
            "--package", FIXTURE_PACKAGE,
            "--default-set", "app",
            "--key", key_pem,
        ]
        if channel:
            cmd += ["--channel", channel]
        else:
            cmd += ["--channel", f"dev={release}"]
        if prev_root:
            cmd += ["--previous-root", str(prev_root)]
        call(*cmd)
        return out

    def sign_raw(pem, in_path, out_path):
        call("openssl", "pkeyutl", "-sign", "-rawin", "-inkey", pem, "-in", str(in_path), "-out", str(out_path))

    r1 = build_site("r1", "2026112100", 1, "2026-11-21")
    r2 = build_site("r2", "2026112200", 2, "2026-11-22", prev_root=r1 / "root.toml")
    r3 = build_site("r3", "2027020100", 3, "2027-02-01", prev_root=r2 / "root.toml")

    sites = {"r2": r2, "r3": r3}

    c1 = scratch / "case1-wrong-key"
    shutil.copytree(r2, c1)
    sign_raw(other_pem, c1 / "root.toml", c1 / "root.toml.sig")
    sites["c1"] = c1

    sites["c2"] = r1

    c3 = scratch / "case3-channel-downgrade"
    shutil.copytree(r2, c3)
    call(
        str(release_bin), "sign-root",
        "--root", str(c3 / "root.toml"),
        "--out", str(c3),
        "--serial", "10",
        "--signed", "2026-11-22",
        "--channel", "dev=2026112100",
        "--key", key_pem,
    )
    sites["c3"] = c3

    c4 = scratch / "case4-index-hash"
    shutil.copytree(r3, c4)
    idx4 = c4 / "snap" / "2027020100" / args.arch / "index.toml"
    idx4.write_bytes(idx4.read_bytes() + b"\n")
    sign_raw(key_pem, idx4, c4 / "snap" / "2027020100" / args.arch / "index.toml.sig")
    sites["c4"] = c4

    c5 = scratch / "case5-object-flip"
    shutil.copytree(r3, c5)
    obj5 = next((c5 / "objects").rglob("*"))
    while obj5.is_dir():
        obj5 = next(p for p in (c5 / "objects").rglob("*") if p.is_file())
    raw5 = bytearray(obj5.read_bytes())
    raw5[0] ^= 0x01
    obj5.write_bytes(bytes(raw5))
    sites["c5"] = c5

    c6 = scratch / "case6-object-oversize"
    shutil.copytree(r3, c6)
    obj6 = next(p for p in (c6 / "objects").rglob("*") if p.is_file())
    obj6.write_bytes(obj6.read_bytes() + b"0123456789abcdef")
    sites["c6"] = c6

    c7a = scratch / "case7a-unknown-key"
    shutil.copytree(r3, c7a)
    root7a = c7a / "root.toml"
    root7a.write_text('unexpected_key = "x"\n' + root7a.read_text())
    sign_raw(key_pem, root7a, c7a / "root.toml.sig")
    sites["c7a"] = c7a

    c7b = scratch / "case7b-unknown-format"
    shutil.copytree(r3, c7b)
    root7b = c7b / "root.toml"
    root7b.write_text(root7b.read_text().replace("format = 1", "format = 2", 1))
    sign_raw(key_pem, root7b, c7b / "root.toml.sig")
    sites["c7b"] = c7b

    return pubkey, sites


def update_index_server(sites):
    import http.server

    state = {"dir": sites["r2"]}
    requests_log = []

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def do_GET(self):
            requests_log.append({
                "path": self.path,
                "user_agent": self.headers.get("User-Agent"),
                "cookie": self.headers.get("Cookie"),
            })
            rel = self.path.lstrip("/")
            if "?" in self.path or ".." in rel.split("/"):
                self.send_response(400)
                self.end_headers()
                return
            target = (state["dir"] / rel).resolve()
            base = state["dir"].resolve()
            if not str(target).startswith(str(base)) or not target.is_file():
                self.send_response(404)
                self.end_headers()
                return
            data = target.read_bytes()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, state, requests_log


def test_update_index(vm, args):
    """As root with a working network, runs update-index-probe against a static server on the host serving ferrion-release snapshots across all eight UP1 refusal and staleness cases, checking state immutability, request hygiene, and that ferrion update stays exit 6 (x86_64 desktop)."""
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-up1-"))
    try:
        pubkey, sites = update_index_fixtures(args, scratch)
        server, srv_state, requests_log = update_index_server(sites)
        port = server.server_address[1]
        try:
            if not root_shell(vm, args.timeout):
                return False, "no root shell on the serial console"
            since = vm.mark()
            vm.type("bash --norc --noprofile --noediting\n")
            if not vm.wait_serial(r"bash-[0-9.]+# ", 30, since):
                return False, "bash prompt did not appear after root login"
            status, network = run_status(
                vm, 'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); '
                    'while [[ -z "$server" || "$server" == 9.9.9.9 ]] && (( SECONDS < deadline )); do '
                    'sleep 0.1; server=$(cat /etc/net/dns); done; echo "configured DNS: $server"; '
                    '[[ -n "$server" && "$server" != 9.9.9.9 ]]', 70)
            (vm.out / "network.txt").write_text(network)
            if status != 0:
                return False, "boot DHCP did not publish a lease DNS server"

            have_code, have_out = run_status(vm, "test -x /usr/bin/update-index-probe")
            if have_code != 0:
                return False, "update-index-probe is not installed in this image"

            transcript = []

            def run_case(label, site_key, today, expect_code, expect_marker):
                srv_state["dir"] = sites[site_key]
                code, out = run_status(
                    vm,
                    f"update-index-probe sync --state /tmp/up1.state --server 10.0.2.2:{port} --today {today}",
                    60,
                )
                transcript.append(f"[{label}] site={site_key} today={today} exit={code}\n{out.strip()}\n")
                if code != expect_code or expect_marker not in out:
                    raise ValueError(f"{label}: exit={code}, expected {expect_code} with {expect_marker!r}; got {out.strip()!r}")
                return out

            def state_hash():
                code, out = run_status(vm, "sha256sum /tmp/up1.state")
                if code != 0:
                    raise ValueError(f"sha256sum /tmp/up1.state failed: {out}")
                m = re.search(r"([0-9a-f]{64})", out)
                if not m:
                    raise ValueError(f"no sha256 in {out!r}")
                return m.group(1)

            try:
                init_code, init_out = run_status(
                    vm,
                    f"update-index-probe init --state /tmp/up1.state --pubkey {pubkey} --arch {args.arch} --channel dev --installed 2026112100 --serial 1",
                )
                transcript.append(f"[init] exit={init_code}\n{init_out.strip()}\n")
                if init_code != 0 or "INITIALIZED release=2026112100 serial=1" not in init_out:
                    raise ValueError(f"init failed: {init_code} {init_out}")

                run_case("baseline-r2", "r2", "2026-11-22", 0, "ACCEPTED release=2026112200 serial=2 packages=1 stale=no")
                baseline_hash = state_hash()

                refusal_cases = [
                    ("case1-wrong-key", "c1", "2026-11-22", "REFUSED reason=invalid-signature"),
                    ("case2-replay-serial", "c2", "2026-11-22", "REFUSED reason=rollback-serial"),
                    ("case3-channel-downgrade", "c3", "2026-11-22", "REFUSED reason=rollback-release"),
                    ("case4-index-hash-mismatch", "c4", "2027-02-01", "REFUSED reason=index-hash-mismatch"),
                    ("case5-object-byte-flip", "c5", "2027-02-01", "REFUSED reason=object-hash-mismatch"),
                    ("case6-object-oversize", "c6", "2027-02-01", "REFUSED reason=object-too-large"),
                    ("case7a-unknown-key", "c7a", "2027-02-01", "REFUSED reason=unknown-key-or-syntax"),
                    ("case7b-unknown-format", "c7b", "2027-02-01", "REFUSED reason=unsupported-format"),
                ]
                for label, site_key, today, marker in refusal_cases:
                    run_case(label, site_key, today, 1, marker)
                    after_hash = state_hash()
                    if after_hash != baseline_hash:
                        raise ValueError(f"{label}: installed state changed ({baseline_hash} -> {after_hash})")

                run_case("case8a-stale-accepted", "r2", "2027-02-01", 0, "ACCEPTED release=2026112200 serial=2 packages=1 stale=yes age_days=71")
                run_case("case8b-fresh-replaces-stale", "r3", "2027-02-01", 0, "ACCEPTED release=2027020100 serial=3 packages=1 stale=no age_days=0")
                if state_hash() == baseline_hash:
                    raise ValueError("case8b: state did not advance after fresh replacement")

                up0_code, up0_out = run_status(vm, "ferrion --json update --yes")
                transcript.append(f"[up0-guard] exit={up0_code}\n{up0_out.strip()}\n")
                if up0_code != 6:
                    raise ValueError(f"ferrion update must stay exit 6 until UP3, got {up0_code}")

                for req in requests_log:
                    if req["user_agent"] != "ferrion-update/1":
                        raise ValueError(f"unexpected User-Agent: {req!r}")
                    if "?" in req["path"] or req["cookie"] is not None:
                        raise ValueError(f"request carried query or cookie: {req!r}")
            except ValueError as err:
                (vm.out / "update-index.txt").write_text("\n".join(transcript))
                (vm.out / "update-index-requests.json").write_text(json.dumps(requests_log, indent=2))
                return False, f"{err}; see {vm.out}"

            (vm.out / "update-index.txt").write_text("\n".join(transcript))
            (vm.out / "update-index-requests.json").write_text(json.dumps(requests_log, indent=2))
            return True, f"8/8 UP1 cases passed (7 refusals left state unchanged; stale accepted with notice then replaced; {len(requests_log)} clean HTTP requests; ferrion update exit 6); see {vm.out}"
        finally:
            server.shutdown()
            server.server_close()
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


# An HTTP server on the host for the ferrion-run test; `seen` lists the paths requested.
# QEMU's user network takes the guest's connections to 10.0.2.2 to the host's 127.0.0.1.
def ferrion_run_server():
    import http.server

    seen = []

    class Handler(http.server.BaseHTTPRequestHandler):
        timeout = 30

        def do_GET(self):
            seen.append(self.path)
            body = b"ferrion-run-ok\n"
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, seen


def test_ferrion_run(vm, args):
    """As `user`: under ferrion-run --no-network curl fails with a network error, also through sudo, and a profile alone withholds the network; a socket opened before the launch is unusable in the program; a socket or disk file as a standard descriptor is refused. Each denial is first run without the launcher, where it must succeed."""
    server, seen = ferrion_run_server()
    try:
        return ferrion_run_checks(vm, args, server.server_address[1], seen)
    finally:
        server.shutdown()
        server.server_close()


def ferrion_run_checks(vm, args, port, seen):
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    address, transcript = f"10.0.2.2:{port}", []

    def run(tag, line, timeout=60, sudo=False):
        """Runs `line` in bash as `user`; returns (exit status, output) or (None, output so far).
        With `sudo`, answers sudo's password prompt with `user`'s empty password."""
        since = vm.mark()
        vm.type(f"bash -c '{line}; echo fr-end-{tag}-$?'\n")
        end = rf"(?s)\n(.*)fr-end-{tag}-(\d+)"
        if sudo and vm.wait_serial(rf"\[sudo\] password|fr-end-{tag}-\d", timeout, since):
            if not re.search(rf"fr-end-{tag}-\d", vm.text(since)):
                vm.type("\n")
        found = vm.wait_serial(end, timeout, since)
        output = found.group(1) if found else vm.text(since)
        transcript.append(f"$ {line}\n{output}exit {found.group(2) if found else 'none'}\n")
        return (int(found.group(2)) if found else None), output

    def curl(path):
        return f"curl -sS -m 10 -o /dev/null -w http-%{{http_code}} http://{address}{path}"

    controls, problems = [], []
    # Without the launcher each of these must reach the host, or the denials below prove nothing.
    for attempt in range(10):
        status, output = run(f"net{attempt}", curl("/plain"))
        if status == 0 and "http-200" in output:
            break
        time.sleep(3)
    else:
        controls.append("plain curl reached no host server")
    status, output = run("probe", f"ferrion-run-probe connect {address} /probe-plain")
    if status != 0 or "/probe-plain" not in seen:
        controls.append("ferrion-run-probe connect failed without the launcher")
    status, output = run("held", f"ferrion-run-probe hold-socket {address} -- ferrion-run-probe use-fd /held-plain")
    if status != 0 or "/held-plain" not in seen:
        controls.append("a held socket was unusable without the launcher")
    # `user` may sudo with an empty password on dev images: sudo is the escape to test.
    status, output = run("sudo", "sudo " + curl("/sudo-plain"), sudo=True)
    if status != 0 or "/sudo-plain" not in seen:
        controls.append("sudo curl failed without the launcher")

    status, output = run("launched", "ferrion-run " + curl("/launched"))
    if status != 0 or "/launched" not in seen:
        problems.append(f"ferrion-run without --no-network did not reach the network (exit {status})")

    status, output = run("nonet", "ferrion-run --no-network " + curl("/nonet"))
    error = re.search(r"curl: \(\d+\).*", output)
    if "/nonet" in seen or status in (0, 125, None) or not error:
        problems.append(f"curl under --no-network: exit {status}, no curl network error, or the host saw it")
    status, output = run("nonet-sudo", "ferrion-run --no-network sudo " + curl("/sudo-nonet"), sudo=True)
    if "/sudo-nonet" in seen or status in (0, 125, None):
        problems.append(f"ferrion-run --no-network sudo curl reached the network or did not run (exit {status})")
    status, output = run("profile", f"ferrion-run ferrion-run-probe connect {address} /probe-profile")
    if "/probe-profile" in seen or status != 1 or "cannot connect" not in output:
        problems.append(f"the profile /etc/ferrion-run/ferrion-run-probe.toml did not withhold the network (exit {status})")
    status, output = run("inherit", f"ferrion-run-probe hold-socket {address} -- ferrion-run --no-network "
                                    "--env FERRION_RUN_PROBE_FDS ferrion-run-probe use-fd /held-launched")
    if "/held-launched" in seen or status != 1 or re.search(r"descriptor \S+ usable:", output):
        problems.append(f"a socket opened before the launch worked in the program (exit {status})")

    status, output = run("stdout", "ferrion-run --no-network true > /tmp/ferrion-run-stdout")
    if status != 125 or "file on disk" not in output:
        problems.append(f"a disk file as standard output was not refused (exit {status})")
    status, output = run("stdin", f"ferrion-run-probe hold-socket {address} --stdin -- ferrion-run --no-network true")
    if status != 125 or "is a socket" not in output:
        problems.append(f"a socket as standard input was not refused (exit {status})")
    _, state_before = run("files-before", "ls -A /home/user/.local/state/ferrion-run 2>/dev/null | wc -l")
    status, output = run("files", "ferrion-run --files /home/user/no-such-folder ls")
    if status != 125 or "no-such-folder" not in output:
        problems.append(f"--files with a missing folder was not refused (exit {status})")
    _, state_after = run("files-after", "ls -A /home/user/.local/state/ferrion-run 2>/dev/null | wc -l")
    if state_before.split()[-1:] != state_after.split()[-1:]:
        problems.append(f"refused --files launch left a session entry ({state_before.split()[-1:]} -> {state_after.split()[-1:]})")

    (vm.out / "ferrion-run.txt").write_text("".join(transcript) + f"host saw: {seen}\n")
    if controls:
        return False, "control without the launcher failed, so the run proves nothing: " + "; ".join(controls) + f"; see {vm.out}"
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    return True, f"--no-network curl fails with {error.group(0).strip()!r}; held socket and profile denied; see {vm.out}"


def test_boot_gc_generation(args, out):
    """A copy whose RedoxFS generation is an allocator-GC threshold reaches the graphical login. Build RedoxFS's boot_gc_generation host example and install it in build/fstools/bin first."""
    helper = ROOT / "build" / "fstools" / "bin" / "boot_gc_generation"
    with tempfile.TemporaryDirectory(prefix="ferrion-vm-test-boot-gc-") as scratch:
        image = Path(scratch) / "harddrive.img"
        shutil.copyfile(ROOT / "build" / args.arch / args.config / "harddrive.img", image)
        prepared = subprocess.run([str(helper), str(image)], check=True,
                                  capture_output=True, text=True)
        (out / "generation.txt").write_text(prepared.stdout)
        vm = VM(args, out, make=[f"DISK={image}"])
        try:
            ok, note = test_boot(vm, args)
            return ok, f"{prepared.stdout.strip()}; {note}"
        finally:
            vm.close()


# Disk encryption (plans/disk-encryption.md DE2 and DE6). These tests boot copies of the image
# without -snapshot, because a key change or an installation must survive a reboot; the copies
# live in a temporary directory that is removed afterwards.
REDOXFS_KEY = ROOT / "build" / "fstools" / "bin" / "redoxfs-key"
KEY_SLOTS_AT = 80  # offset of the 64 key slots of 48 bytes in a version 8 RedoxFS header
SENDKEY = {" ": "spc", "-": "minus", "_": "shift-minus", ".": "dot", "/": "slash", "\n": "ret"}


def sendkeys(command, text):
    """Types text on QEMU's keyboard through the monitor, for the bootloader's prompt."""
    for char in text:
        key = SENDKEY.get(char) or (f"shift-{char.lower()}" if char.isupper() else char)
        command(f"sendkey {key}")
        time.sleep(0.03)


def raw_header_info(image):
    best = None
    with open(image, "rb") as f:
        offset = 0
        while block := f.read(4096):
            if block[:8] == b"RedoxFS\0" and int.from_bytes(block[8:16], "little") == 8:
                generation = int.from_bytes(block[40:48], "little")
                if best is None or generation > best[0]:
                    best = (generation, offset, block)
            offset += 4096
            if offset >= 64 << 20 and best:
                break
    if best is None:
        raise RuntimeError(f"no RedoxFS header in {image}")
    generation, offset, data = best
    ring_block = (offset // 4096) - (generation % 256)
    ring_sector = ring_block * 8
    slots = [data[KEY_SLOTS_AT + 48 * i:KEY_SLOTS_AT + 48 * (i + 1)] for i in range(64)]
    return ring_sector, generation, slots


def raw_key_slots(image):
    """The 64 key slots of the newest RedoxFS header in a raw image, as bytes."""
    return raw_header_info(image)[2]


def write_blkdebug_conf(path, sector, count, skip=0):
    sections = []
    if skip > 0:
        skip_entry = (
            "[inject-error]\n"
            'event = "write_aio"\n'
            'iotype = "write"\n'
            'errno = "0"\n'
            f'sector = "{sector}"\n'
            'once = "on"\n'
        )
        sections.extend(skip_entry for _ in range(skip))
    if count is None:
        sections.append(
            "[inject-error]\n"
            'event = "write_aio"\n'
            'iotype = "write"\n'
            'errno = "5"\n'
            f'sector = "{sector}"\n'
            'once = "off"\n'
        )
    else:
        entry = (
            "[inject-error]\n"
            'event = "write_aio"\n'
            'iotype = "write"\n'
            'errno = "5"\n'
            f'sector = "{sector}"\n'
            'once = "on"\n'
        )
        sections.extend(entry for _ in range(count))
    path.write_text("\n".join(sections))


def boot_blkdebug(args, out, disk, extra, disk_blk=None, extra_blk=None):
    out.mkdir(parents=True, exist_ok=True)
    d0 = f"blkdebug:{disk_blk}:{disk}" if disk_blk else str(disk)
    d1 = f"blkdebug:{extra_blk}:{extra}" if extra_blk else str(extra)
    flags = (
        f"-drive file={d0},format=raw,if=none,id=drv0 -device nvme,drive=drv0,serial=NVME_SERIAL "
        f"-drive file={d1},format=raw,if=none,id=drv1 -device nvme,drive=drv1,serial=NVME_EXTRA"
    )
    return VM(
        args,
        out,
        make=["disk=none", f"DISK={disk}", f"EXTRA_DISK={extra}"],
        qemu_flags=flags,
        snapshot=False,
    )


def config_passphrase(args):
    import tomllib
    path = ROOT / "config" / args.arch / f"{args.config}.toml"
    if not path.exists():
        path = ROOT / "config" / f"{args.config}.toml"
    passphrase = tomllib.loads(path.read_text()).get("general", {}).get("encrypt_disk")
    if not passphrase:
        sys.exit(f"vm-test: {path.relative_to(ROOT)} sets no general.encrypt_disk")
    return passphrase


def host_recovery_key(image, passphrase, scratch):
    key_file = scratch / "passphrase"
    key_file.write_text(passphrase)
    output = subprocess.run([str(REDOXFS_KEY), "add", "--recovery", "--key-file", str(key_file), str(image)],
                            check=True, capture_output=True, text=True).stdout
    key_file.unlink()
    return re.search(r"Recovery key[^:]*: ([0-9 ]+)", output).group(1).replace(" ", "")


def unlock(vm, passphrase, attempt, timeout):
    """Waits for the bootloader's prompt for this attempt and types the passphrase."""
    if not vm.wait_serial(rf"(?:RedoxFS p|P)assword \({attempt}/\d+\)", timeout):
        return False
    sendkeys(vm.command, passphrase + "\n")
    return True


def boot_copy(args, out, make):
    """Boots a copy of an image, given by DISK= and EXTRA_DISK=, that keeps what the guest writes."""
    out.mkdir(parents=True, exist_ok=True)
    return VM(args, out, make=make, snapshot=False)


PROMPT_REDRAW_S = 3


def login(vm, account, password, shell_re, since=0, timeout=60):
    if not vm.wait_serial(r"login: ", timeout, since):
        return False, "no login prompt"
    time.sleep(PROMPT_REDRAW_S)
    mark = vm.mark()
    vm.type(f"{account}\n")
    if password is not None:
        if not vm.wait_serial(r"(?i)password: ", 20, mark):
            return False, f"no password prompt for {account}"
        mark = vm.mark()
        vm.type(f"{password}\n")
    if shell_re is not None and not vm.wait_serial(shell_re, 20, mark):
        return False, f"no {account} shell"
    return True, None


def root_shell(vm, timeout):
    return login(vm, "root", "password", r"root:.*# ", since=0, timeout=timeout)[0]


def run_status(vm, line, timeout=120):
    """Runs a line in the guest's root shell; returns Answered(status, out) or NoAnswer(out)."""
    seq = vm._next_cmd_seq()
    since = vm.mark()
    vm.type(f"{line}; echo vm-test-status-{seq}-$?\n")
    found = vm.wait_serial(rf"(?s)\n(.*)vm-test-status-{seq}-(\d+)", timeout, since)
    if not found:
        vm.console_desynced = line
        return NoAnswer(vm.text(since))
    return Answered(int(found.group(2)), found.group(1))


MKFS_REUSE_MARK = b"FERRION-MKFS-REUSE"
MKFS_REUSE_MOUNT_POLLS = 20
MKFS_REUSE_POLL_S = 0.5


class MountState(enum.Enum):
    MOUNTED = "mounted"
    REFUSED = "refused"
    TIMED_OUT = "timed-out"
    NO_ANSWER = "no-answer"


@dataclass
class Mount:
    name: str
    pid_file: str
    state: MountState
    entries: int | None
    live: bool = False


def mkfs_reuse_mount(vm, disk, name, flags="", active=None):
    """Mounts the volume on `disk` at /scheme/<name>; returns a Mount with its own PID file and state."""
    assert active is None or not active.live, f"previous mount {active.name} is still live"
    pid_file = f"/tmp/mr-{name}.pid"
    start_reply = run_status(
        vm,
        f"bash -c 'rm -f {pid_file}; redoxfs -d {flags} {disk} {name} </dev/null >/tmp/{name}.log 2>&1 & "
        f"echo $! > {pid_file}'",
    )
    if isinstance(start_reply, NoAnswer):
        return Mount(name, pid_file, MountState.NO_ANSWER, None, live=True)
    for _ in range(MKFS_REUSE_MOUNT_POLLS):
        probe_reply = run_status(
            vm,
            f"bash -c 'if ls /scheme/{name} >/dev/null 2>&1; then echo mr-state-$((40+2))"
            f" $(ls -A /scheme/{name} | wc -l); elif grep -qi password: /tmp/{name}.log 2>/dev/null;"
            f" then echo mr-state-$((40+4)); elif kill -0 $(cat {pid_file} 2>/dev/null) 2>/dev/null;"
            f" then echo mr-state-$((40+3)); else echo mr-state-$((40+1)); fi'",
        )
        if isinstance(probe_reply, NoAnswer):
            return Mount(name, pid_file, MountState.NO_ANSWER, None, live=True)
        found = re.search(r"mr-state-42 +(\d+)|mr-state-44|mr-state-43|mr-state-41", probe_reply.out)
        if not found:
            return Mount(name, pid_file, MountState.NO_ANSWER, None, live=True)
        if found.group(1) is not None:
            return Mount(name, pid_file, MountState.MOUNTED, int(found.group(1)), live=True)
        if found.group(0) == "mr-state-44":
            run_status(vm, f"cat /tmp/{name}.log")
            return Mount(name, pid_file, MountState.REFUSED, None, live=True)
        if found.group(0) == "mr-state-41":
            run_status(vm, f"cat /tmp/{name}.log")
            return Mount(name, pid_file, MountState.REFUSED, None, live=False)
        time.sleep(MKFS_REUSE_POLL_S)
    run_status(vm, f"cat /tmp/{name}.log")
    return Mount(name, pid_file, MountState.TIMED_OUT, None, live=True)


def mkfs_reuse_unmount(vm, mount):
    if mount is None or not mount.live:
        return
    # SIGTERM is only seen after the scheme's next request; every commit is already on the disk
    # (the mount's disk cache writes through), so the daemon is stopped outright.
    run_status(
        vm,
        f"bash -c 'pid=$(cat {mount.pid_file} 2>/dev/null); [ -n \"$pid\" ] && kill -9 \"$pid\" 2>/dev/null; "
        f"rm -f {mount.pid_file}'; sleep 1",
    )
    mount.live = False


def test_mkfs_reuse(args, out):
    """redoxfs-mkfs over a volume that was mounted and written creates a volume whose next mount is new and empty, and redoxfs-mkfs --encrypt over an unencrypted volume asks for its passphrase (the extra disk, a 64 MiB scratch file)."""
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-mkfs-"))
    extra = scratch / "extra.img"
    try:
        extra.write_bytes(MKFS_REUSE_MARK)
        os.truncate(extra, 64 << 20)
        vm = VM(args, out, make=[f"EXTRA_DISK={extra}"])
        active = None
        try:
            if not root_shell(vm, args.timeout):
                return False, f"no root shell; see {vm.out}"
            # bash cannot glob /scheme, and head fails on a disk; list with ls and read one block with dd
            _, output = run_status(vm, "bash -c 'for s in $(ls /scheme | grep ^disk); do for e in $(ls /scheme/$s); do "
                                       "dd if=/scheme/$s/$e bs=4096 count=1 2>/dev/null"
                                       f" | grep -aq {MKFS_REUSE_MARK.decode()} && echo found:/scheme/$s/$e; done; done'")
            found = re.search(r"found:(/scheme/disk[^\s$']+)", output)
            if not found:
                return False, f"the extra disk was not found under /scheme/disk.*; see {vm.out}"
            disk = found.group(1)
            base_ps = run_status(vm, "ps")
            if isinstance(base_ps, NoAnswer):
                return False, f"no answer from ps before mounts; see {vm.out}"
            base_fs_pids = {
                fields[0] for line in base_ps.out.splitlines()
                if (fields := line.split()) and fields[0].isdigit() and any("redoxfs" in f for f in fields[1:])
            }

            status, output = run_status(vm, f"redoxfs-mkfs {disk}")
            if status != 0:
                return False, f"the first redoxfs-mkfs failed ({status}): {output.strip()[-200:]}"
            active = mkfs_reuse_mount(vm, disk, "mkfsreuse1", active=active)
            if active.state != MountState.MOUNTED:
                return False, f"the first volume did not mount ({active.state.value}); see {vm.out}"
            run_status(vm, "bash -c 'echo old-volume > /scheme/mkfsreuse1/old.txt; mkdir /scheme/mkfsreuse1/d; "
                           "for i in 1 2 3 4 5 6 7 8 9 10; do echo $i > /scheme/mkfsreuse1/d/$i; done'")
            mkfs_reuse_unmount(vm, active)
            status, output = run_status(vm, f"redoxfs-mkfs {disk}")
            if status != 0:
                return False, f"the second redoxfs-mkfs failed ({status}): {output.strip()[-200:]}"
            active = mkfs_reuse_mount(vm, disk, "mkfsreuse2", active=active)
            if active.state != MountState.MOUNTED:
                return False, f"the new volume did not mount ({active.state.value}; an older header won); see {vm.out}"
            if active.entries != 0:
                return False, f"the mount shows {active.entries} entries: the old volume, not the new one"
            run_status(vm, "echo plain-volume > /scheme/mkfsreuse2/plain.txt")
            mkfs_reuse_unmount(vm, active)

            status, output = run_status(vm, f"printf %s mkfs-reuse-pass > /tmp/mr.key; "
                                            f"redoxfs-mkfs --encrypt --key-file /tmp/mr.key {disk}")
            if status != 0:
                return False, f"redoxfs-mkfs --encrypt failed ({status}): {output.strip()[-200:]}"
            active = mkfs_reuse_mount(vm, disk, "mkfsreuse3", active=active)
            if active.state != MountState.REFUSED:
                return False, (
                    "the encrypted volume mounted with no passphrase: an older, unencrypted header won"
                    if active.state == MountState.MOUNTED
                    else f"the encrypted volume check without a passphrase did not finish ({active.state.value}); see {vm.out}"
                )
            mkfs_reuse_unmount(vm, active)
            active = mkfs_reuse_mount(vm, disk, "mkfsreuse4", "--key-file /tmp/mr.key", active=active)
            if active.state != MountState.MOUNTED or active.entries != 0:
                return False, f"the encrypted volume with its passphrase: state={active.state.value}, entries={active.entries}"
            mkfs_reuse_unmount(vm, active)
            ps_reply = run_status(vm, "ps")
            if isinstance(ps_reply, NoAnswer):
                return False, f"no answer from ps after unmount; see {vm.out}"
            end_fs_pids = {
                fields[0] for line in ps_reply.out.splitlines()
                if (fields := line.split()) and fields[0].isdigit() and any("redoxfs" in f for f in fields[1:])
            }
            leaked = sorted(end_fs_pids - base_fs_pids)
            if leaked:
                return False, f"check redoxfs PIDs still running after unmount: {leaked}; see {vm.out}"
        finally:
            if active is not None and active.live and vm.console_desynced is None:
                mkfs_reuse_unmount(vm, active)
            vm.close()
        return True, "second redoxfs-mkfs mounted new and empty; --encrypt over a plain volume refused without its passphrase, mounted empty with it"
    finally:
        shutil.rmtree(scratch)


def test_disk_keys(args, out):
    """DE2: a recovery key added on the host opens the volume at the bootloader; after `redoxfs-key change` on the mounted root and a reboot the old passphrase is refused and the new one accepted; no header keeps the old slot (use --config disk-encryption-test)."""
    passphrase, new = config_passphrase(args), "ferrion-disk-changed"
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-disk-"))
    image = scratch / "harddrive.img"
    try:
        shutil.copyfile(ROOT / "build" / args.arch / args.config / "harddrive.img", image)
        (scratch / "extra.img").write_bytes(b"")
        os.truncate(scratch / "extra.img", 64 << 20)
        disk = [f"DISK={image}", f"EXTRA_DISK={scratch / 'extra.img'}"]
        recovery = host_recovery_key(image, passphrase, scratch)
        old_slot = raw_key_slots(image)[0]

        vm = boot_copy(args, out / "recovery", disk)
        try:
            if not unlock(vm, recovery, 1, args.timeout):
                return False, "no bootloader passphrase prompt"
            if not root_shell(vm, args.timeout):
                return False, f"the recovery key did not reach the login; see {vm.out}"
            run_status(vm, f"printf %s {passphrase} > /tmp/old; printf %s {new} > /tmp/new")
            status, output = run_status(vm, "redoxfs-key change --key-file /tmp/old --new-key-file /tmp/new /")
            run_status(vm, "rm /tmp/old /tmp/new")
            (vm.out / "change.txt").write_text(output)
            if status != 0:
                return False, f"redoxfs-key change on / failed ({status}); see {vm.out / 'change.txt'}"
        finally:
            vm.close()

        vm = boot_copy(args, out / "changed", disk)
        try:
            if not unlock(vm, passphrase, 1, args.timeout):
                return False, "no bootloader passphrase prompt after the change"
            if not unlock(vm, new, 2, 60):
                return False, f"the old passphrase was not refused; see {vm.out}"
            if not vm.wait_serial(r"login: ", args.timeout):
                return False, f"the new passphrase did not reach the login; see {vm.out}"
        finally:
            vm.close()

        slots = raw_key_slots(image)
        if slots[0] == old_slot or image.read_bytes().find(old_slot) != -1:
            return False, "the old slot's bytes are still in the image"
        in_use = sum(slot[:16] != bytes(16) for slot in slots)
        return True, f"recovery key opened it; old passphrase refused, new accepted; old slot in no header; {in_use} slots in use"
    finally:
        shutil.rmtree(scratch)


def test_disk_install(args, out):
    """DE6: the live image's text installer installs to a second disk with one passphrase; that disk starts with the passphrase and with the recovery key the installer printed; its filesystem.toml holds no passphrase and its raw bytes no file name (use --config disk-encryption-test after `make live`)."""
    import tomllib

    def read_guest_toml(vm, path):
        status, out_text = run_status(vm, f"echo TOML-BEGIN; cat {path}; echo TOML-END")
        if status != 0:
            return None, out_text
        match = re.search(r"(?ms)^TOML-BEGIN\n(.*)\nTOML-END$", out_text)
        return (match.group(1) if match else None), out_text

    passphrase, installed = config_passphrase(args), "ferrion-installed-disk"
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-install-"))
    live, target, extra = scratch / "live.iso", scratch / "target.img", scratch / "extra.img"
    try:
        shutil.copyfile(ROOT / "build" / args.arch / args.config / "redox-live.iso", live)
        for path, size in ((target, 2 << 30), (extra, 64 << 20)):
            path.write_bytes(b"")
            os.truncate(path, size)

        vm = boot_copy(args, out / "install", [f"DISK={live}", f"EXTRA_DISK={target}"])
        try:
            if not unlock(vm, passphrase, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                return False, f"the live image did not reach a root shell; see {vm.out}"
            live_toml, live_raw = read_guest_toml(vm, "/filesystem.toml")
            if live_toml is None:
                return False, f"could not read live /filesystem.toml: {live_raw[-200:]!r}; see {vm.out}"
            since = vm.mark()
            vm.type("redox_installer_tui\n")
            # lived logs "removing handle: ..." on the same console as the installer closes its
            # handles; that line can land between a drive's number and its path.
            choice = vm.wait_serial(r"(\d+): (?:removing handle[^\n]*\n)?\S+(?:removing handle[^\n]*\n)?: 2\.00 GB", args.timeout, since)
            if not choice or not vm.wait_serial(r"Select a drive", 10, since):
                return False, f"the installer did not offer the 2 GB disk; see {vm.out}"
            vm.type(choice.group(1) + "\n")
            if not vm.wait_serial(r"Encrypt this disk\? \[Y/n\]", 30, since):
                return False, "the installer did not ask about encryption"
            vm.type("\n")
            if not vm.wait_serial(r"Account name \[\S+\]: ", 30, since):
                return False, "the installer did not ask for the account name"
            vm.type("\n")
            for prompt in (r"Password for \S+: ", r"Confirm password: "):
                if not vm.wait_serial(prompt, 30, since):
                    return False, f"no {prompt.strip()} prompt; see {vm.out}"
                vm.type(installed + "\n")
                since = vm.mark()
            key = vm.wait_serial(r"Recovery key[^:]*: ([0-9 ]{55})|failed to install: .*", 900, since)
            if not key or not key.group(1) or not vm.wait_serial(r"Type SAVED", 30, since):
                return False, f"the installer printed no recovery key: {key.group(0) if key else 'timeout'}; see {vm.out}"
            recovery = key.group(1).replace(" ", "")
            vm.type("SAVED\n")
            if not vm.wait_serial(r"root:.*# ", 120, since):
                return False, f"the installer did not finish; see {vm.out}"
        finally:
            vm.close()

        notes = []
        for name, secret in (("passphrase", installed), ("recovery", recovery)):
            vm = boot_copy(args, out / name, [f"DISK={target}", f"EXTRA_DISK={extra}"])
            try:
                if not unlock(vm, secret, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                    return False, f"the installed disk did not start with its {name}; see {vm.out}"
                if name == "passphrase":
                    status, output = run_status(vm, f"grep -c -e encrypt_disk -e unlock_account -e {installed} -e {passphrase} /filesystem.toml")
                    if status != 1:
                        return False, f"the installed filesystem.toml holds encryption fields or a passphrase ({status}): {output!r}"
                    inst_toml, inst_raw = read_guest_toml(vm, "/filesystem.toml")
                    if inst_toml is None:
                        return False, f"could not read installed /filesystem.toml: {inst_raw[-200:]!r}; see {vm.out}"
                    expected_cfg = tomllib.loads(live_toml)
                    expected_cfg.get("general", {}).pop("encrypt_disk", None)
                    expected_cfg.get("general", {}).pop("unlock_account", None)
                    if "user" in expected_cfg.get("users", {}):
                        expected_cfg["users"]["user"].pop("password", None)
                    if tomllib.loads(inst_toml) != expected_cfg:
                        (vm.out / "live-filesystem.toml").write_text(live_toml)
                        (vm.out / "installed-filesystem.toml").write_text(inst_toml)
                        return False, f"installed filesystem.toml differed beyond stripped encryption fields; see {vm.out}"
                notes.append(f"starts with its {name}")
            finally:
                vm.close()

        key_file = scratch / "passphrase"
        key_file.write_text(installed)
        listing = subprocess.run([str(REDOXFS_KEY), "list", "--key-file", str(key_file), str(target)],
                                 check=True, capture_output=True, text=True).stdout
        (out / "slots.txt").write_text(listing)
        if listing.count("in use") != 2:
            return False, f"expected two slots in use: {listing!r}"
        raw = target.read_bytes()
        names = [name for name in (b"filesystem.toml", b"os-release", b"redoxfs-key") if name in raw]
        if names:
            return False, f"file names readable in the raw disk: {names}"

        unenc = scratch / "unenc.img"
        unenc.write_bytes(b"")
        os.truncate(unenc, 2 << 30)
        gen_line = next(
            (i + 1 for i, line in enumerate(live_toml.splitlines()) if line.strip() == "[general]"),
            None,
        )
        if gen_line is None:
            return False, "live /filesystem.toml has no [general] header for unencrypted install check"
        vm = boot_copy(args, out / "unenc-install", [f"DISK={live}", f"EXTRA_DISK={unenc}"])
        try:
            if not unlock(vm, passphrase, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                return False, f"live image did not reach root shell for unencrypted install; see {vm.out}"
            for cmd in (
                f"head -n {gen_line} /filesystem.toml > /tmp/fs.toml",
                "echo 'unlock_account = \"user\"' >> /tmp/fs.toml",
                f"tail -n +{gen_line + 1} /filesystem.toml >> /tmp/fs.toml",
                "cp /tmp/fs.toml /filesystem.toml",
                "rm -f /tmp/fs.toml",
            ):
                rc, out_cmd = run_status(vm, cmd)
                if rc != 0:
                    return False, f"failed to stage stale unlock_account ({cmd}, rc={rc}): {out_cmd!r}"
            since = vm.mark()
            vm.type("redox_installer_tui\n")
            choice = vm.wait_serial(r"(\d+): (?:removing handle[^\n]*\n)?\S+(?:removing handle[^\n]*\n)?: 2\.00 GB", args.timeout, since)
            if not choice or not vm.wait_serial(r"Select a drive", 10, since):
                return False, f"installer did not offer 2 GB disk for unencrypted install; see {vm.out}"
            vm.type(choice.group(1) + "\n")
            if not vm.wait_serial(r"Encrypt this disk\? \[Y/n\]", 30, since):
                return False, f"installer did not ask about encryption for unencrypted install; see {vm.out}"
            since = vm.mark()
            vm.type("n\n")
            if not vm.wait_serial(r"installed successfully|failed to install: .*", 900, since):
                return False, f"unencrypted install timed out; see {vm.out}"
            if "failed to install" in vm.text(since):
                return False, f"unencrypted install failed; see {vm.out}"
            if not vm.wait_serial(r"root:.*# ", 120, since):
                return False, f"unencrypted install did not return to root shell; see {vm.out}"
        finally:
            vm.close()

        if b"UNLOCK  TXT" not in target.read_bytes()[2048 * 512:4096 * 512]:
            return False, "encrypted install did not write UNLOCK.TXT into the EFI partition"
        efi_bytes = unenc.read_bytes()[2048 * 512:4096 * 512]
        if b"UNLOCK  TXT" in efi_bytes:
            return False, "unencrypted install wrote UNLOCK.TXT into the EFI partition"

        vm = boot_copy(args, out / "unenc-boot", [f"DISK={unenc}", f"EXTRA_DISK={extra}"])
        try:
            if not root_shell(vm, args.timeout * 2):
                return False, f"unencrypted installed disk did not reach root shell; see {vm.out}"
            status_sl, _ = run_status(vm, "ls -l /etc/unlock-slots")
            if status_sl == 0:
                return False, f"unencrypted install created /etc/unlock-slots; see {vm.out}"
            unenc_toml, unenc_raw = read_guest_toml(vm, "/filesystem.toml")
            if unenc_toml is None:
                return False, f"could not read unencrypted /filesystem.toml: {unenc_raw[-200:]!r}; see {vm.out}"
            unenc_gen = tomllib.loads(unenc_toml).get("general", {})
            if "encrypt_disk" in unenc_gen or "unlock_account" in unenc_gen:
                return False, f"unencrypted filesystem.toml kept encryption fields: {unenc_gen!r}"
            notes.append("unencrypted install cleared stale unlock_account with no UNLOCK.TXT or /etc/unlock-slots")
        finally:
            vm.close()

        small = scratch / "small.img"
        small.write_bytes(b"")
        os.truncate(small, 48 << 20)
        vm = boot_copy(args, out / "small-fail", [f"DISK={live}", f"EXTRA_DISK={small}"])
        try:
            if not unlock(vm, passphrase, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                return False, f"live image did not reach root shell for small-disk failure check; see {vm.out}"
            since = vm.mark()
            vm.type("redox_installer_tui; echo small-install-rc-$?\n")
            choice = vm.wait_serial(r"(\d+): (?:removing handle[^\n]*\n)?\S+(?:removing handle[^\n]*\n)?: 48\.0 MB", args.timeout, since)
            if not choice or not vm.wait_serial(r"Select a drive", 10, since):
                return False, f"installer did not offer 48 MB disk; see {vm.out}"
            vm.type(choice.group(1) + "\n")
            if not vm.wait_serial(r"Encrypt this disk\? \[Y/n\]", 30, since):
                return False, f"installer did not ask about encryption on 48 MB disk; see {vm.out}"
            since = vm.mark()
            vm.type("n\n")
            done = vm.wait_serial(r"small-install-rc-(\d+)", 300, since)
            transcript = vm.text(since)
            (vm.out / "small-fail.txt").write_text(transcript)
            if not done or int(done.group(1)) == 0:
                return False, f"48 MB install did not fail cleanly (match={done}); see {vm.out / 'small-fail.txt'}"
            if not re.search(r"failed to install:.*(?:failed to write|failed to create|No space left|ENOSPC)", transcript, re.S):
                return False, f"48 MB install error did not preserve the copy failure; see {vm.out / 'small-fail.txt'}"
            notes.append("mid-copy failure unmounted cleanly and preserved the copy error")
        finally:
            vm.close()

        return True, "; ".join(notes) + "; two slots in use; no file name in the raw disk"
    finally:
        shutil.rmtree(scratch)


def one_password_install(args, out, live, target, password):
    vm = boot_copy(args, out, [f"DISK={live}", f"EXTRA_DISK={target}"])
    try:
        if not unlock(vm, config_passphrase(args), 1, args.timeout) or not root_shell(vm, args.timeout * 2):
            return None
        since = vm.mark()
        vm.type("redox_installer_tui\n")
        choice = vm.wait_serial(
            r"(\d+)(?:removing handle[^\n]*\n)?: (?:removing handle[^\n]*\n)?\S+(?:removing handle[^\n]*\n)?: 2\.00 GB",
            args.timeout,
            since,
        )
        if not choice or not vm.wait_serial(r"Select a drive", 10, since):
            return None
        vm.type(choice.group(1) + "\n")
        for prompt, answer in ((r"Encrypt this disk\? \[Y/n\]", ""), (r"Account name \[user\]: ", ""),
                               (r"Password for user: ", password), (r"Confirm password: ", password)):
            if not vm.wait_serial(prompt, 30, since):
                return None
            vm.type(answer + "\n")
            since = vm.mark()
        key = vm.wait_serial(r"Recovery key[^:]*: ([0-9 ]{55})|failed to install: .*", 900, since)
        if not key or not key.group(1) or not vm.wait_serial(r"Type SAVED", 30, since):
            return None
        vm.type("SAVED\n")
        if not vm.wait_serial(r"root:.*# ", 120, since):
            return None
        return key.group(1).replace(" ", "")
    finally:
        vm.close()


def session_owner(vm):
    reply = run_status(vm, "grep launcher /scheme/sys/context")
    if isinstance(reply, NoAnswer):
        return reply
    found = re.search(r"(?m)^\s*\d+\s+(\d+)\s.*launcher\s*$", reply.out)
    return int(found.group(1)) if found else None


def wait_session(vm, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        owner = session_owner(vm)
        if isinstance(owner, NoAnswer):
            raise RuntimeError("session_owner gave no answer")
        if owner is not None:
            return owner
        time.sleep(2)
    return None


def screen_kind_within(vm, kinds, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        kind = vm.screenshot().kind()
        if kind in kinds:
            return kind
        time.sleep(0.5)
    return None


def test_one_password(args, out):
    """DE6a + issues 01/02: the installer's one password opens the disk at the bootloader, whose prompt names the account, and reaches that account's desktop with no second prompt; the recovery key reaches the login screen with nobody logged in; a logout and a second login ask; after passwd as that account and a reboot the old password is refused and the new one opens the disk and logs in; after root resets the account's password, passwd asks for the previous disk password and rewraps the slot; a second account added with useradd cannot unlock until root runs passwd --enroll, then unlocks into its own session, and userdel revokes its slot and wipes its old bytes from the image (use --config disk-encryption-test after `make live`)."""
    import mmap
    password, new = "ferrion-one-pass", "ferrion-one-changed"
    admin_reset, recovered = "ferrion-admin-reset", "ferrion-one-recovered"
    second_user, second_pass = "second", "ferrion-second-pass"
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-onepw-"))
    live, target, extra = scratch / "live.iso", scratch / "target.img", scratch / "extra.img"
    disk = [f"DISK={target}", f"EXTRA_DISK={extra}"]
    notes = []

    def run_prompts(vm, cmd, prompts, done_re, marker, transcript_name, timeout=120):
        since = vm.mark()
        vm.type(cmd + "\n")
        for prompt, answer in prompts:
            if not vm.wait_serial(prompt, 30, since):
                (vm.out / transcript_name).write_text(vm.text(since))
                return None, f"{cmd} did not ask {prompt!r}; see {vm.out / transcript_name}"
            vm.type(answer + "\n")
            since = vm.mark()
        vm.wait_serial(done_re, timeout, since)
        time.sleep(1)
        vm.type(f"echo {marker}-$?\n")
        status = vm.wait_serial(rf"{marker}-(\d+)", 60, since)
        text = vm.text(since)
        (vm.out / transcript_name).write_text(text)
        if not status:
            return None, f"{cmd} produced no exit status; see {vm.out / transcript_name}"
        return int(status.group(1)), text

    try:
        shutil.copyfile(ROOT / "build" / args.arch / args.config / "redox-live.iso", live)
        for path, size in ((target, 2 << 30), (extra, 64 << 20)):
            path.write_bytes(b"")
            os.truncate(path, size)
        recovery = one_password_install(args, out / "install", live, target, password)
        if not recovery:
            return False, f"the installer did not finish with one password; see {out / 'install'}"

        user_uid = None
        vm = boot_copy(args, out / "password", disk)
        try:
            if not vm.wait_serial(r"user[\s\S]{0,400}Password \(1/\d+\)", args.timeout):
                return False, f"the bootloader prompt does not name the account; see {vm.out}"
            sendkeys(vm.command, password + "\n")
            if not root_shell(vm, args.timeout * 2):
                return False, f"no root shell on the serial console; see {vm.out}"
            owner = wait_session(vm, args.timeout)
            frame = settle(vm, 20, still=2.0)
            frame.save_png(vm.out / "desktop.png")
            _, env = run_status(vm, "grep REDOXFS_UNLOCK /scheme/sys/env")
            _, uid = run_status(vm, "grep '^user;' /etc/passwd")
            uid = re.search(r"user;(\d+);", uid)
            if ("REDOXFS_UNLOCK_KIND=passphrase" not in env or not uid or owner != int(uid.group(1))
                    or frame.kind() == "login"):
                return False, (f"no session of user after the unlock (launcher euid {owner}, screen {frame.kind()}, "
                               f"env {env.strip()!r}); see {vm.out}")
            user_uid = int(uid.group(1))
            notes.append("bootloader password reached user's desktop")
            _, context = run_status(vm, "grep launcher /scheme/sys/context")
            for pid in re.findall(r"(?m)^\s*(\d+)\s+\d+\s.*launcher\s*$", context):
                run_status(vm, f"kill {pid}")
            if screen_kind_within(vm, ("login",), args.timeout) is None:
                return False, f"after the session ended the login screen did not come back; see {vm.out}"
            notes.append("logout shows the login")
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no serial login after root's exit; see {vm.out}"
            time.sleep(3)
            since = vm.mark()
            vm.type("user\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                return False, f"a second login as user did not ask for the password; see {vm.out}"
            vm.type(password + "\n")
            time.sleep(3)
            code, err = run_prompts(
                vm,
                "passwd",
                ((r"current password: ", password), (r"new password: ", new),
                 (r"confirm password: ", new)),
                r"user:\S*\$ ",
                "passwd-status",
                "passwd.txt",
            )
            if code != 0:
                return False, f"passwd as user failed ({code}): {err}"
            notes.append("second login asked; passwd changed the password")
        finally:
            vm.close()

        vm = boot_copy(args, out / "recovery", disk)
        try:
            if not unlock(vm, recovery, 1, args.timeout):
                return False, "no bootloader prompt for the recovery key"
            if screen_kind_within(vm, ("login",), args.timeout * 2) is None:
                return False, f"the recovery key did not end at the login screen; see {vm.out}"
            settle(vm, 10).save_png(vm.out / "login.png")
            if not root_shell(vm, args.timeout):
                return False, f"no root shell after the recovery key; see {vm.out}"
            owner = session_owner(vm)
            if isinstance(owner, NoAnswer):
                return False, f"session_owner gave no answer after a recovery-key unlock; see {vm.out}"
            if owner is not None:
                return False, f"a session (euid {owner}) started after a recovery-key unlock; see {vm.out}"
            notes.append("recovery key ends at the login with nobody logged in")
        finally:
            vm.close()

        vm = boot_copy(args, out / "changed", disk)
        try:
            if not unlock(vm, password, 1, args.timeout):
                return False, "no bootloader prompt after passwd"
            if not unlock(vm, new, 2, 60):
                return False, f"the old password was not refused at the bootloader; see {vm.out}"
            if not root_shell(vm, args.timeout * 2) or wait_session(vm, args.timeout) != user_uid:
                return False, f"the new password did not log user in; see {vm.out}"
            notes.append("after passwd the old password is refused and the new one opens and logs in")

            code, err = run_prompts(
                vm,
                "passwd user",
                ((r"new password: ", admin_reset), (r"confirm password: ", admin_reset)),
                r"root:.*# ",
                "admin-reset-status",
                "admin-reset.txt",
            )
            if code != 0:
                return False, f"root passwd user failed ({code}): {err}"
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no serial login after admin reset; see {vm.out}"
            time.sleep(3)
            since = vm.mark()
            vm.type("user\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                return False, f"login as user after admin reset did not ask for password; see {vm.out}"
            vm.type(admin_reset + "\n")
            time.sleep(3)
            code, err = run_prompts(
                vm,
                "passwd",
                ((r"current password: ", admin_reset), (r"new password: ", recovered),
                 (r"confirm password: ", recovered), (r"previous disk password: ", new)),
                r"user:\S*\$ ",
                "passwd-recover-status",
                "passwd-recover.txt",
            )
            if code != 0:
                return False, f"passwd with previous disk password failed ({code}): {err}"
        finally:
            vm.close()

        vm = boot_copy(args, out / "admin-reset", disk)
        try:
            if not unlock(vm, new, 1, args.timeout):
                return False, "no bootloader prompt after admin-reset recovery"
            if not unlock(vm, recovered, 2, 60):
                return False, f"the pre-reset disk password was not refused; see {vm.out}"
            if not root_shell(vm, args.timeout * 2) or wait_session(vm, args.timeout) != user_uid:
                return False, f"the recovered password did not open and log user in; see {vm.out}"
            notes.append("after admin reset, passwd with previous disk password rewraps the slot and logs user in")

            status, output = run_status(vm, f"useradd -m -s /usr/bin/ion {second_user}")
            if status != 0:
                return False, f"useradd {second_user} failed ({status}): {output!r}"
            code, err = run_prompts(
                vm,
                f"passwd {second_user}",
                ((r"new password: ", second_pass), (r"confirm password: ", second_pass)),
                r"root:.*# ",
                "second-passwd-status",
                "second-passwd.txt",
            )
            if code != 0:
                return False, f"root passwd {second_user} failed ({code}): {err}"
            reply = run_status(vm, f"grep ' {second_user}$' /etc/unlock-slots")
            if not refused(reply):
                return False, (
                    f"{second_user} was enrolled automatically before passwd --enroll"
                    if isinstance(reply, Answered)
                    else f"unlock-slots check for {second_user} gave no answer"
                )
        finally:
            vm.close()

        second_slot = None
        vm = boot_copy(args, out / "second-unenrolled", disk)
        try:
            if not unlock(vm, second_pass, 1, args.timeout):
                return False, "no bootloader prompt for unenrolled second account check"
            if not unlock(vm, recovered, 2, 60):
                return False, f"unenrolled {second_user}'s password was not refused at the bootloader; see {vm.out}"
            if not root_shell(vm, args.timeout * 2):
                return False, f"no root shell after second-unenrolled check; see {vm.out}"
            if wait_session(vm, args.timeout) != user_uid:
                return False, f"user session did not start before claim-integrity check; see {vm.out}"
            notes.append("unenrolled second account refused at bootloader")

            _, pre_claim = run_status(vm, "ls -ld /var/run/unlock-session /var/run/unlock-session/claimed")
            (vm.out / "claim-check.txt").write_text(f"pre:\n{pre_claim}\n")
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no serial login for second claim check; see {vm.out}"
            time.sleep(2)
            since = vm.mark()
            vm.type(f"{second_user}\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                return False, f"no password prompt for {second_user}; see {vm.out}"
            vm.type(second_pass + "\n")
            if not vm.wait_serial(rf"{second_user}:.*\$ ", 20, since):
                return False, f"no shell for {second_user}; see {vm.out}"
            for cmd in (
                "cp /etc/hostname /var/run/unlock-session/claimed",
                "rm -f /var/run/unlock-session/claimed",
                "mv /var/run/unlock-session/claimed /tmp/claim-moved-1",
            ):
                run_status(vm, cmd, timeout=30)
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no serial login after {second_user} exit; see {vm.out}"
            time.sleep(2)
            since = vm.mark()
            vm.type("user\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                (vm.out / "claim-check.txt").write_text(f"pre:\n{pre_claim}\nafter-second:\n{vm.text(since)}\n")
                return False, f"user logged in without password after {second_user} touched claim marker; see {vm.out}"
            vm.type(recovered + "\n")
            if not vm.wait_serial(r"user:\S*\$ ", 20, since):
                return False, f"no user shell after password prompt in claim check; see {vm.out}"
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no serial login after user exit in claim check; see {vm.out}"
            time.sleep(2)
            since = vm.mark()
            vm.type("root\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                return False, f"no root password prompt after claim check; see {vm.out}"
            vm.type("password\n")
            if not vm.wait_serial(r"root:.*# ", 20, since):
                return False, f"no root shell after claim check; see {vm.out}"
            _, post_claim = run_status(vm, "ls -ld /var/run/unlock-session /var/run/unlock-session/claimed")
            (vm.out / "claim-check.txt").write_text(f"pre:\n{pre_claim}\npost:\n{post_claim}\n")
            if "drwx------" not in post_claim or "/var/run/unlock-session/claimed" not in post_claim:
                return False, f"claim marker not in root-only 0700 /var/run/unlock-session: {post_claim!r}"
            run_status(vm, "rm -rf /var/run/unlock-session")
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no serial login for fail-closed claim check; see {vm.out}"
            time.sleep(2)
            since = vm.mark()
            vm.type("user\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                return False, f"login did not fail closed when /var/run/unlock-session was missing; see {vm.out}"
            vm.type(recovered + "\n")
            if not vm.wait_serial(r"user:\S*\$ ", 20, since):
                return False, f"no user shell after fail-closed check; see {vm.out}"
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no serial login after fail-closed user exit; see {vm.out}"
            time.sleep(2)
            since = vm.mark()
            vm.type("root\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                return False, f"no root password prompt after fail-closed check; see {vm.out}"
            vm.type("password\n")
            if not vm.wait_serial(r"root:.*# ", 20, since):
                return False, f"no root shell after fail-closed claim check; see {vm.out}"
            reply_dir = run_status(vm, "ls -ld /var/run/unlock-session")
            if not refused(reply_dir):
                return False, (
                    "login recreated missing /var/run/unlock-session instead of failing closed"
                    if isinstance(reply_dir, Answered)
                    else "unlock-session fail-closed check gave no answer"
                )
            notes.append("claim marker protected from second account and fails closed when directory is missing")

            reply = run_status(vm, "passwd --enroll root")
            if not refused(reply):
                return False, (
                    "passwd --enroll root succeeded unexpectedly"
                    if isinstance(reply, Answered)
                    else "passwd --enroll root gave no answer"
                )
            code, err = run_prompts(
                vm,
                f"passwd --enroll {second_user}",
                ((rf"password for {second_user}: ", second_pass),
                 (r"disk password or recovery key: ", recovered)),
                r"root:.*# ",
                "enroll-status",
                "enroll.txt",
            )
            if code != 0:
                return False, f"passwd --enroll {second_user} failed ({code}): {err}"
            _, slots_text = run_status(vm, "cat /etc/unlock-slots")
            match = re.search(rf"(?m)^(\d+) {second_user}$", slots_text)
            if not match:
                return False, f"{second_user} not recorded in /etc/unlock-slots: {slots_text!r}"
            second_slot = int(match.group(1))
        finally:
            vm.close()

        second_slot_bytes = raw_key_slots(target)[second_slot]
        if second_slot_bytes[:16] == bytes(16):
            return False, f"enrolled slot {second_slot} for {second_user} is empty on disk"

        vm = boot_copy(args, out / "second-enrolled", disk)
        try:
            if not unlock(vm, second_pass, 1, args.timeout):
                return False, f"no bootloader prompt for enrolled {second_user}"
            if not root_shell(vm, args.timeout * 2):
                return False, f"enrolled {second_user} password did not unlock the disk; see {vm.out}"
            owner = wait_session(vm, args.timeout)
            _, env = run_status(vm, "grep REDOXFS_UNLOCK /scheme/sys/env")
            _, uid = run_status(vm, f"grep '^{second_user};' /etc/passwd")
            uid = re.search(rf"{second_user};(\d+);", uid)
            if (f"REDOXFS_UNLOCK_SLOT={second_slot}" not in env
                    or "REDOXFS_UNLOCK_KIND=passphrase" not in env
                    or not uid or owner != int(uid.group(1))):
                return False, (f"enrolled {second_user} did not launch its session (launcher euid {owner}, "
                               f"expected {uid.group(1) if uid else None}, env {env.strip()!r}); see {vm.out}")
            notes.append(f"passwd --enroll unlocked and launched {second_user}'s session")

            _, ctx_out = run_status(vm, "cat /scheme/sys/context")
            pids = [int(m) for m in re.findall(r"(?m)^\s*(\d+)\s+\d+\s", ctx_out)]
            max_pid = max(pids) if pids else 0
            block_paths = [f"/etc/.unlock-slots.tmp.{pid}" for pid in range(max_pid + 1, max_pid + 51)]
            for start in range(0, len(block_paths), 5):
                run_status(vm, "mkdir " + " ".join(block_paths[start:start + 5]))
            code_blk, err_blk = run_prompts(
                vm,
                f"userdel {second_user}",
                ((r"disk password or recovery key: ", recovered),),
                r"root:.*# ",
                "userdel-blk-status",
                "userdel-blk.txt",
            )
            for start in range(0, len(block_paths), 5):
                run_status(vm, "rmdir " + " ".join(block_paths[start:start + 5]))
            if code_blk == 0:
                return False, f"userdel {second_user} succeeded while unlock-slots write was blocked: {err_blk}"
            status_pw_blk, _ = run_status(vm, f"grep '^{second_user};' /etc/passwd")
            status_sl_blk, _ = run_status(vm, f"grep '^{second_slot} {second_user}$' /etc/unlock-slots")
            run_status(vm, f"printf %s {recovered} > /tmp/rec.key")
            status_list_blk, list_blk = run_status(vm, "redoxfs-key list --key-file /tmp/rec.key /")
            run_status(vm, "rm -f /tmp/rec.key")
            if (status_pw_blk != 0 or status_sl_blk != 0 or status_list_blk != 0
                    or re.search(rf"(?m)^slot {second_slot}: in use\b", list_blk)):
                return False, (f"failed userdel left state inconsistent: passwd={status_pw_blk}, "
                               f"unlock-slots={status_sl_blk}, list_rc={status_list_blk}, list={list_blk!r}")
            notes.append("failed userdel cleared disk slot while keeping account and unlock-slots record for retry")

            code, err = run_prompts(
                vm,
                f"userdel {second_user}",
                ((r"disk password or recovery key: ", recovered),),
                r"root:.*# ",
                "userdel-status",
                "userdel.txt",
            )
            if code != 0:
                return False, f"userdel {second_user} failed ({code}): {err}"
            reply_pw = run_status(vm, f"grep '^{second_user};' /etc/passwd")
            reply_sl = run_status(vm, f"grep ' {second_user}$' /etc/unlock-slots")
            if not refused(reply_pw) or not refused(reply_sl):
                return False, f"userdel left {second_user} in passwd ({reply_pw[0]}) or unlock-slots ({reply_sl[0]})"
        finally:
            vm.close()

        if raw_key_slots(target)[second_slot] != bytes(48):
            return False, f"slot {second_slot} was not cleared in the newest header after userdel"
        with target.open("rb") as disk_file:
            with mmap.mmap(disk_file.fileno(), 0, access=mmap.ACCESS_READ) as mm:
                if mm.find(second_slot_bytes) != -1:
                    return False, f"removed slot {second_slot}'s old bytes are still in the image"

        vm = boot_copy(args, out / "second-removed", disk)
        try:
            if not unlock(vm, second_pass, 1, args.timeout):
                return False, "no bootloader prompt after userdel"
            if not unlock(vm, recovered, 2, 60):
                return False, f"removed {second_user}'s password was not refused at the bootloader; see {vm.out}"
            if not root_shell(vm, args.timeout * 2):
                return False, f"no root shell after userdel reboot; see {vm.out}"
            notes.append("userdel revoked the slot and old slot bytes are absent from the image")
        finally:
            vm.close()

        return True, "; ".join(notes)
    finally:
        shutil.rmtree(scratch)


def test_key_slot_roll(args, out):
    """Issue 36: mid-roll NVMe write errors injected via QEMU blkdebug across enroll, change, remove and installer recovery-key creation preserve the key-slot invariants and refuse removed accounts at bootloader unlock (use --config disk-encryption-test after `make live`)."""
    import mmap
    passphrase = config_passphrase(args)
    inst_pass = "ferrion-roll-pass1"
    changed_pass = "ferrion-roll-pass2"
    second_user, second_pass = "second", "ferrion-roll-second"
    third_user, third_pass = "third", "ferrion-roll-third"
    boot_commit_limit = 64
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-keyroll-"))
    live = scratch / "live.iso"
    target = scratch / "target.img"
    extra = scratch / "extra.img"
    notes = []

    def run_prompts(vm, cmd, prompts, done_re, marker, transcript_name, timeout=180):
        since = vm.mark()
        vm.type(cmd + "\n")
        for prompt, answer in prompts:
            if not vm.wait_serial(prompt, 30, since):
                (vm.out / transcript_name).write_text(vm.text(since))
                return None, f"{cmd} did not ask {prompt!r}; see {vm.out / transcript_name}"
            vm.type(answer + "\n")
            since = vm.mark()
        vm.wait_serial(done_re, timeout, since)
        time.sleep(1)
        vm.type(f"echo {marker}-$?\n")
        status = vm.wait_serial(rf"{marker}-(\d+)", 60, since)
        text = vm.text(since)
        (vm.out / transcript_name).write_text(text)
        if not status:
            return None, f"{cmd} produced no exit status; see {vm.out / transcript_name}"
        return int(status.group(1)), text

    def check_boot_budget(image, g0, label):
        _, g_start, _ = raw_header_info(image)
        delta = g_start - g0
        if delta < 0 or delta > boot_commit_limit:
            return None, f"setup invalid ({label}): G0={g0}, G_start={g_start} (delta {delta} > {boot_commit_limit})"
        return g_start, None

    try:
        shutil.copyfile(ROOT / "build" / args.arch / args.config / "redox-live.iso", live)
        for path, size in ((target, 2 << 30), (extra, 64 << 20)):
            path.write_bytes(b"")
            os.truncate(path, size)
        recovery = one_password_install(args, out / "install-base", live, target, inst_pass)
        if not recovery:
            return False, f"base install did not finish; see {out / 'install-base'}"

        ring_sector, _, _ = raw_header_info(target)

        vm = boot_copy(args, out / "stage-users", [f"DISK={target}", f"EXTRA_DISK={extra}"])
        try:
            if not unlock(vm, inst_pass, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                return False, f"stage-users did not reach root shell; see {vm.out}"
            for uname, upass in ((second_user, second_pass), (third_user, third_pass)):
                st, out_add = run_status(vm, f"useradd -m -s /usr/bin/ion {uname}")
                if st != 0:
                    return False, f"useradd {uname} failed ({st}): {out_add!r}"
                code, err = run_prompts(
                    vm,
                    f"passwd {uname}",
                    ((r"new password: ", upass), (r"confirm password: ", upass)),
                    r"root:.*# ",
                    f"pw-{uname}",
                    f"pw-{uname}.txt",
                )
                if code != 0:
                    return False, f"passwd {uname} failed ({code}): {err}"
        finally:
            vm.close()

        g0 = raw_header_info(target)[1]
        target_slot_idx = (g0 - 1) % 256
        fault_sector = ring_sector + target_slot_idx * 8
        cfg_enroll_transient = scratch / "enroll-transient.conf"
        write_blkdebug_conf(cfg_enroll_transient, fault_sector, 1)
        vm = boot_blkdebug(args, out / "enroll-transient", target, extra, disk_blk=cfg_enroll_transient)
        try:
            if not unlock(vm, inst_pass, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                return False, f"enroll-transient did not reach root shell; see {vm.out}"
            g_start, invalid = check_boot_budget(target, g0, "enroll-transient")
            if invalid:
                return False, invalid
            code, err = run_prompts(
                vm,
                f"passwd --enroll {second_user}",
                ((rf"password for {second_user}: ", second_pass),
                 (r"disk password or recovery key: ", inst_pass)),
                r"root:.*# ",
                "enroll2-rc",
                "enroll2.txt",
            )
            if code != 0:
                return False, f"transient-fault passwd --enroll {second_user} failed ({code}): {err}"
            _, slots_text = run_status(vm, "cat /etc/unlock-slots")
            match = re.search(rf"(?m)^(\d+) {second_user}$", slots_text)
            if not match:
                return False, f"{second_user} missing from /etc/unlock-slots after transient roll fault: {slots_text!r}"
            second_slot = int(match.group(1))
        finally:
            vm.close()
        g_after = raw_header_info(target)[1]
        second_slot_bytes = raw_key_slots(target)[second_slot]
        if second_slot_bytes == bytes(48):
            return False, f"enrolled slot {second_slot} is empty after transient fault"
        notes.append(
            f"enroll-transient ok (G0={g0}, G_start={g_start}, ring_slot={target_slot_idx}, G_after={g_after})"
        )

        g0 = raw_header_info(target)[1]
        target_slot_idx = (g0 - 1) % 256
        fault_sector = ring_sector + target_slot_idx * 8
        cfg_enroll_exhaust = scratch / "enroll-exhaust.conf"
        write_blkdebug_conf(cfg_enroll_exhaust, fault_sector, 4)
        vm = boot_blkdebug(args, out / "enroll-exhaust", target, extra, disk_blk=cfg_enroll_exhaust)
        try:
            if not unlock(vm, inst_pass, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                return False, f"enroll-exhaust did not reach root shell; see {vm.out}"
            g_start, invalid = check_boot_budget(target, g0, "enroll-exhaust")
            if invalid:
                return False, invalid
            code, err = run_prompts(
                vm,
                f"passwd --enroll {third_user}",
                ((rf"password for {third_user}: ", third_pass),
                 (r"disk password or recovery key: ", inst_pass)),
                r"root:.*# ",
                "enroll3-rc",
                "enroll3.txt",
            )
            if code == 0:
                return False, f"exhausted-fault passwd --enroll {third_user} succeeded unexpectedly: {err}"
            _, slots_text = run_status(vm, "cat /etc/unlock-slots")
            match3 = re.search(rf"(?m)^(\d+) {third_user}$", slots_text)
            if not match3:
                return False, f"exhausted-fault enroll left live slot for {third_user} unrecorded in /etc/unlock-slots: {slots_text!r}"
            third_slot = int(match3.group(1))
            code_del3, err_del3 = run_prompts(
                vm,
                f"userdel {third_user}",
                ((r"disk password or recovery key: ", inst_pass),),
                r"root:.*# ",
                "del3-rc",
                "del3.txt",
            )
            if code_del3 != 0:
                return False, f"userdel {third_user} after partial enroll failed ({code_del3}): {err_del3}"
            st_pw3, _ = run_status(vm, f"grep '^{third_user};' /etc/passwd")
            st_sl3, _ = run_status(vm, f"grep ' {third_user}$' /etc/unlock-slots")
            if st_pw3 == 0 or st_sl3 == 0:
                return False, f"userdel {third_user} left passwd ({st_pw3}) or unlock-slots ({st_sl3})"
        finally:
            vm.close()
        g_after = raw_header_info(target)[1]
        if raw_key_slots(target)[third_slot] != bytes(48):
            return False, f"slot {third_slot} not cleared after revoking partially enrolled {third_user}"
        notes.append(
            f"enroll-exhaust recorded+revoked (G0={g0}, G_start={g_start}, ring_slot={target_slot_idx}, G_after={g_after})"
        )

        g0 = raw_header_info(target)[1]
        target_slot_idx = (g0 - 1) % 256
        fault_sector = ring_sector + target_slot_idx * 8
        cfg_change = scratch / "change-exhaust.conf"
        write_blkdebug_conf(cfg_change, fault_sector, 4)
        vm = boot_blkdebug(args, out / "change-exhaust", target, extra, disk_blk=cfg_change)
        try:
            if not unlock(vm, inst_pass, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                return False, f"change-exhaust did not reach root shell; see {vm.out}"
            if wait_session(vm, args.timeout) is None:
                return False, f"change-exhaust did not start user session before login check; see {vm.out}"
            g_start, invalid = check_boot_budget(target, g0, "change-exhaust")
            if invalid:
                return False, invalid
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no login prompt for change-exhaust; see {vm.out}"
            time.sleep(2)
            since = vm.mark()
            vm.type("user\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                return False, f"user login did not prompt for password; see {vm.out}"
            vm.type(inst_pass + "\n")
            if not vm.wait_serial(r"user:\S*\$ ", 20, since):
                return False, f"no user shell before passwd; see {vm.out}"
            code_chg, err_chg = run_prompts(
                vm,
                "passwd",
                ((r"current password: ", inst_pass),
                 (r"new password: ", changed_pass),
                 (r"confirm password: ", changed_pass)),
                r"user:\S*\$ ",
                "passwd-chg-rc",
                "passwd-chg.txt",
            )
            if code_chg != 0:
                return False, f"passwd did not save new login password on partial disk roll ({code_chg}): {err_chg}"
            since = vm.mark()
            vm.type("exit\n")
            if not vm.wait_serial(r"login: ", args.timeout, since):
                return False, f"no login prompt after passwd; see {vm.out}"
            time.sleep(2)
            since = vm.mark()
            vm.type("user\n")
            if not vm.wait_serial(r"(?i)password: ", 20, since):
                return False, f"no password prompt verifying saved password; see {vm.out}"
            vm.type(changed_pass + "\n")
            if not vm.wait_serial(r"user:\S*\$ ", 20, since):
                return False, f"new login password was rejected after partial disk roll; see {vm.out}"
        finally:
            vm.close()
        g_after = raw_header_info(target)[1]
        notes.append(
            f"change-exhaust kept login+disk in step (G0={g0}, G_start={g_start}, ring_slot={target_slot_idx}, G_after={g_after})"
        )

        g0 = raw_header_info(target)[1]
        target_slot_idx = (g0 - 1) % 256
        fault_sector = ring_sector + target_slot_idx * 8
        cfg_remove = scratch / "remove-transient.conf"
        write_blkdebug_conf(cfg_remove, fault_sector, 1)
        vm = boot_blkdebug(args, out / "remove-transient", target, extra, disk_blk=cfg_remove)
        try:
            if not unlock(vm, changed_pass, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                return False, f"remove-transient did not unlock with changed_pass or reach root shell; see {vm.out}"
            g_start, invalid = check_boot_budget(target, g0, "remove-transient")
            if invalid:
                return False, invalid
            code_del2, err_del2 = run_prompts(
                vm,
                f"userdel {second_user}",
                ((r"disk password or recovery key: ", changed_pass),),
                r"root:.*# ",
                "del2-rc",
                "del2.txt",
            )
            if code_del2 != 0:
                return False, f"transient-fault userdel {second_user} failed ({code_del2}): {err_del2}"
            st_pw2, _ = run_status(vm, f"grep '^{second_user};' /etc/passwd")
            st_sl2, _ = run_status(vm, f"grep ' {second_user}$' /etc/unlock-slots")
            if st_pw2 == 0 or st_sl2 == 0:
                return False, f"userdel {second_user} left passwd ({st_pw2}) or unlock-slots ({st_sl2})"
        finally:
            vm.close()
        g_after = raw_header_info(target)[1]
        if raw_key_slots(target)[second_slot] != bytes(48):
            return False, f"slot {second_slot} not cleared after transient-fault userdel"
        with target.open("rb") as disk_file:
            with mmap.mmap(disk_file.fileno(), 0, access=mmap.ACCESS_READ) as mm:
                if mm.find(second_slot_bytes) != -1:
                    return False, f"removed slot {second_slot}'s old bytes remained in image after transient-fault userdel"
        notes.append(
            f"remove-transient cleared ring (G0={g0}, G_start={g_start}, ring_slot={target_slot_idx}, G_after={g_after})"
        )

        vm = boot_copy(args, out / "refuse-removed", [f"DISK={target}", f"EXTRA_DISK={extra}"])
        try:
            if not unlock(vm, second_pass, 1, args.timeout):
                return False, "no bootloader prompt verifying removed account refusal"
            if not unlock(vm, changed_pass, 2, 60):
                return False, f"removed {second_user}'s password was not refused at the bootloader; see {vm.out}"
            if not root_shell(vm, args.timeout * 2):
                return False, f"remaining password did not reach root shell; see {vm.out}"
            notes.append(f"removed {second_user}'s password refused at bootloader")

            stale_a, stale_a_pass = "stale_a", "ferrion-stale-a"
            stale_b, stale_b_pass = "stale_b", "ferrion-stale-b"
            for uname, upass in ((stale_a, stale_a_pass), (stale_b, stale_b_pass)):
                st_u, out_u = run_status(vm, f"useradd -m -s /usr/bin/ion {uname}")
                if st_u != 0:
                    return False, f"stale-record-enroll: useradd {uname} failed ({st_u}): {out_u!r}"
                code_u, err_u = run_prompts(
                    vm,
                    f"passwd {uname}",
                    ((r"new password: ", upass), (r"confirm password: ", upass)),
                    r"root:.*# ",
                    f"pw-{uname}",
                    f"pw-{uname}.txt",
                )
                if code_u != 0:
                    return False, f"stale-record-enroll: passwd {uname} failed ({code_u}): {err_u}"
            code_ea, err_ea = run_prompts(
                vm,
                f"passwd --enroll {stale_a}",
                ((rf"password for {stale_a}: ", stale_a_pass),
                 (r"disk password or recovery key: ", changed_pass)),
                r"root:.*# ",
                "enroll-a-rc",
                "enroll-a.txt",
            )
            if code_ea != 0:
                return False, f"stale-record-enroll: passwd --enroll {stale_a} failed ({code_ea}): {err_ea}"
            _, slots_a = run_status(vm, "cat /etc/unlock-slots")
            match_a = re.search(rf"(?m)^(\d+) {stale_a}$", slots_a)
            if not match_a:
                return False, f"stale-record-enroll: {stale_a} missing from /etc/unlock-slots: {slots_a!r}"
            stale_a_slot = int(match_a.group(1))
            _, ctx_out = run_status(vm, "cat /scheme/sys/context")
            pids = [int(m) for m in re.findall(r"(?m)^\s*(\d+)\s+\d+\s", ctx_out)]
            max_pid = max(pids) if pids else 0
            block_paths = [f"/etc/.unlock-slots.tmp.{pid}" for pid in range(max_pid + 1, max_pid + 51)]
            for start in range(0, len(block_paths), 5):
                run_status(vm, "mkdir " + " ".join(block_paths[start:start + 5]))
            code_da_blk, err_da_blk = run_prompts(
                vm,
                f"userdel {stale_a}",
                ((r"disk password or recovery key: ", changed_pass),),
                r"root:.*# ",
                "del-a-blk-rc",
                "del-a-blk.txt",
            )
            for start in range(0, len(block_paths), 5):
                run_status(vm, "rmdir " + " ".join(block_paths[start:start + 5]))
            if code_da_blk == 0:
                return False, f"stale-record-enroll: blocked userdel {stale_a} succeeded unexpectedly: {err_da_blk}"
            code_eb, err_eb = run_prompts(
                vm,
                f"passwd --enroll {stale_b}",
                ((rf"password for {stale_b}: ", stale_b_pass),
                 (r"disk password or recovery key: ", changed_pass)),
                r"root:.*# ",
                "enroll-b-rc",
                "enroll-b.txt",
            )
            if code_eb != 0:
                return False, f"stale-record-enroll: passwd --enroll {stale_b} failed ({code_eb}): {err_eb}"
            _, slots_b = run_status(vm, "cat /etc/unlock-slots")
            match_b = re.search(rf"(?m)^(\d+) {stale_b}$", slots_b)
            if not match_b:
                return False, f"stale-record-enroll: {stale_b} missing from /etc/unlock-slots: {slots_b!r}"
            stale_b_slot = int(match_b.group(1))
            if stale_b_slot == stale_a_slot:
                return False, f"stale-record-enroll: {stale_b} reused stale slot {stale_a_slot}"
            run_status(vm, f"printf %s {changed_pass} > /tmp/k.key")
            st_ls, out_ls = run_status(vm, "redoxfs-key list --key-file /tmp/k.key /")
            run_status(vm, "rm -f /tmp/k.key")
            if (st_ls != 0 or re.search(rf"(?m)^slot {stale_a_slot}: in use\b", out_ls)
                    or not re.search(rf"(?m)^slot {stale_b_slot}: in use\b", out_ls)):
                return False, (f"stale-record-enroll: unexpected live slots (stale_a={stale_a_slot}, "
                               f"stale_b={stale_b_slot}, rc={st_ls}): {out_ls!r}")
            for uname in (stale_a, stale_b):
                code_du, err_du = run_prompts(
                    vm,
                    f"userdel {uname}",
                    ((r"disk password or recovery key: ", changed_pass),),
                    r"root:.*# ",
                    f"del-{uname}-rc",
                    f"del-{uname}.txt",
                )
                if code_du != 0:
                    return False, f"stale-record-enroll: cleanup userdel {uname} failed ({code_du}): {err_du}"
            notes.append(f"stale-record-enroll skipped stale slot {stale_a_slot} and enrolled slot {stale_b_slot}")
        finally:
            vm.close()
        if raw_key_slots(target)[stale_a_slot] != bytes(48) or raw_key_slots(target)[stale_b_slot] != bytes(48):
            return False, f"stale-record-enroll: slots {stale_a_slot}/{stale_b_slot} not cleared after cleanup"

        for case_name, fault_count, expect_text, expect_cleared in (
            ("install-cleanup-ok", 4, "cleared slot 1", True),
            ("install-cleanup-fail", None, "failed to clear slot 1", False),
        ):
            inst_img = scratch / f"{case_name}.img"
            inst_img.write_bytes(b"")
            os.truncate(inst_img, 2 << 30)
            live_copy = scratch / f"{case_name}-live.iso"
            shutil.copyfile(live, live_copy)
            cfg_inst = scratch / f"{case_name}.conf"
            write_blkdebug_conf(cfg_inst, ring_sector + 255 * 8, fault_count, skip=1)
            vm = boot_blkdebug(args, out / case_name, live_copy, inst_img, extra_blk=cfg_inst)
            try:
                if not unlock(vm, passphrase, 1, args.timeout) or not root_shell(vm, args.timeout * 2):
                    return False, f"{case_name}: live image did not reach root shell; see {vm.out}"
                st_mv, out_mv = run_status(
                    vm,
                    "mkdir -p /tmp/pkg-aside; mv /var/lib/packages/*.pkgar_head /tmp/pkg-aside/",
                )
                if st_mv != 0:
                    return False, f"{case_name}: could not trim package heads ({st_mv}): {out_mv!r}"
                since = vm.mark()
                vm.type("redox_installer_tui; echo inst-rc-$?\n")
                choice = vm.wait_serial(
                    r"(\d+)(?:removing handle[^\n]*\n)?: (?:removing handle[^\n]*\n)?\S+(?:removing handle[^\n]*\n)?: 2\.00 GB",
                    args.timeout,
                    since,
                )
                if not choice or not vm.wait_serial(r"Select a drive", 10, since):
                    return False, f"{case_name}: installer did not offer 2 GB disk; see {vm.out}"
                vm.type(choice.group(1) + "\n")
                for prompt, answer in (
                    (r"Encrypt this disk\? \[Y/n\]", ""),
                    (r"Account name \[\S+\]: ", ""),
                    (r"Password for \S+: ", inst_pass),
                    (r"Confirm password: ", inst_pass),
                ):
                    if not vm.wait_serial(prompt, 30, since):
                        return False, f"{case_name}: missing prompt {prompt!r}; see {vm.out}"
                    vm.type(answer + "\n")
                    since = vm.mark()
                done = vm.wait_serial(r"inst-rc-(\d+)|Type SAVED", 300, since)
                serial_text = vm.text(since)
                (vm.out / "installer.txt").write_text(serial_text)
                if not done or done.group(0) == "Type SAVED":
                    return False, f"{case_name}: installer asked to save recovery key or timed out; see {vm.out / 'installer.txt'}"
                if int(done.group(1)) == 0:
                    return False, f"{case_name}: installer exited 0 despite recovery-key fault; see {vm.out / 'installer.txt'}"
                if "Recovery key (" in serial_text:
                    return False, f"{case_name}: installer printed recovery key despite fault; see {vm.out / 'installer.txt'}"
                if expect_text not in serial_text:
                    return False, f"{case_name}: installer output missing {expect_text!r}; see {vm.out / 'installer.txt'}"
            finally:
                vm.close()
            inst_ring, inst_gen, inst_slots = raw_header_info(inst_img)
            if inst_ring != ring_sector:
                return False, f"setup invalid ({case_name}): target ring_sector={inst_ring}, expected {ring_sector}"
            if expect_cleared and inst_slots[1] != bytes(48):
                return False, f"{case_name}: slot 1 still populated after installer cleanup"
            if not expect_cleared and inst_slots[1] == bytes(48):
                return False, f"{case_name}: slot 1 unexpectedly empty when cleanup fault was permanent"
            notes.append(f"{case_name} ok (ring_sector={inst_ring}, ring_slot=255, G_after={inst_gen})")

        return True, "; ".join(notes)
    finally:
        shutil.rmtree(scratch)


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


def test_resolver(vm, args):
    """Issue 11: /etc/hosts wins as user, and a captured DNS query goes to the server advertised by DHCP, not the bootstrap fallback."""
    capture = vm.out / "resolver.pcap"
    reply = vm.command("object_add filter-dump,id=ferrion-resolver,netdev=net0,file="
                       + str(capture.resolve()))
    if "error" in reply.lower() or not capture.exists():
        return False, f"could not capture net0: {reply}"

    def run(line):
        since = vm.mark()
        vm.type(f"bash -c '{line}; echo resolver-status-$?'\n")
        found = vm.wait_serial(r"(?s)\n(.*)resolver-status-(\d+)", 90, since)
        return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))

    try:
        if not vm.wait_serial(r"login: ", args.timeout):
            return False, "no serial login for the resolver fixture"
        vm.type("user\n")
        time.sleep(3)
        status, output = run("id -u")
        if status != 0 or not re.search(r"(?m)^[1-9][0-9]*$", output):
            return False, f"resolver probe did not enter an ordinary-user session: {output}"
        # Boot starts dhcpd asynchronously; a second client races its port-68 bind.
        # Observe the existing client's published result instead.
        status, output = run(
            'deadline=$((SECONDS+60)); server=$(cat /etc/net/dns); '
            'while [[ -z "$server" || "$server" == 9.9.9.9 ]] && (( SECONDS < deadline )); do '
            'sleep 0.1; server=$(cat /etc/net/dns); done; '
            'echo "configured DNS: $server"; [[ -n "$server" && "$server" != 9.9.9.9 ]]')
        (vm.out / "resolver-dhcp.txt").write_text(output)
        if status != 0:
            return False, f"boot DHCP did not publish a lease DNS server (exit {status}); see {vm.out}"
        status, output = run(
            'printf "\\n192.0.2.71 hosts.resolver.fixture.invalid ferrion-host-alias\\n" | sudo tee -a /etc/hosts')
        if status != 0:
            return False, f"could not install hosts fixture: {output}"
        transcript = []
        for name in ("hosts.resolver.fixture.invalid", "FERRION-HOST-ALIAS"):
            status, output = run(
                f'curl -4 --noproxy "*" --connect-timeout 2 --max-time 5 -v http://{name}/')
            transcript.append(f"[{name}] exit {status}\n{output}\n")
            (vm.out / "resolver-user.txt").write_text("".join(transcript))
            if not re.search(r"(?m)^\* +Trying 192\.0\.2\.71:80", output):
                return False, f"hosts name {name} did not resolve to its fixture address; see {vm.out}"
        status, output = run(
            'curl -4 --noproxy "*" --connect-timeout 5 --max-time 10 -v http://dns.resolver.fixture.invalid/')
        transcript.append(f"[DNS query] exit {status}\n{output}\n")
        (vm.out / "resolver-user.txt").write_text("".join(transcript))
        if status not in (6, 28):
            return False, f"expected NXDOMAIN or timeout for DNS fixture, got curl exit {status}; see {vm.out}"
    finally:
        vm.command("object_del ferrion-resolver")

    # Read the wire evidence only after closing QEMU's capture, which flushes it.
    raw = capture.read_bytes()
    order = {b"\xd4\xc3\xb2\xa1": "little", b"\xa1\xb2\xc3\xd4": "big"}.get(raw[:4])
    if order is None or len(raw) < 24 or int.from_bytes(raw[20:24], order) != 1:
        return False, "resolver capture is not an Ethernet pcap"
    packets, pos = [], 24
    while pos < len(raw):
        if pos + 16 > len(raw):
            return False, "truncated resolver capture header"
        size = int.from_bytes(raw[pos + 8:pos + 12], order)
        pos += 16
        frame = raw[pos:pos + size]
        if len(frame) != size:
            return False, "truncated resolver capture packet"
        pos += size
        if len(frame) < 42 or frame[12:14] != b"\x08\x00":
            continue
        ip = frame[14:]
        ihl = (ip[0] & 15) * 4
        if ip[0] >> 4 != 4 or ihl < 20 or ip[9] != 17 or len(ip) < ihl + 8:
            continue
        udp = ip[ihl:]
        src, dst = int.from_bytes(udp[:2], "big"), int.from_bytes(udp[2:4], "big")
        payload = udp[8:int.from_bytes(udp[4:6], "big")]
        packets.append((src, dst, socket.inet_ntoa(ip[16:20]), payload))

    servers, queries = set(), []
    for src, dst, address, payload in packets:
        if src == 67 and dst == 68 and len(payload) >= 240 and payload[236:240] == b"\x63\x82\x53\x63":
            options, cursor = {}, 240
            while cursor < len(payload):
                tag = payload[cursor]
                cursor += 1
                if tag == 255:
                    break
                if tag == 0:
                    continue
                if cursor >= len(payload):
                    break
                size = payload[cursor]
                cursor += 1
                if cursor + size > len(payload):
                    break
                options[tag] = payload[cursor:cursor + size]
                cursor += size
            dns = options.get(6, b"")
            if options.get(53) == b"\x02" and dns and len(dns) % 4 == 0:
                servers.add(socket.inet_ntoa(dns[:4]))
        if dst == 53 and len(payload) >= 12 and not payload[2] & 0x80:
            labels, cursor = [], 12
            while cursor < len(payload) and 0 < payload[cursor] < 64:
                size = payload[cursor]
                labels.append(payload[cursor + 1:cursor + 1 + size])
                cursor += size + 1
            queries.append((b".".join(labels).lower(), address))
    if len(servers) != 1 or "9.9.9.9" in servers:
        return False, f"need one captured DHCP offer with a non-fallback DNS server, saw {servers}"
    server = next(iter(servers))
    destinations = {addr for name, addr in queries if name == b"dns.resolver.fixture.invalid"}
    hosts_queries = [name for name, _ in queries
                     if name in (b"hosts.resolver.fixture.invalid", b"ferrion-host-alias")]
    (vm.out / "resolver-wire.txt").write_text(
        f"DHCP DNS: {server}\nDNS destinations: {sorted(destinations)}\nHosts queries: {hosts_queries}\n")
    if destinations != {server} or hosts_queries:
        return False, f"resolver ignored hosts or DHCP DNS; see {vm.out / 'resolver-wire.txt'}"
    return True, f"hosts names resolve without DNS; user query captured to DHCP server {server}"


def test_cli_tools(vm, args):
    """Issue 12 + sm-core-patches 07: desktop text filters and tar options match host GNU tar."""
    if not root_shell(vm, args.timeout):
        return False, "no root shell on the serial console"
    cases = (
        ("t_match", "tar -tzf diff.tar.gz alpha"),
        ("t_miss", "tar -tzf diff.tar.gz missing"),
        ("t_dir", "tar -tzf diff.tar.gz -C input"),
        ("c_strip", "tar -czf stripped.tar.gz --strip-components=1 -C input sub/beta && tar -tzf stripped.tar.gz"),
        ("x_match", "tar -xzf diff.tar.gz -C ext1 alpha && ls -A ext1"),
        ("x_miss", "tar -xzf diff.tar.gz -C ext2 missing"),
    )
    host_expected = {}
    with tempfile.TemporaryDirectory(prefix="ferrion-tar-ref-") as ref_dir:
        ref = Path(ref_dir)
        (ref / "input" / "sub").mkdir(parents=True)
        (ref / "ext1").mkdir()
        (ref / "ext2").mkdir()
        (ref / "input" / "alpha").write_text("alpha\n")
        (ref / "input" / "sub" / "beta").write_text("beta\n")
        subprocess.run(["tar", "-czf", "diff.tar.gz", "-C", "input", "alpha", "sub/beta"], cwd=ref, check=True)
        for name, cmd in cases:
            proc = subprocess.run(["bash", "-c", cmd], cwd=ref, capture_output=True, text=True)
            host_expected[name] = (proc.returncode, proc.stdout)
    guest_cases = "\n".join(
        f'({cmd}) > "$work/diff/out" 2>/dev/null; rc=$?\n'
        f'b64=$(base64 "$work/diff/out" | tr -d "\\r\\n")\n'
        f'printf "TARCASE {name} %d %s\\n" "$rc" "$b64"'
        for name, cmd in cases
    )
    script = (
        r'''set -euo pipefail
work=$(mktemp -d /tmp/ferrion-cli-tools.XXXXXX)
trap 'rm -rf "$work"' EXIT
test "$(printf 'before\n' | sed 's/before/after/')" = after
echo cli-sed-ok
test "$(printf 'one 7\ntwo 5\n' | awk '{sum += $2} END {print sum}')" = 12
echo cli-awk-ok
mkdir -p "$work/input/empty" "$work/output"
printf 'archive payload\n' > "$work/input/message"
cd "$work"
tar -czf bundle.tar.gz -C input .
tar -xzf bundle.tar.gz -C output
test "$(cat output/message)" = "archive payload"
test -d output/empty
echo cli-tar-ok
mkdir -p "$work/diff/input/sub" "$work/diff/ext1" "$work/diff/ext2"
printf 'alpha\n' > "$work/diff/input/alpha"
printf 'beta\n' > "$work/diff/input/sub/beta"
cd "$work/diff"
tar -czf diff.tar.gz -C input alpha sub/beta
set +e
'''
        + guest_cases
        + "\nset -e\necho cli-tar-diff-done\n"
    )
    encoded = base64.b64encode(script.encode()).decode()
    run_status(vm, "rm -f /tmp/cli-tools.b64", timeout=30)
    for start in range(0, len(encoded), 160):
        chunk = encoded[start:start + 160]
        rc, _ = run_status(vm, f"printf %s {chunk} >> /tmp/cli-tools.b64", timeout=30)
        if rc != 0:
            return False, f"cli-tools script upload failed at offset {start} (rc={rc})"
    status, output = run_status(
        vm, "bash -c 'base64 -d /tmp/cli-tools.b64 | bash; rc=$?; rm -f /tmp/cli-tools.b64; exit $rc'", timeout=60)
    (vm.out / "cli-tools.txt").write_text(output)
    expected = ("cli-sed-ok", "cli-awk-ok", "cli-tar-ok", "cli-tar-diff-done")
    missing = [marker for marker in expected if not re.search(rf"(?m)^{marker}$", output)]
    if status != 0 or missing:
        return False, f"cli-tools exit {status}; missing {missing}; see {vm.out / 'cli-tools.txt'}"
    observed = {}
    for match in re.finditer(r"(?m)^TARCASE (\S+) (\d+) ([A-Za-z0-9+/=]*)$", output):
        name, rc_str, b64 = match.groups()
        observed[name] = (int(rc_str), base64.b64decode(b64).decode("utf-8", errors="replace"))
    diffs = []
    for name, _ in cases:
        if observed.get(name) != host_expected[name]:
            diffs.append(f"{name}: guest={observed.get(name)!r} host={host_expected[name]!r}")
    if diffs:
        return False, "tar diff mismatch: " + "; ".join(diffs) + f"; see {vm.out / 'cli-tools.txt'}"
    return True, "sed, awk, tar -czf/-xzf -C and 6-case GNU tar differential matrix passed"


# The net-hardening test's network: QEMU's user network (restricted, so nothing leaves
# the host) and a host-side peer share one hub with the guest's card. The peer sees
# every frame the guest sends and writes its own frames back through a dgram netdev.
# The user network's replies are held for 300 ms, so the peer's replies arrive first.
GUEST_MAC = bytes.fromhex("525400123456")
SLIRP_MAC = bytes.fromhex("52550a000202")  # the user network's gateway, 10.0.2.2
GUEST_IP = "10.0.2.15"
# The server the guest is told to ask. It is on the guest's link, where only the peer
# answers ARP for it: the restricted user network gives no router or name server.
TEST_DNS = "10.0.2.53"
TEST_DNS_MAC = bytes.fromhex("525400000253")
NET_CARDS = {"e1000": "e1000", "virtio": "virtio-net-pci"}


def inet_sum(data):
    if len(data) % 2:
        data += b"\0"
    total = sum(int.from_bytes(data[i:i + 2], "big") for i in range(0, len(data), 2))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return ~total & 0xFFFF


def ipv4_packet(src, dst, proto, payload):
    header = bytearray(20)
    header[0], header[8], header[9] = 0x45, 64, proto
    header[2:4] = (20 + len(payload)).to_bytes(2, "big")
    header[12:16], header[16:20] = socket.inet_aton(src), socket.inet_aton(dst)
    header[10:12] = inet_sum(bytes(header)).to_bytes(2, "big")
    return bytes(header) + payload


def udp_frame(dst_mac, src_ip, dst_ip, sport, dport, payload, src_mac=SLIRP_MAC):
    length = 8 + len(payload)
    udp = sport.to_bytes(2, "big") + dport.to_bytes(2, "big") + length.to_bytes(2, "big") + b"\0\0" + payload
    pseudo = socket.inet_aton(src_ip) + socket.inet_aton(dst_ip) + b"\0\x11" + length.to_bytes(2, "big")
    udp = udp[:6] + (inet_sum(pseudo + udp) or 0xFFFF).to_bytes(2, "big") + udp[8:]
    return dst_mac + src_mac + b"\x08\x00" + ipv4_packet(src_ip, dst_ip, 17, udp)


def dhcp_reply(xid, chaddr, kind, yiaddr, server="10.0.2.2", dns=None, tail=b"\xff"):
    options = bytes([53, 1, kind, 54, 4]) + socket.inet_aton(server) + bytes([1, 4, 255, 255, 255, 0, 3, 4, 10, 0, 2, 2])
    if dns:
        options += bytes([6, 4]) + socket.inet_aton(dns)
    body = (bytes([2, 1, 6, 0]) + xid + b"\0\0\x80\0" + bytes(4) + socket.inet_aton(yiaddr)
            + socket.inet_aton("10.0.2.2") + bytes(4) + chaddr + bytes(192) + b"\x63\x82\x53\x63")
    return udp_frame(b"\xff" * 6, "10.0.2.2", "255.255.255.255", 67, 68, body + options + tail)


def dns_reply(query_id, question, answer, flags=0x8180):
    """A reply with one question (wire form, with type and class) and one A record."""
    return (struct.pack("!HHHHHH", query_id, flags, 1, 1, 0, 0) + question
            + b"\xc0\x0c" + struct.pack("!HHIH", 1, 1, 30, 4) + socket.inet_aton(answer))


class NetPeer:
    """The host-side peer on the net-hardening and net-calls tests' hub, for one boot. With
    `serve_lease` it answers DHCP with a lease of GUEST_IP naming TEST_DNS as name server;
    without it, only with replies the guest must ignore."""

    def __init__(self, out, serve_lease=False):
        self.out = out
        self.serve_lease = serve_lease
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1 << 22)
        self.sock.bind(("127.0.0.1", 0))
        probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        probe.bind(("127.0.0.1", 0))
        self.qemu_port = probe.getsockname()[1]
        probe.close()
        self.lock = threading.Condition()
        self.log = {"dhcp": [], "dns": [], "syn": [], "sent": []}
        self.arp_replies = 0
        self.guest_ip = GUEST_IP  # the address the guest sends from, once it has one
        self.closed = False
        threading.Thread(target=self._read, daemon=True).start()

    def qemu_flags(self, card):
        return (f"-netdev user,id=net0,restrict=on "
                f"-netdev dgram,id=netpeer,local.type=inet,local.host=127.0.0.1,local.port={self.qemu_port},"
                f"remote.type=inet,remote.host=127.0.0.1,remote.port={self.sock.getsockname()[1]} "
                "-netdev hubport,id=hub-user,hubid=7,netdev=net0 -netdev hubport,id=hub-peer,hubid=7,netdev=netpeer "
                f"-netdev hubport,id=hub-card,hubid=7 -device {NET_CARDS[card]},netdev=hub-card,mac=52:54:00:12:34:56 "
                "-object filter-buffer,id=user-delay,netdev=net0,queue=tx,interval=300000")

    def send(self, frame, note=None):
        self.sock.sendto(frame, ("127.0.0.1", self.qemu_port))
        if note:
            with self.lock:
                self.log["sent"].append(note)

    def _read(self):
        while not self.closed:
            try:
                frame = self.sock.recv(65536)
            except OSError:
                return
            try:
                self._handle(frame)
            except (IndexError, ValueError, struct.error):
                pass

    def _handle(self, frame):
        if frame[6:12] != GUEST_MAC:
            return
        kind = frame[12:14]
        if kind == b"\x08\x06" and frame[20:22] == b"\x00\x02" and frame[38:40] == b"\x0a\x01":
            with self.lock:
                self.arp_replies += 1
                self.lock.notify_all()
            return
        if kind == b"\x08\x06" and frame[20:22] == b"\x00\x01" and frame[38:42] == socket.inet_aton(TEST_DNS):
            self.send(GUEST_MAC + TEST_DNS_MAC + b"\x08\x06\x00\x01\x08\x00\x06\x04\x00\x02" + TEST_DNS_MAC
                      + socket.inet_aton(TEST_DNS) + GUEST_MAC + frame[28:32])
            return
        if kind != b"\x08\x00":
            return
        ip = frame[14:]
        ihl = (ip[0] & 15) * 4
        src, dst, payload = socket.inet_ntoa(ip[12:16]), socket.inet_ntoa(ip[16:20]), ip[ihl:int.from_bytes(ip[2:4], "big")]
        sport, dport = int.from_bytes(payload[0:2], "big"), int.from_bytes(payload[2:4], "big")
        if src != "0.0.0.0":
            self.guest_ip = src
        if ip[9] == 6 and payload[13] & 0x12 == 0x02:
            with self.lock:
                self.log["syn"].append({"dst": f"{dst}:{dport}", "sport": sport,
                                        "isn": int.from_bytes(payload[4:8], "big")})
        elif ip[9] == 17 and dport == 67:
            self._dhcp(payload[8:])
        elif ip[9] == 17 and dport == 53 and dst == TEST_DNS:
            self._dns(src, sport, payload[8:])

    def _dhcp(self, body):
        xid, chaddr, options = body[4:8], body[28:44], body[240:]
        found, cursor = {}, 0
        while cursor + 1 < len(options) and options[cursor] != 255:
            if options[cursor] == 0:
                cursor += 1
                continue
            size = options[cursor + 1]
            found.setdefault(options[cursor], options[cursor + 2:cursor + 2 + size])
            cursor += 2 + size
        kind = found.get(53, b"\0")[0]
        entry = {"type": kind, "xid": xid.hex()}
        if 50 in found:
            entry["requested"] = socket.inet_ntoa(found[50])
        with self.lock:
            self.log["dhcp"].append(entry)
        other_xid = bytes(b ^ 0xA5 for b in xid)
        other_mac = bytes([chaddr[0] ^ 2]) + chaddr[1:]
        if self.serve_lease and kind in (1, 3):  # discover or request: offer or acknowledge
            self.send(dhcp_reply(xid, chaddr, 2 if kind == 1 else 5, GUEST_IP, dns=TEST_DNS), "lease")
        elif kind == 1:  # discover: offers that must all be ignored
            self.send(dhcp_reply(other_xid, chaddr, 2, "10.0.2.99", dns="192.0.2.99"), "offer, other xid")
            self.send(dhcp_reply(xid, other_mac, 2, "10.0.2.98", dns="192.0.2.98"), "offer, other hardware address")
            self.send(dhcp_reply(xid, chaddr, 5, "10.0.2.97", dns="192.0.2.97"), "ack instead of offer")
            self.send(dhcp_reply(xid, chaddr, 2, "10.0.2.96", tail=bytes([6, 40]) + socket.inet_aton("192.0.2.96")),
                      "offer, option past the end")
        elif kind == 3:  # request: answers that must all be ignored
            requested = entry.get("requested", GUEST_IP)
            self.send(dhcp_reply(other_xid, chaddr, 5, requested, dns="192.0.2.95"), "ack, other xid")
            self.send(dhcp_reply(xid, chaddr, 6, requested, server="10.0.2.9"), "nak from another server")
            self.send(dhcp_reply(xid, chaddr, 5, "10.0.2.94", dns="192.0.2.94"), "ack of another address")

    def _dns(self, guest_ip, port, query):
        query_id, cursor = int.from_bytes(query[:2], "big"), 12
        while query[cursor]:
            cursor += 1 + query[cursor]
        question = query[12:cursor + 5]
        other = b"\x05other\x07invalid\x00" + question[-4:]
        with self.lock:
            self.log["dns"].append({"id": query_id, "sport": port})

        def reply(source, sport, answer, qid=query_id, q=question):
            self.send(udp_frame(GUEST_MAC, source, guest_ip, sport, port, dns_reply(qid, q, answer), TEST_DNS_MAC))

        # Each wrong reply names its own address; only the last one may be used.
        reply(TEST_DNS, 53, "192.0.2.66", qid=query_id ^ 0x5A5A)
        reply(TEST_DNS, 53, "192.0.2.67", q=other)
        reply(TEST_DNS, 5353, "192.0.2.68")
        reply("10.0.2.54", 53, "192.0.2.69")
        reply(TEST_DNS, 53, "192.0.2.70")

    def large_frames(self, guest_ip):
        """Frames of 1,514 (full size), 1,518, 4,000 and 9,000 bytes, each twice."""
        for size in (1514, 1518, 4000, 9000) * 2:
            self.send(udp_frame(GUEST_MAC, "10.0.2.2", guest_ip, 9, 9, bytes(size - 42)), f"{size}-byte frame")
            time.sleep(0.05)

    def arp_flood(self, guest_ip, count, window=16):
        """ARP requests for the guest's address from `count` distinct senders in
        10.1.0.0/16, `window` at a time; returns how many the guest answered."""
        with self.lock:
            start = self.arp_replies
        target = socket.inet_aton(guest_ip)
        for first in range(0, count, window):
            for n in range(first, min(count, first + window)):
                mac = b"\x02\x00" + n.to_bytes(4, "big")
                sender = bytes([10, 1, (n >> 8) & 0xFF, n & 0xFF])
                arp = b"\x00\x01\x08\x00\x06\x04\x00\x01" + mac + sender + bytes(6) + target
                self.send(b"\xff" * 6 + mac + b"\x08\x06" + arp)
            with self.lock:
                self.lock.wait_for(lambda: self.arp_replies - start >= min(count, first + window) - window // 2, 0.05)
        time.sleep(1)
        with self.lock:
            return self.arp_replies - start

    def close(self, name):
        self.closed = True
        self.sock.close()
        with self.lock:
            (self.out / name).write_text(json.dumps(self.log, indent=1) + "\n")
            return self.log


def consecutive_run(ports):
    """The longest run of consecutive numbers among `ports`."""
    longest, run, previous = 0, 0, None
    for port in sorted(set(ports)):
        run = run + 1 if previous is not None and port == previous + 1 else 1
        longest, previous = max(longest, run), port
    return longest


# The generator smoltcp 0.14 draws TCP initial sequence numbers from (src/rand.rs, rand_u32):
# a 64-bit multiply-add whose output is 32 bits of the new state, at a shift set by its top
# three bits. Two outputs leave few states, found below without trying all 2^64.
PCG_MUL, PCG_ADD, MASK64 = 0xBB2EFCEC3C39611D, 0x7590EF39, (1 << 64) - 1


def pcg_output(state):
    return (state >> (29 - (state >> 61))) & 0xFFFFFFFF


def pcg_jump(steps):
    """(a, c) such that the state `steps` draws later is a * state + c (mod 2^64)."""
    a, c = 1, 0
    for _ in range(steps):
        a, c = a * PCG_MUL & MASK64, (c * PCG_MUL + PCG_ADD) & MASK64
    return a, c


def first_multiple_in(a, m, lo, hi):
    """The least x >= 0 with lo <= a*x mod m <= hi, for 0 <= lo <= hi < m, or None."""
    a %= m
    if lo == 0:
        return 0
    if a == 0:
        return None
    x = -(-lo // a)
    if a * x <= hi:
        return x
    # No multiple of a lands in [lo, hi] before wrapping: find the least wrap count y with
    # a*x - m*y in [lo, hi], which is the same question modulo a.
    y = first_multiple_in(m % a, a, -hi % a, -lo % a)
    if y is None:
        return None
    x = -(-(lo + m * y) // a)
    return x if a * x - m * y <= hi else None


def affine_hits(a, b, m, lo, hi, count):
    """Each x in range(count), in order, with lo <= (a*x + b) mod m <= hi."""
    start = 0
    while start < count:
        base = (b + a * start) % m
        low, high = (lo - base) % m, (hi - base) % m
        x = first_multiple_in(a, m, low, high) if low <= high else 0
        if x is None or start + x >= count:
            return
        yield start + x
        start += x + 1


def pcg_states(x1, x2, steps):
    """Each generator state that outputs x2 and was reached `steps` draws after one that
    output x1. Of x1's state, the top three bits fix the shift and 32 bits are x1; the other
    29 are the low bits, which alone decide x2's lowest bits (found as the solutions of a
    linear congruence), and the bits just under the top three (tried in full)."""
    a, c = pcg_jump(steps)
    for top1 in range(8):
        shift1 = 29 - top1
        known = (top1 << 61) | (x1 << shift1)
        for top2 in range(8):
            shift2 = 29 - top2
            bits = min(shift1, shift2) + 32
            m = 1 << bits
            lo = (x2 % (1 << (bits - shift2))) << shift2
            for low in affine_hits(a, (a * known + c) % m, m, lo, lo + (1 << shift2) - 1, 1 << shift1):
                for middle in range(1 << (29 - shift1)):
                    state = (a * (known | low | (middle << (shift1 + 32))) + c) & MASK64
                    if pcg_output(state) == x2:
                        yield state


def pcg_prediction(isns, gap=4):
    """The first (index, steps, later) where isns[index + 2] is the generator's output `later`
    draws after a state that gave isns[index] and, `steps` draws on, isns[index + 1]; None
    when no number follows from the two before it with up to `gap` draws between."""
    for index in range(len(isns) - 2):
        x1, x2, x3 = isns[index:index + 3]
        for steps in range(1, gap + 1):
            for state in pcg_states(x1, x2, steps):
                for later in range(1, gap + 1):
                    a, c = pcg_jump(later)
                    if pcg_output((a * state + c) & MASK64) == x3:
                        return index, steps, later
    return None


def net_card(args):
    """The network card `--make net=` picks for net-hardening and net-calls; e1000 by default."""
    return next((item.split("=", 1)[1] for item in reversed(args.make) if item.startswith("net=")), "e1000")


def context_rows(vm):
    """PID, private memory in bytes and name of each process, from /scheme/sys/context."""
    _, text = run_status(vm, "cat /scheme/sys/context > /tmp/context.txt && cat /tmp/context.txt", 60)
    units = {"B": 1, "KB": 1 << 10, "MB": 1 << 20, "GB": 1 << 30}
    rows = []
    for match in re.finditer(r"(?m)^(\d+)\s+\d+\s+\d+\s+\S+\s+\S+\s+\S+\s+\S+\s+\S+\s+([\d.]+) ([KMG]?B)\s+"
                             r"[\d.]+ [KMG]?B\s+(\S.*?)\s*$", text):
        rows.append((int(match.group(1)), float(match.group(2)) * units[match.group(3)], match.group(4)))
    return rows


def process(rows, name):
    found = [row for row in rows if row[2].rsplit("/", 1)[-1] == name]
    return found[0] if found else None


def test_net_hardening(args, out):
    """Network hardening on the card `--make net=` picks (e1000 or virtio), over two boots: DHCP offers and answers for another request or malformed are ignored; TCP initial sequence numbers differ between boots, do not step by a fixed amount within one and are not smoltcp's generator's next output after the two before; source ports are not consecutive; a name lookup takes only the reply with its ID and question from the server's address and port, 20 times of 20; frames of 1,514 to 9,000 bytes and reads of the driver's mac past its end leave the network driver running; 40,000 ARP senders grow the stack by less than 512 KiB and it still answers."""
    card = net_card(args)
    if card not in NET_CARDS:
        return False, f"net-hardening runs with net=e1000 (the default) or net=virtio, not net={card}"
    problems, notes, boots = [], [f"card {card}"], []

    def lookup(vm, name):
        _, text = run_status(vm, f"bash -c 'curl -4 --noproxy \"*\" -sv --connect-timeout 1 --max-time 4 "
                                 f"http://{name}.lookup.invalid/ 2>&1 | grep -E \"Trying|resolve\"'", 30)
        found = re.search(r"Trying (192\.0\.2\.\d+):80", text)
        return found.group(1) if found else "none"

    for boot in (1, 2):
        boot_out = out / f"boot-{boot}"
        boot_out.mkdir(parents=True, exist_ok=True)
        peer = NetPeer(boot_out)
        vm = None
        log = None
        try:
            vm = VM(args, boot_out, qemu_flags=peer.qemu_flags(card), make=("net=no",))
            if not root_shell(vm, args.timeout):
                return False, f"boot {boot}: no root shell on the serial console; see {boot_out}"
            # dhcpd starts in the background; wait until it has exited.
            _, text = run_status(vm, "bash -c 'for i in $(seq 600); do grep -q dhcpd /scheme/sys/context || break; "
                                     "sleep 0.1; done; cat /etc/net/dns; echo; cat /scheme/netcfg/ifaces/eth0/addr/list'", 90)
            (boot_out / "dhcp.txt").write_text(text)
            configured = re.findall(r"(?m)^\s*(\d+\.\d+\.\d+\.\d+(?:/\d+)?)\s*$", text)
            if any(re.match(r"192\.0\.2\.9\d|10\.0\.2\.9\d", value) for value in configured):
                problems.append(f"boot {boot}: dhcpd took a reply meant to be ignored: {configured}")

            run_status(vm, "bash -c 'for i in $(seq 20); do curl -s --connect-timeout 1 http://10.0.2.2:9/ >/dev/null; done'", 120)

            if boot == 1:
                run_status(vm, f"echo {TEST_DNS} > /etc/net/dns")
                # The restricted user network names no router, and with no route `curl` can
                # hang connecting to the looked-up address past --max-time (seen with
                # virtio-net); a route to the user network's gateway lets it time out.
                run_status(vm, "bash -c 'printf \"default via 10.0.2.2\" > /scheme/netcfg/route/add'")
                answers = [lookup(vm, f"q{n}") for n in range(20)]
                (boot_out / "lookups.txt").write_text("\n".join(answers) + "\n")
                wrong = [answer for answer in answers if answer != "192.0.2.70"]
                if wrong:
                    problems.append(f"{len(wrong)} of 20 lookups did not take the matching reply: {sorted(set(wrong))}")
                else:
                    notes.append("20 of 20 lookups took the matching reply")

                driver = "virtio-netd" if card == "virtio" else "e1000d"
                before = process(context_rows(vm), driver)
                peer.large_frames(peer.guest_ip)
                time.sleep(2)
                after = process(context_rows(vm), driver)
                answer = lookup(vm, "after-frames")
                if before is None or after is None or before[0] != after[0] or answer == "none":
                    problems.append(f"after large frames: {driver} {before} then {after}, lookup {answer}")
                else:
                    notes.append(f"{driver} kept PID {after[0]} through frames of 1,514 to 9,000 bytes")

                # driver-network answers a read of `mac` past its end with EINVAL.
                before = process(context_rows(vm), driver)
                status, text = run_status(vm, "net-call-probe mac", 60)
                (boot_out / "mac.txt").write_text(text)
                after = process(context_rows(vm), driver)
                answer = lookup(vm, "after-mac")
                if status != 0 or before is None or after is None or before[0] != after[0] or answer == "none":
                    problems.append(f"mac reads: net-call-probe exit {status}, {driver} {before} then {after}, "
                                    f"lookup {answer}; see {boot_out / 'mac.txt'}")
                else:
                    notes.append(f"{driver} refused mac reads past the end and kept PID {after[0]}")

                before = process(context_rows(vm), "netstack")
                answered = peer.arp_flood(peer.guest_ip, 40_000)
                after = process(context_rows(vm), "netstack")
                answer = lookup(vm, "after-arp")
                (boot_out / "arp.txt").write_text(f"answered {answered}\nbefore {before}\nafter {after}\nlookup {answer}\n")
                if answered < 10_000:
                    problems.append(f"the ARP flood reached the stack only {answered} times; see {boot_out}")
                elif before is None or after is None or after[1] - before[1] >= 512 << 10 or answer == "none":
                    problems.append(f"after {answered} ARP senders: netstack {before} then {after}, lookup {answer}")
                else:
                    notes.append(f"netstack grew {int(after[1] - before[1]) >> 10} KiB over {answered} ARP senders")
            vm.kill()
        finally:
            if vm is not None:
                vm.close()
            log = peer.close(f"peer-boot-{boot}.json")
        if not any(entry["type"] == 1 for entry in log["dhcp"]):
            problems.append(f"boot {boot}: the peer saw no DHCP discover, so its replies were not sent")
        requested = {entry.get("requested") for entry in log["dhcp"] if entry["type"] == 3}
        if requested != {GUEST_IP}:
            problems.append(f"boot {boot}: dhcpd requested {sorted(map(str, requested))}, not only {GUEST_IP}")
        syns = list({entry["sport"]: entry for entry in log["syn"] if entry["dst"] == "10.0.2.2:9"}.values())
        boots.append(syns)
        if len(syns) < 20:
            problems.append(f"boot {boot}: saw {len(syns)} of 20 connections")
        elif consecutive_run([entry["sport"] for entry in syns]) >= 3:
            problems.append(f"boot {boot}: consecutive source ports {sorted(entry['sport'] for entry in syns)}")
        numbers = [entry["isn"] for entry in syns]
        steps = [(b - a) % (1 << 32) for a, b in zip(numbers, numbers[1:])]
        predicted = pcg_prediction(numbers)
        if any(step == next_step for step, next_step in zip(steps, steps[1:])):
            problems.append(f"boot {boot}: sequence numbers step by a fixed amount: {numbers}")
        elif predicted:
            index, gap, later = predicted
            problems.append(f"boot {boot}: sequence number {numbers[index + 2]} of connection {index + 3} is "
                            f"smoltcp's generator's output {later} draw(s) after the state that gave "
                            f"{numbers[index]} and, {gap} draw(s) on, {numbers[index + 1]}")
        elif len(numbers) >= 3:
            notes.append(f"boot {boot}: no sequence number follows from the two before it")
        if boot == 1:
            queries = log["dns"]
            if consecutive_run([entry["sport"] for entry in queries]) >= 3 or len({e["id"] for e in queries}) < len(queries):
                problems.append(f"lookup ports or IDs repeat or count up: {queries}")

    isns = [[entry["isn"] for entry in syns] for syns in boots]
    if all(isns) and (isns[0][0] == isns[1][0] or set(isns[0]) & set(isns[1])):
        problems.append(f"initial sequence numbers repeat across boots: {isns[0][:3]} and {isns[1][:3]}")
    elif all(isns):
        notes.append(f"first sequence numbers {isns[0][0]} and {isns[1][0]}; no number shared between boots")
    if problems:
        return False, "; ".join(problems) + f"; see {out}"
    return True, "; ".join(notes)


def test_net_calls(args, out):
    """Boots with a host-side peer as DHCP and name server on the card `--make net=` picks (e1000 or virtio), logs in as `user` and runs net-call-probe: calls on tcp and udp sockets with buffers shorter than the reply or sizes past them are refused with EINVAL, datagrams longer than the buffer are cut to fit, the stack still carries UDP and TCP over loopback, and a lookup and a TCP connection leave the guest before and after the probe."""
    card = net_card(args)
    if card not in NET_CARDS:
        return False, f"net-calls runs with net=e1000 (the default) or net=virtio, not net={card}"
    peer = NetPeer(out, serve_lease=True)
    vm = None

    def run(tag, line, timeout):
        # The status is echoed inside bash: `user`'s login shell is not POSIX.
        since = vm.mark()
        vm.type(f"bash -c '{line}; echo nc-end-{tag}-$?'\n")
        found = vm.wait_serial(rf"(?s)\n(.*)nc-end-{tag}-(\d+)", timeout, since)
        return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))

    def lookup_and_connect(tag):
        """The address a lookup of `tag` gave, or None, and whether the peer then saw the
        guest's connection to it."""
        with peer.lock:
            seen = len(peer.log["syn"])
        _, text = run(tag, f'curl -4 --noproxy "*" -sv --connect-timeout 1 --max-time 4 '
                           f'http://{tag}.lookup.invalid/ 2>&1 | grep -E "Trying|resolve"', 30)
        found = re.search(r"Trying (192\.0\.2\.\d+):80", text)
        if not found:
            return None, False
        with peer.lock:
            return found.group(1), any(entry["dst"] == f"{found.group(1)}:80" for entry in peer.log["syn"][seen:])

    try:
        vm = VM(args, out, qemu_flags=peer.qemu_flags(card), make=("net=no",))
        if not vm.wait_serial(r"login: ", args.timeout):
            return False, f"no serial login prompt within {args.timeout}s; see {out}"
        vm.type("user\n")
        time.sleep(3)
        _, text = run("lease", f"for i in $(seq 600); do grep -q {TEST_DNS} /etc/net/dns && break; sleep 0.1; done; "
                               "cat /etc/net/dns", 90)
        if TEST_DNS not in text:
            return False, f"the guest did not take the peer's lease naming {TEST_DNS}: {text.strip()!r}; see {out}"
        before = lookup_and_connect("before")
        if not all(before):
            return False, f"before the probe: lookup gave {before[0]}, connection seen {before[1]}; see {out}"
        status, text = run("probe", "net-call-probe", 120)
        (out / "net-calls.txt").write_text(f"exit {status}\n{text}")
        after = lookup_and_connect("after")
    finally:
        if vm is not None:
            vm.close()
        peer.close("peer.json")
    lines = [line.strip() for line in re.findall(r"net-call-probe: (?:pass|FAIL) .*", text)]
    failed = [line for line in lines if ": FAIL " in line]
    problems = []
    if status != 0 or failed or not lines:
        problems.append(f"net-call-probe exit {status}: {'; '.join(failed) or 'no result line'}")
    if not all(after):
        problems.append(f"after the probe: lookup gave {after[0]}, connection seen {after[1]}")
    if problems:
        return False, "; ".join(problems) + f"; see {out / 'net-calls.txt'}"
    return True, f"card {card}; {len(lines)} checks passed; lookup and connection to {after[0]} before and after"


def test_dso_atexit(vm, args):
    """A shared object's exit callback runs exactly once: at exit while the object stays open, at dlclose once it is closed (use --config dso-atexit)."""
    if not root_shell(vm, args.timeout):
        return False, "no root serial login"
    results = []
    for mode in ("keep-open", "close"):
        since = vm.mark()
        vm.type(f"bash -c 'dso-atexit-probe {mode}; echo DSO-EXIT-{mode}-$?'\n")
        found = vm.wait_serial(rf"DSO-EXIT-{mode}-(\d+)", args.timeout, since)
        output = vm.text(since)
        (vm.out / f"{mode}.txt").write_text(output)
        if not found:
            return False, f"{mode}: no exit status; see {vm.out / (mode + '.txt')}"
        status = int(found.group(1))
        results.append(f"{mode} status {status}")
        if status or "DSO_ATEXIT_RETURN" not in output:
            return False, "; ".join(results)
        calls = output.count("DSO_ATEXIT_CALLBACK")
        if calls != 1:
            return False, f"{mode}: callback ran {calls} times"
        callback = output.index("DSO_ATEXIT_CALLBACK")
        if mode == "keep-open" and callback < output.index("DSO_ATEXIT_RETURN"):
            return False, "kept-open callback ran before exit"
        if mode == "close" and ("DSO_ATEXIT_CLOSED" not in output or callback > output.index("DSO_ATEXIT_CLOSED")):
            return False, "closed object's callback did not run at dlclose"
    return True, "; ".join(results) + "; callback once, at exit when kept open and at dlclose when closed"


def test_relro_got_readonly(vm, args):
    """Checks RELRO and immediate binding enforcement on full-RELRO, partial-RELRO and static probes (use --config relro)."""
    if not root_shell(vm, args.timeout):
        return False, "no root serial login"

    def run_probe(tag, cmd):
        since = vm.mark()
        vm.type(f"{cmd}\n")
        if not vm.wait_serial(
            r"(?:RELRO_INFO|STATIC_RELRO_INFO|RELRO_WRITE_SUCCEEDED|RELRO_ERROR|ended by signal [A-Z0-9]+)[\s\S]*?root:.*# ",
            args.timeout,
            since,
        ):
            output = vm.text(since)
            (vm.out / f"{tag}.txt").write_text(output)
            return None, output
        status_since = vm.mark()
        vm.type(f"echo RELRO-EXIT-{tag}-$?\n")
        found = vm.wait_serial(rf"RELRO-EXIT-{tag}-(\d+)[\s\S]*?root:.*# ", args.timeout, status_since)
        output = vm.text(since)
        (vm.out / f"{tag}.txt").write_text(output)
        if not found:
            return None, output
        return int(found.group(1)), output

    def faulted(status, out):
        if "RELRO_BEFORE_WRITE" not in out or "RELRO_WRITE_SUCCEEDED" in out:
            return False
        return "ended by signal SIGSEGV" in out or status in (11, 139)

    failures = []
    results = []

    status, out = run_probe("full-info", "relro-probe-full info")
    expected_full = (
        "RELRO_INFO bind_now=1 has_relro=1 slot_in_relro=1 ptr_in_relro=1 "
        "bound_before_call=1 value=42 match=1 mutable=99"
    )
    if status != 0 or expected_full not in out:
        failures.append(f"full-info status={status}")
    else:
        results.append("full-info ok")

    for tag, cmd in (
        ("full-write-got", "relro-probe-full write-got"),
        ("full-write-relro", "relro-probe-full write-relro"),
        ("full-write-dso", "relro-probe-full write-dso"),
    ):
        status, out = run_probe(tag, cmd)
        if status is None:
            failures.append(f"{tag}: no exit status")
        elif not faulted(status, out):
            failures.append(f"{tag}: write did not fault (status={status})")
        else:
            results.append(f"{tag} SIGSEGV")

    status, out = run_probe("partial-info", "relro-probe-partial info")
    expected_lazy = 0 if args.arch == "x86_64" else 1
    expected_partial = (
        f"RELRO_INFO bind_now=0 has_relro=1 slot_in_relro=0 ptr_in_relro=1 "
        f"bound_before_call={expected_lazy} value=42 match=1 mutable=99"
    )
    if status != 0 or expected_partial not in out:
        failures.append(f"partial-info status={status}")
    else:
        results.append("partial-info ok")

    status, out = run_probe("partial-write-relro", "relro-probe-partial write-relro")
    if status is None:
        failures.append("partial-write-relro: no exit status")
    elif not faulted(status, out):
        failures.append(f"partial-write-relro: write did not fault (status={status})")
    else:
        results.append("partial-write-relro SIGSEGV")

    status, out = run_probe("partial-write-got", "relro-probe-partial write-got")
    if status != 0 or "RELRO_WRITE_SUCCEEDED" not in out:
        failures.append(f"partial-write-got: unexpected status={status}")
    else:
        results.append("partial-write-got writable")

    status, out = run_probe("static-info", "relro-probe-static info")
    if status != 0 or "STATIC_RELRO_INFO value=42 mutable=99" not in out:
        failures.append(f"static-info status={status}")
    else:
        results.append("static-info ok")

    status, out = run_probe("static-write-relro", "relro-probe-static write-relro")
    if status is None:
        failures.append("static-write-relro: no exit status")
    elif not faulted(status, out):
        failures.append(f"static-write-relro: write did not fault (status={status})")
    else:
        results.append("static-write-relro SIGSEGV")

    if failures:
        return False, "; ".join(failures)
    return True, "; ".join(results)


# The rig of plans/audio.md: QEMU's `wav` backend records what the guest's sound device plays,
# and the checker compares the recording with its source frame by frame. QEMU leaves the header's
# lengths at 0 when the guest powers off, so every sample past the 44-byte header is read.
WAV_HEADER = 44
AUDIO_FRAME = 4  # 16-bit stereo
AUDIO_RATE = 44100
AUDIO_FRAMES = 6 * AUDIO_RATE
AUDIO_MATCH = 8  # consecutive frames that occur only once in either signal
AUDIO_SILENCE = bytes(AUDIO_FRAME)
# `seq` from here prints 7-byte lines, so 8 frames always hold a whole, unique number.
SEQ_FIRST = 100000
AUDIOD_DEFAULT_VOLUME = 50  # audiod's volume at start (audiod/src/scheme.rs)


def rig_signal(frames=AUDIO_FRAMES):
    """The rig's signal: frame n has n mod 65,536 on the left and bits 8 to 23 of a linear congruential sequence on the right."""
    out = bytearray()
    x = 12345
    for n in range(frames):
        out += struct.pack("<HH", n & 0xFFFF, (x >> 8) & 0xFFFF)
        x = (1103515245 * x + 12345) % (1 << 31)
    return bytes(out)


def seq_signal(frames=AUDIO_FRAMES):
    """AU0's signal, made in the guest without a cook: `seq SEQ_FIRST <last> | head -c <bytes>`. Returns (bytes, last)."""
    size = frames * AUDIO_FRAME
    last = SEQ_FIRST + size // 7 + 1
    return b"".join(b"%d\n" % n for n in range(SEQ_FIRST, last + 1))[:size], last


def audiod_gain(source, volume):
    """The source as audiod's mixer sends it at `volume`: each sample times (volume/100)^3, truncated toward zero."""
    factor = (volume / 100) ** 3
    samples = struct.unpack(f"<{len(source) // 2}h", source)
    return struct.pack(f"<{len(samples)}h", *(int(sample * factor) for sample in samples))


def check_wav(source, recording):
    """Compares a recording from QEMU's wav backend with its 16-bit stereo source, frame by frame.

    A piece is a run of recorded frames that continues the source from some frame; it starts where
    AUDIO_MATCH recorded frames occur once in the source. Frames of zeros outside a piece are
    silence, anything else is foreign. Exact frames are the source frames some piece holds.
    """
    frames = [source[i:i + AUDIO_FRAME] for i in range(0, len(source) // AUDIO_FRAME * AUDIO_FRAME, AUDIO_FRAME)]
    data = recording[WAV_HEADER:]
    recorded = [data[i:i + AUDIO_FRAME] for i in range(0, len(data) // AUDIO_FRAME * AUDIO_FRAME, AUDIO_FRAME)]
    starts = {}
    for i in range(len(frames) - AUDIO_MATCH + 1):
        starts.setdefault(b"".join(frames[i:i + AUDIO_MATCH]), []).append(i)
    pieces, silence, foreign, held = [], 0, 0, bytearray(len(frames))
    j = 0
    while j < len(recorded):
        found = starts.get(b"".join(recorded[j:j + AUDIO_MATCH]))
        if found and len(found) == 1:
            start, length = found[0], 0
            while j + length < len(recorded) and start + length < len(frames) \
                    and recorded[j + length] == frames[start + length]:
                length += 1
            pieces.append((j, start, length))
            held[start:start + length] = b"\x01" * length
            j += length
            continue
        if recorded[j] == AUDIO_SILENCE:
            silence += 1
        else:
            foreign += 1
        j += 1
    exact = held.count(1)
    return {"source_frames": len(frames), "recorded_frames": len(recorded), "exact": exact,
            "lost": len(frames) - exact, "repeated": sum(piece[2] for piece in pieces) - exact,
            "silence": silence, "foreign": foreign, "pieces": pieces,
            "one_piece": len(pieces) == 1 and pieces[0][1] == 0 and pieces[0][2] == len(frames)}


def wav_summary(report):
    """One line: exact frames, pieces, and where the source breaks."""
    text = (f"{report['exact']:,} of {report['source_frames']:,} frames exact in {len(report['pieces'])} "
            f"piece{'s' * (len(report['pieces']) != 1)}; lost {report['lost']:,}, repeated {report['repeated']:,}, "
            f"silence {report['silence']:,}, foreign {report['foreign']:,}")
    breaks = [f"source frame {piece[1] + piece[2]:,} ({(piece[1] + piece[2]) / AUDIO_RATE:.3f} s)"
              for piece in report["pieces"][:-1]]
    if breaks:
        text += "; breaks after " + ", ".join(breaks[:5]) + (" …" if len(breaks) > 5 else "")
    return text


def audio_vm(args, out, wav):
    """Boots with the rig's sound device: Intel HDA recorded by QEMU's wav backend at 44.1 kHz.

    `make qemu` adds no sound device (audio=no): a second -audiodev beside its own makes QEMU exit.
    """
    flags = (f"-audiodev wav,id=snd0,path={shlex.quote(str(wav.resolve()))} "
             "-device ich9-intel-hda -device hda-output,audiodev=snd0")
    return VM(args, out, qemu_flags=flags, make=("audio=no",))


def play_seq(vm, timeout, last, volume=None, loops=0):
    """In a root shell: makes AU0's signal with `seq`, sets the volume, starts `loops` busy loops per CPU, and writes the signal to /scheme/audio in whole frames. Returns a problem or None."""
    status, _ = run_status(vm, f"seq {SEQ_FIRST} {last} | head -c {AUDIO_FRAMES * AUDIO_FRAME} > /tmp/signal")
    if status != 0:
        return f"making /tmp/signal with seq failed (status {status})"
    if volume is not None:
        status, _ = run_status(vm, f"echo {volume} > /scheme/audio/volume")
        if status != 0:
            return f"setting /scheme/audio/volume to {volume} failed (status {status})"
    if loops:
        cpus = len(re.findall(r"CPU #\d+", vm.command("info cpus")))
        status, _ = run_status(vm, f"bash -c 'for i in $(seq {loops * cpus}); do (while :; do :; done) & done'")
        if status != 0:
            return f"starting {loops * cpus} busy loops failed (status {status})"
        time.sleep(0.5)
    # 4,096 bytes are 1,024 whole frames; a write shorter than a frame would return 0.
    wait = max(timeout, 120)
    status, output = run_status(vm, "dd if=/tmp/signal of=/scheme/audio bs=4096", wait)
    if status is None:
        return f"dd to /scheme/audio did not finish within {wait:.0f} s"
    if status != 0:
        return f"dd to /scheme/audio failed (status {status}): {output.strip()[-200:]}"
    # What audiod and the driver still hold, at most 4,096 frames, leaves the device in 0.1 s.
    time.sleep(1)
    return None


def test_audio_baseline(args, out):
    """AU0: boots four times with Intel HDA recorded by QEMU's wav backend, logs in as root on the serial console and writes 6 s of 16-bit stereo made by `seq` to /scheme/audio: at volume 100 and at the default volume, each without and with two busy loops per CPU; the host compares each recording with the same bytes. Passes when every recording holds the signal, at the gain audiod applies; the numbers are the result."""
    source, last = seq_signal()
    gains = {100: source, AUDIOD_DEFAULT_VOLUME: audiod_gain(source, AUDIOD_DEFAULT_VOLUME)}
    cases = [("volume-100", 100, 0), ("volume-default", None, 0),
             ("volume-100-load", 100, 2), ("volume-default-load", None, 2)]
    results, problems = {}, []
    for name, volume, loops in cases:
        case_out = out / name
        case_out.mkdir(parents=True, exist_ok=True)
        wav = case_out / "audio.wav"
        wav.unlink(missing_ok=True)
        host_load = Path("/proc/loadavg").read_text().split()[:3]
        vm = audio_vm(args, case_out, wav)
        try:
            problem = "no root serial login" if not root_shell(vm, args.timeout) else \
                play_seq(vm, args.timeout, last, volume, loops)
        finally:
            vm.close()
        report = check_wav(gains[volume or AUDIOD_DEFAULT_VOLUME], wav.read_bytes()) if wav.exists() else None
        results[name] = {"volume": volume or AUDIOD_DEFAULT_VOLUME, "busy_loops_per_cpu": loops,
                         "host_loadavg": host_load, "problem": problem,
                         "summary": wav_summary(report) if report else None, **(report or {})}
        if problem:
            problems.append(f"{name}: {problem}")
        elif not report or not report["exact"]:
            problems.append(f"{name}: the recording holds none of the signal")
    (out / "audio-baseline.json").write_text(json.dumps(results, indent=2) + "\n")
    note = "; ".join(f"{name}: {result['summary']}" for name, result in results.items() if result["summary"])
    if problems:
        return False, "; ".join(problems) + f"; {note}; see {out}"
    return True, f"{note}; see {out}"


def ihdad_descriptors(vm, tag):
    """The number of descriptors ihdad holds, from /scheme/sys/iostat, or None."""
    iostat = read_iostat(vm, tag)
    if iostat is None:
        return None
    (vm.out / f"{tag}.txt").write_text(iostat)
    threads = iostat_processes(iostat).get("ihdad")
    return sum(len(rows) for _, rows in threads) if threads else None


def test_audio_ring(args, out):
    """AU1, issue #8: boots with Intel HDA recorded by QEMU's wav backend, logs in as root on the serial console and runs `audio-probe ring-churn 20` (opens and closes the driver's shared ring 20 times), then writes AU0's signal to /scheme/audio at volume 100 and checks the console still answers. Passes when every round opens the ring, ihdad's descriptor count in /scheme/sys/iostat is back where it started, the signal is exact in one piece and the console answers (x86_64 desktop)."""
    source, last = seq_signal()
    wav = out / "audio.wav"
    wav.unlink(missing_ok=True)
    problems, notes = [], []
    vm = audio_vm(args, out, wav)
    try:
        if not root_shell(vm, args.timeout):
            return False, "no root serial login"
        before = ihdad_descriptors(vm, "iostat-before")
        since = vm.mark()
        vm.type("audio-probe ring-churn 20; echo churn-exit-$?\n")
        found = vm.wait_serial(r"\nchurn-exit-(\d+)", 60, since)
        (out / "ring-churn.txt").write_text(vm.text(since))
        rounds = re.findall(r"audio-probe: round (\d+): (ok|FAIL .*)", vm.text(since))
        failed = [f"round {n}: {result.strip()}" for n, result in rounds if result != "ok"]
        notes.append(f"{len(rounds) - len(failed)} of 20 rounds opened the ring")
        if failed:
            problems.append("first failure " + failed[0])
        if not found:
            problems.append("ring-churn did not finish within 60 s")
            return False, "; ".join(problems + notes) + f"; see {out}"
        if found.group(1) != "0":
            problems.append(f"ring-churn exited {found.group(1)}")
        after = ihdad_descriptors(vm, "iostat-after")
        notes.append(f"ihdad descriptors {before} before, {after} after")
        if before is None or after != before:
            problems.append("ihdad's descriptor count did not return to its start")
        problem = play_seq(vm, args.timeout, last, volume=100)
        if problem:
            problems.append(problem)
        since = vm.mark()
        vm.type("echo console-$((40+2))\n")
        if not vm.wait_serial(r"\nconsole-42", 10, since):
            problems.append("the serial console did not answer within 10 s")
    finally:
        vm.close()
    report = check_wav(source, wav.read_bytes()) if wav.exists() else None
    notes.append("signal: " + (wav_summary(report) if report else "no recording"))
    if not report or not report["one_piece"] or report["foreign"]:
        problems.append("the signal is not exact in one piece")
    if problems:
        return False, "; ".join(problems + notes) + f"; see {out}"
    return True, "; ".join(notes) + f"; see {out}"


def audio_tone_report(recording, expected, absent=None):
    """Project complete one-second windows onto known tones, without another WAV parser."""
    import math
    data = recording[WAV_HEADER:]
    samples = list(struct.iter_unpack("<hh", data[:len(data) // AUDIO_FRAME * AUDIO_FRAME]))
    peaks = {}
    for hz in (expected,) if absent is None else (expected, absent):
        cosine = [math.cos(math.tau * hz * n / AUDIO_RATE) for n in range(AUDIO_RATE)]
        sine = [math.sin(math.tau * hz * n / AUDIO_RATE) for n in range(AUDIO_RATE)]
        channels = []
        for channel in (0, 1):
            amplitudes = []
            for start in range(0, len(samples) - AUDIO_RATE + 1, AUDIO_RATE):
                window = samples[start:start + AUDIO_RATE]
                real = sum(frame[channel] * c for frame, c in zip(window, cosine))
                imag = sum(frame[channel] * s for frame, s in zip(window, sine))
                amplitudes.append(2 * math.hypot(real, imag) / AUDIO_RATE / 32767)
            channels.append(max(amplitudes, default=0))
        peaks[hz] = channels
    target = 0.25 if absent is None else 0.125
    errors = [20 * math.log10(value / target) if value else -999 for value in peaks[expected]]
    return {"amplitudes": peaks, "gain_error_db": errors,
            "ok": all(abs(error) <= 0.1 for error in errors)
                  and (absent is None or max(peaks[absent]) <= target * 0.001)}


def audio_stats(vm):
    status, text = run_status(vm, "cat /scheme/audio/stats")
    if status != 0:
        raise RuntimeError(f"audio stats read failed: {text.strip()}")
    for line in text.splitlines():
        if line.strip().startswith("{"):
            return json.loads(line)
    raise RuntimeError("audio stats returned no JSON")


def audio_cli_smoke(vm, results, problems):
    """Keep the CLI envelope and validate the public payload, not human output."""
    records = results.setdefault("cli", {})
    for command in ("status", "apps", "volume 100 --yes", "volume"):
        status, text = run_status(vm, f"ferrion --json audio {command}")
        lines = [line.strip() for line in text.splitlines() if line.lstrip().startswith("{")]
        record = None
        if len(lines) == 1:
            try:
                record = json.loads(lines[0])
            except ValueError:
                pass
        records[command] = {"status": status, "output": text, "envelope": record}
        if (status != 0 or not isinstance(record, dict) or record.get("schema_version") != 1
                or record.get("ok") is not True or record.get("exit_code") != 0
                or record.get("command") != f"audio {command.split()[0]}"
                or not isinstance(record.get("data"), dict)):
            problems.append(f"ferrion audio {command}: invalid success envelope")
            continue
        data = record["data"]
        if command == "status":
            if (data.get("rate") != AUDIO_RATE or data.get("channels") != 2
                    or data.get("format") != "s16le" or data.get("volume") != 100):
                problems.append("ferrion audio status: wrong device payload")
        elif command == "apps":
            if not isinstance(data.get("apps"), list):
                problems.append("ferrion audio apps: missing apps array")
        elif data.get("volume") != 100:
            problems.append(f"ferrion audio {command}: volume is not 100")


def test_audio_apps(args, out):
    """AU2: legacy tone and exact PCM, two factory-backed subprocess tones with independent program gains, readable volume, and 10,000 stream closes with flat audiod private memory and bounded stats. Uses two HDA/WAV boots; requires the rebuilt audio-probe even on the red image."""
    results, problems = {}, []
    # This boot works on the old daemon too: a missing probe is not the expected red.
    legacy_out = out / "legacy"
    legacy_out.mkdir(parents=True, exist_ok=True)
    wav = legacy_out / "audio.wav"
    wav.unlink(missing_ok=True)
    vm = audio_vm(args, legacy_out, wav)
    try:
        if not root_shell(vm, args.timeout):
            return False, "no root serial login"
        source, last = seq_signal()
        problem = play_seq(vm, args.timeout, last, volume=100)
        if problem:
            problems.append(problem)
        status, text = run_status(vm, "audio-probe tone 997 6", 120)
        (legacy_out / "tone.txt").write_text(text)
        if status != 0:
            problems.append(f"legacy tone probe failed: {status}; rebuild audio-probe before the red boot")
        time.sleep(1)  # Drain the device, as in play_seq.
    finally:
        vm.close()
    recording = wav.read_bytes() if wav.exists() else b""
    exact = check_wav(source, recording)
    results["legacy_pcm"] = exact
    results["legacy_tone"] = audio_tone_report(recording, 997)
    if not exact["one_piece"] or not results["legacy_tone"]["ok"]:
        problems.append("legacy PCM or legacy tone failed")

    apps_out = out / "apps"
    apps_out.mkdir(parents=True, exist_ok=True)
    wav = apps_out / "audio.wav"
    wav.unlink(missing_ok=True)
    vm = audio_vm(args, apps_out, wav)
    try:
        if not root_shell(vm, args.timeout):
            return False, "no root serial login"
        status, text = run_status(vm, "echo 100 > /scheme/audio/volume && cat /scheme/audio/volume")
        results["volume"] = {"status": status, "output": text}
        if status != 0 or not re.search(r"(?m)^100\s*$", text):
            problems.append("cat /scheme/audio/volume did not read 100 successfully")
        status, text = run_status(vm, "echo mute > /scheme/audio/program/tone-a/gain && "
                                      "echo -6.020599913279624 > /scheme/audio/program/tone-b/gain")
        results["controls"] = {"status": status, "output": text}
        if status != 0:
            problems.append("program gain controls failed (expected red on the old daemon)")
        else:
            audio_cli_smoke(vm, results, problems)
            status, text = run_status(vm, "bash -c 'audio-probe stream tone-a 997 6 & a=$!; "
                                          "audio-probe stream tone-b 1499 6 & b=$!; "
                                          "wait $a; sa=$?; wait $b; sb=$?; "
                                          "test $sa -eq 0 && test $sb -eq 0'", 120)
            (apps_out / "tones.txt").write_text(text)
            if status != 0 or text.count("audio-probe: tone complete") != 2:
                problems.append("both factory tone subprocesses did not complete")
            time.sleep(1)
            status, text = run_status(vm, "audio-probe churn 100", 120)
            if status != 0:
                problems.append("stream churn warmup failed")
            before_stats = audio_stats(vm)
            before = process(context_rows(vm), "ferrion-audiod")
            status, text = run_status(vm, "audio-probe churn 10000", 300)
            (apps_out / "churn.txt").write_text(text)
            after_stats = audio_stats(vm)
            after = process(context_rows(vm), "ferrion-audiod")
            results["churn"] = {"before": before, "after": after,
                                "before_stats": before_stats, "after_stats": after_stats}
            if status != 0 or "audio-probe: churn: 10000 closed" not in text:
                problems.append("10,000 stream open/closes did not complete")
            if before is None or after is None or before[0] != after[0] or after[1] > before[1]:
                problems.append("audiod private memory did not stay flat after warmup")
            for key in ("streams", "handles", "queued_frames"):
                if key not in before_stats or key not in after_stats or after_stats[key] > before_stats[key]:
                    problems.append(f"audio stats {key} is missing or grew after churn")
    finally:
        vm.close()
    results["tones"] = audio_tone_report(wav.read_bytes() if wav.exists() else b"", 1499, 997)
    if not results["tones"]["ok"]:
        problems.append("1499 Hz is not within 0.1 dB of half gain or muted 997 Hz exceeds -60 dB")
    (out / "audio-apps.json").write_text(json.dumps(results, indent=2) + "\n")
    return not problems, ("; ".join(problems) if problems else
                          "legacy PCM/tone, independent gains, readable volume, and flat 10,000-stream churn") + f"; see {out}"


def heard(text):
    """The numbers of audio-probe's `heard:` line, or None."""
    found = re.search(r"audio-probe: heard: frames=(\d+) nonzero=(\d+) peak=(\S+) crossings_per_second=(\S+) "
                      r"level_440=(\S+) level_880=(\S+)", text)
    if not found:
        return None
    keys = ("frames", "nonzero", "peak", "crossings_per_second", "level_440", "level_880")
    return {key: (int if key in ("frames", "nonzero") else float)(value) for key, value in zip(keys, found.groups())}


def test_audio_snoop(args, out):
    """AU3: hearing is refused by default. Boots the audio-snoop config (its server has a loopback input and a microphone grant for the session `serial-console`) with the sound device of the other audio checks. As root on the serial console, a 440 Hz tone plays into the loopback input and a program `tone-c` plays 880 Hz. Passes when a record stream without a grant opens, reads and exits 0 with zeros only; one in the session `serial-console` hears the tone (zero crossings within 2 percent of 880 a second) and nothing of 880 Hz; a tap on `tone-c` is zeros without a grant, zeros with a grant naming another program, the 880 Hz tone with a grant naming `tone-c`, and zeros again once the process holding the capture service's handle is killed (x86_64, --config audio-snoop)."""
    wav = out / "audio.wav"
    wav.unlink(missing_ok=True)
    results, problems = {}, []
    probe_seconds = 3
    frames = probe_seconds * AUDIO_RATE

    def run(name, line, timeout=60):
        status, text = run_status(vm, line, timeout)
        (out / f"{name}.txt").write_text(text)
        results[name] = {"status": status, "heard": heard(text), "errors": re.findall(r"audio-probe: FAIL .*", text)}
        return results[name]

    def silent(name, result):
        numbers = result["heard"]
        if result["status"] != 0 or result["errors"]:
            problems.append(f"{name}: exit {result['status']}, {result['errors'] or 'no output'} (zeros were due, without an error)")
        elif not numbers or numbers["frames"] != frames or numbers["nonzero"]:
            problems.append(f"{name}: not {frames:,} frames of zeros: {numbers}")

    def tone(name, result, hz, absent):
        numbers = result["heard"]
        if result["status"] != 0 or not numbers:
            problems.append(f"{name}: exit {result['status']}, {result['errors'] or 'no output'}")
            return
        crossings = 2 * hz
        if abs(numbers["crossings_per_second"] - crossings) > 0.02 * crossings:
            problems.append(f"{name}: {numbers['crossings_per_second']} zero crossings a second, not within 2% of {crossings}")
        # The probe plays 0.25 full scale; the other tone must stay 60 dB below that.
        if numbers[f"level_{hz}"] < 0.2 or numbers[f"level_{absent}"] > 0.25e-3:
            problems.append(f"{name}: level at {hz} Hz {numbers[f'level_{hz}']}, at {absent} Hz {numbers[f'level_{absent}']}")

    vm = audio_vm(args, out, wav)
    try:
        if not root_shell(vm, args.timeout):
            return False, "no root serial login"
        # Both play for longer than the steps below take.
        run("start", "bash -c 'audio-probe loopback 440 90 > /tmp/loopback.txt 2>&1 & "
                     "audio-probe stream tone-c 880 90 > /tmp/tone-c.txt 2>&1 &'")
        time.sleep(2)
        result = run("record-no-grant", f"audio-probe record snoop audio-probe {probe_seconds}")
        if result["errors"] and "openat record" in result["errors"][0]:
            problems.append(f"no record handle: {result['errors'][0]}")
            return False, "; ".join(problems) + f"; see {out}"
        silent("record-no-grant", result)
        tone("record-test-grant", run("record-test-grant", f"audio-probe record listener serial-console {probe_seconds}"), 440, 880)
        silent("tap-no-grant", run("tap-no-grant", f"audio-probe tap listener serial-console tone-c {probe_seconds}"))
        def hold(name, target):
            """Starts `audio-probe grant` for a tap on `target` and waits until it holds the handle."""
            run(name, f"bash -c 'audio-probe grant 600 \"tap session:serial-console {target}\" "
                      f"> /tmp/{name}.txt 2>&1 & echo $! > /tmp/{name}.pid'")
            for _ in range(30):
                status, text = run_status(vm, f"cat /tmp/{name}.txt")
                if "audio-probe: grants:" in text or "audio-probe: FAIL" in text:
                    break
                time.sleep(1)
            if '"lease":"capture"' not in text or f'"target":"{target}"' not in text:
                problems.append(f"{name}: the capture handle's grant was not listed: {text.strip()[-300:]}")

        def release(name):
            """Ends the holder of the capture handle; its grants must end with the handle."""
            run(f"{name}-end", f"bash -c 'kill $(cat /tmp/{name}.pid)'")
            for _ in range(30):
                status, text = run_status(vm, "cat /scheme/audio/stats; echo")
                if re.search(r'"grants":1\b', text):
                    return
                time.sleep(1)
            problems.append(f"{name}: its grant outlived the capture handle: {text.strip()[-300:]}")

        hold("grant-d", "tone-d")
        silent("tap-grant-names-other", run("tap-grant-names-other", f"audio-probe tap listener serial-console tone-c {probe_seconds}"))
        release("grant-d")
        hold("grant-c", "tone-c")
        tone("tap-grant-names-it", run("tap-grant-names-it", f"audio-probe tap listener serial-console tone-c {probe_seconds}"), 880, 440)
        release("grant-c")
        silent("tap-after-release", run("tap-after-release", f"audio-probe tap listener serial-console tone-c {probe_seconds}"))
        run("stats", "cat /scheme/audio/stats; echo")
    finally:
        vm.close()
    (out / "audio-snoop.json").write_text(json.dumps(results, indent=2) + "\n")
    return not problems, ("; ".join(problems) if problems else
                          "zeros without a grant and no error; the test grant hears 440 Hz and no 880 Hz; "
                          "a tap hears tone-c only while a grant names it") + f"; see {out}"


def test_mesa_cpu(vm, args):
    """Mesa 26.3.1 CPU GL and Vulkan reproduce the exact triangle (use --config mesa-cpu on x86_64)."""
    if not root_shell(vm, args.timeout):
        return False, "no root shell"
    since = vm.mark()
    vm.type("bash -c 'gpu-probe mesa-cpu; echo MESA-CPU-EXIT-$?'\n")
    finished = vm.wait_serial(r"MESA-CPU-EXIT-(\d+)", args.timeout, since)
    output = vm.text(since)
    (vm.out / "mesa-cpu.txt").write_text(output)
    if not finished or finished.group(1) != "0":
        return False, "CPU graphics probe failed; see mesa-cpu.txt"
    for api in ("gl", "vulkan"):
        expected = f"GPU_PIXELS api={api} triangle=17540 mismatches=0 hash=47972063c7a47bfb"
        if expected not in output:
            return False, f"missing exact {api} pixels; see mesa-cpu.txt"
    if "GPU_MESA_CPU_OK" not in output:
        return False, "missing GPU_MESA_CPU_OK; see mesa-cpu.txt"
    return True, "Mesa CPU versions and both exact frames checked"


GL_EXACT_VENUS_TRACE = ("virtio_gpu_cmd_ctx_submit",)


def test_gl_exact(vm, args):
    """OpenGL ES 2 exact triangle through Zink over lavapipe (default) or Venus (--venus, unproven until sync)."""
    if not root_shell(vm, args.timeout):
        return False, "no root shell"
    env = ["MESA_LOADER_DRIVER_OVERRIDE=zink"]
    if args.venus:
        icd = args.venus_icd or f"/usr/share/vulkan/icd.d/virtio_icd.{args.arch}.json"
        env.insert(0, f"VK_DRIVER_FILES={icd}")
    else:
        env.insert(0, "LIBGL_ALWAYS_SOFTWARE=1")
    cmd = f"{' '.join(env)} gpu-probe gl-exact"
    since = vm.mark()
    vm.type(f"bash -c '{cmd}; echo GL-EXACT-EXIT-$?'\n")
    finished = vm.wait_serial(r"GL-EXACT-EXIT-(\d+)", args.timeout, since)
    output = vm.text(since)
    (vm.out / "gl-exact.txt").write_text(output)
    if not finished or finished.group(1) != "0":
        return False, "gl-exact probe failed; see gl-exact.txt"
    match = re.search(r'GPU_GL_EXACT version="([^"]*)" renderer="([^"]*)"', output)
    if not match:
        return False, "missing GPU_GL_EXACT line; see gl-exact.txt"
    renderer = match.group(2)
    if "zink" not in renderer.lower():
        return False, f"renderer is not Zink ({renderer!r}); see gl-exact.txt"
    if args.venus:
        low = renderer.lower()
        if "venus" not in low or "llvmpipe" in low:
            return False, f"renderer does not name Zink on Venus ({renderer!r}); see gl-exact.txt"
        submits = [line for line in vm.trace() if "virtio_gpu_cmd_ctx_submit" in line]
        (vm.out / "gl-exact-trace.txt").write_text("\n".join(submits) + ("\n" if submits else ""))
        if not submits:
            return False, "QEMU trace has no virtio_gpu_cmd_ctx_submit lines; see gl-exact-trace.txt"
    expected = "GPU_PIXELS api=gl-exact triangle=17540 mismatches=0 hash=47972063c7a47bfb"
    if expected not in output:
        return False, "missing exact gl-exact pixels; see gl-exact.txt"
    if "GPU_GL_EXACT_OK" not in output:
        return False, "missing GPU_GL_EXACT_OK; see gl-exact.txt"
    backend = "Venus" if args.venus else "lavapipe"
    return True, f"Zink ({backend}) exact GLES2 frame checked: {renderer}"


GPU_CONTEXT_TRACE = ("virtio_gpu_features", "virtio_gpu_cmd_ctx_create", "virtio_gpu_cmd_ctx_destroy")
GPU_BLOB_TRACE = (
    "virtio_gpu_features",
    "virtio_gpu_cmd_ctx_create",
    "virtio_gpu_cmd_ctx_submit",
    "virtio_gpu_cmd_ctx_destroy",
    "virtio_gpu_cmd_res_create_blob",
    "virtio_gpu_cmd_res_map_blob",
    "virtio_gpu_cmd_res_unmap_blob",
    "virtio_gpu_cmd_res_unref",
)
GPU_SUBMIT_TRACE = (
    "virtio_gpu_features",
    "virtio_gpu_cmd_ctx_create",
    "virtio_gpu_cmd_ctx_submit",
    "virtio_gpu_fence_resp",
    "virtio_gpu_cmd_ctx_destroy",
)
VENUS_CAPSET_SHA256 = "11069f17d3858d6b725a3b2b2b810fafc6ae6529ccdedf45bab62acdcb342609"


def reference_venus_capset():
    """The Venus capset's sha256 from this checkout's newest reference-guest record, and where it came from."""
    for record in sorted((ROOT / "build" / "gpu-reference").glob("run-*/result.json"), reverse=True):
        try:
            variants = json.loads(record.read_text()).get("variants", [])
        except (OSError, ValueError, AttributeError):
            continue
        for variant in variants:
            for run in variant.get("runs", []):
                capset = (run.get("guest") or {}).get("venus_capset") or {}
                if re.fullmatch(r"[0-9a-f]{64}", str(capset.get("sha256", ""))):
                    return capset["sha256"], str(record.relative_to(ROOT))
    return VENUS_CAPSET_SHA256, "the reference guest's record of 2026-11-21"


def test_gpu_context(vm, args):
    """Logs in as root on the serial console and runs gpu-probe-context on a 3D device (gpu=venus unless --make names another): render handles on the GPU's render scheme give Venus contexts named after the probe, the capsets include 1, 2 and 4 and the Venus capset's sha256 equals the reference guest's, display requests are refused, a second handle reaches neither the first's context nor its numbers, and QEMU's trace shows virgl acknowledged and each context created and destroyed with its own handle."""
    tag = "gpu-probe-context"
    if not root_shell(vm, args.timeout):
        return False, "no root serial login"
    status, output = guest_run(vm, "gc", tag, 120)
    _, schemes = guest_run(vm, "gl", "ls /scheme", 30)
    log = vm.out / "gpu-context.txt"
    log.write_text(f"exit {status}\n{output}\n[ls /scheme]\n{schemes}\n")
    reasons = []
    fields = dict(re.findall(rf"(?m)^{tag}: (render|capsets|context-name|context-name-second|context-name-third) (\S+)",
                             output))
    capset = re.search(rf"(?m)^{tag}: venus-capset size (\d+) sha256 ([0-9a-f]{{64}})", output)
    failed = re.findall(rf"(?m)^{tag}: FAIL (\S+):", output)
    if "render" not in fields:
        reasons.append("the probe found no render name")
    elif status != 0 or not re.search(rf"(?m)^{tag}: PASS\s*$", output):
        reasons.append(f"probe exit {status}, failed checks: {', '.join(failed) or 'no result line'}")
    if "render" in fields:
        listed = schemes.split()
        if fields["render"] not in listed or "display.virtio-gpu" not in listed:
            reasons.append("/scheme no longer lists the render and display names after the probe")
    ids = sorted(int(i) for i in fields.get("capsets", "").split(",") if i.isdigit())
    if "render" in fields and not {1, 2, 4} <= set(ids):
        reasons.append(f"capsets {ids}, not 1, 2 and 4")
    want_sha, reference = reference_venus_capset()
    if "render" in fields and (not capset or capset.group(2) != want_sha):
        reasons.append(f"Venus capset sha256 {capset.group(2) if capset else 'missing'}, the reference "
                       f"({reference}) is {want_sha}")

    names = [fields.get(key) for key in ("context-name", "context-name-second", "context-name-third")]
    deadline = time.monotonic() + 10
    while True:
        trace = vm.trace()
        destroyed = [int(m.group(1), 16) for line in trace
                     if (m := re.search(r"virtio_gpu_cmd_ctx_destroy\b.*?\bctx (0x[0-9a-f]+)", line))]
        if len(destroyed) >= 3 or time.monotonic() >= deadline or "render" not in fields:
            break
        time.sleep(0.5)
    features = [line for line in trace if re.search(r"\bvirtio_gpu_features\b", line)]
    events = []
    for line in trace:
        if m := re.search(r"virtio_gpu_cmd_ctx_create\b.*?\bctx (0x[0-9a-f]+), name (.*)$", line):
            events.append(("create", int(m.group(1), 16), m.group(2).strip()))
        elif m := re.search(r"virtio_gpu_cmd_ctx_destroy\b.*?\bctx (0x[0-9a-f]+)", line):
            events.append(("destroy", int(m.group(1), 16), None))
    (vm.out / "gpu-context-trace.txt").write_text("\n".join(features + [f"{kind} {ctx:#x} {name or ''}"
                                                                      for kind, ctx, name in events]) + "\n")
    if not any(re.search(r"\bvirgl 1\b", line) for line in features):
        reasons.append("QEMU's trace has no `virtio_gpu_features virgl 1`")
    created = [(ctx, name) for kind, ctx, name in events if kind == "create"]
    if not created:
        reasons.append("QEMU's trace has no virtio_gpu_cmd_ctx_create line")
    elif all(names):
        contexts = []
        for name in names:
            matching = [ctx for ctx, created_name in created if created_name == name]
            if len(matching) != 1:
                reasons.append(f"{len(matching)} contexts named {name} in QEMU's trace, not 1")
            contexts.append(matching[0] if matching else None)
        if None not in contexts:
            first, second, third = contexts
            order = [(kind, ctx) for kind, ctx, _ in events if ctx in contexts]
            if len(set(contexts)) != 3:
                reasons.append(f"the three handles' contexts share a number: {[hex(c) for c in contexts]}")
            elif order.count(("destroy", first)) != 1 or order.count(("destroy", second)) != 1 \
                    or order.count(("destroy", third)) != 1:
                reasons.append(f"each context is not destroyed once: {order}")
            elif not (order.index(("create", first)) < order.index(("create", second))
                      < order.index(("destroy", second)) < order.index(("destroy", first))):
                reasons.append(f"the second handle's close did not leave the first's context alone: {order}")
    elif "render" in fields:
        reasons.append("the probe printed no context names")
    if reasons:
        return False, "; ".join(reasons) + f"; see {log}"
    return True, (f"{fields['render']}: capsets {','.join(map(str, ids))}, Venus capset sha256 matches {reference}; "
                  f"trace: virgl 1, contexts {', '.join(names)} created and each destroyed with its own handle; "
                  f"{output.count(f'{tag}: pass ')} probe checks passed")


def parse_hostmem_bytes(make_args):
    value = "4G"
    for item in make_args:
        if item.startswith("QEMU_GPU_HOSTMEM="):
            value = item.split("=", 1)[1].strip()
    m = re.fullmatch(r"(\d+)([KMG]?)", value, re.IGNORECASE)
    if not m:
        return 4 * 1024 * 1024 * 1024
    mult = {"": 1, "K": 1024, "M": 1024 ** 2, "G": 1024 ** 3}[m.group(2).upper()]
    return int(m.group(1)) * mult


def test_gpu_blob(vm, args):
    """Logs in as root on the serial console and runs gpu-probe-blob on a 3D device (gpu=venus unless --make names another): verifies the 4 GiB host-visible window size in the boot log, creates/maps/reads back/unmaps/closes host-visible blobs including a 131,268-byte Venus ring blob, and checks QEMU's trace for blob create and map events on the probe's context."""
    tag = "gpu-probe-blob"
    if not root_shell(vm, args.timeout):
        return False, "no root serial login"
    status, output = guest_run(vm, "gb", tag, 120)
    _, schemes = guest_run(vm, "gl", "ls /scheme", 30)
    log = vm.out / "gpu-blob.txt"
    log.write_text(f"exit {status}\n{output}\n[ls /scheme]\n{schemes}\n")
    reasons = []
    boot_log = vm.text(0)
    want_window = parse_hostmem_bytes(args.make)
    win = re.search(
        r"virtio-gpu: host-visible window bar=(\d+) offset=(0x[0-9a-f]+) phys=(0x[0-9a-f]+) size=(0x[0-9a-f]+)",
        boot_log,
    )
    if not win:
        reasons.append("serial log has no host-visible window line")
    else:
        win_phys = int(win.group(3), 16)
        win_size = int(win.group(4), 16)
        if win_phys == 0:
            reasons.append("host-visible window phys address is 0")
        if win_size != want_window:
            reasons.append(f"host-visible window size {win_size:#x}, expected {want_window:#x}")
    fields = dict(re.findall(rf"(?m)^{tag}: (render|context-name) (\S+)", output))
    blob_match = re.search(rf"(?m)^{tag}: blob bo=(\d+) res=(\d+) size=(\d+)", output)
    failed = re.findall(rf"(?m)^{tag}: FAIL (\S+):", output)
    if "render" not in fields:
        reasons.append("the probe found no render name")
    elif status != 0 or not re.search(rf"(?m)^{tag}: PASS\s*$", output):
        reasons.append(f"probe exit {status}, failed checks: {', '.join(failed) or 'no result line'}")
    if "render" in fields:
        listed = schemes.split()
        if fields["render"] not in listed or "display.virtio-gpu" not in listed:
            reasons.append("/scheme no longer lists the render and display names after the probe")
    deadline = time.monotonic() + 10
    while True:
        trace = vm.trace()
        destroyed = [int(m.group(1), 16) for line in trace
                     if (m := re.search(r"virtio_gpu_cmd_ctx_destroy\b.*?\bctx (0x[0-9a-f]+)", line))]
        if len(destroyed) >= 2 or time.monotonic() >= deadline or "render" not in fields:
            break
        time.sleep(0.5)
    (vm.out / "gpu-blob-trace.txt").write_text("\n".join(trace) + "\n")
    ctx_name = fields.get("context-name", tag)
    ctx_ids = [
        int(m.group(1), 16)
        for line in trace
        if (m := re.search(r"virtio_gpu_cmd_ctx_create\b.*?\bctx (0x[0-9a-f]+), name (.*)$", line))
        and m.group(2).strip() == ctx_name
    ]
    create_lines = [line for line in trace if re.search(r"\bvirtio_gpu_cmd_res_create_blob\b", line)]
    order = re.search(rf"(?m)^{tag}: queue-order submits=(\d+) res=(\d+)", output)
    if not order:
        reasons.append("the probe printed no queue-order result")
    else:
        burst, order_res = int(order.group(1)), int(order.group(2))
        create_at = next((i for i, line in enumerate(trace) if re.search(
            rf"\bvirtio_gpu_cmd_res_create_blob\b.*\bres {order_res:#x}\b", line)), None)
        if create_at is None:
            reasons.append(f"QEMU's trace has no virtio_gpu_cmd_res_create_blob for res {order_res:#x}")
        else:
            before = sum(1 for line in trace[:create_at] if re.search(r"\bvirtio_gpu_cmd_ctx_submit\b", line))
            if before < burst:
                reasons.append(f"only {before} of the {burst} submissions reached the host before blob {order_res:#x}")
    map_lines = [line for line in trace if re.search(r"\bvirtio_gpu_cmd_res_map_blob\b", line)]
    unref_lines = [line for line in trace if re.search(r"\bvirtio_gpu_cmd_res_unref\b", line)]
    first_map = re.search(
        r"virtio-gpu: first map-blob res=(\d+) offset=(0x[0-9a-f]+) size=(0x[0-9a-f]+) map_info=(0x[0-9a-f]+) \((\w+)\)",
        boot_log,
    )
    if not ctx_ids:
        reasons.append(f"QEMU's trace has no virtio_gpu_cmd_ctx_create line for {ctx_name}")
    if not create_lines:
        reasons.append("QEMU's trace has no virtio_gpu_cmd_res_create_blob line")
    elif blob_match:
        res_id = int(blob_match.group(2))
        if not any(re.search(rf"\bres {res_id:#x}, size 135168\b", line) for line in create_lines):
            reasons.append(f"QEMU's trace has no virtio_gpu_cmd_res_create_blob for res {res_id:#x} size 135168")
        if not any(re.search(rf"\bres {res_id:#x}\b", line) for line in unref_lines):
            reasons.append(f"QEMU's trace has no virtio_gpu_cmd_res_unref for res {res_id:#x}")
    if map_lines:
        if blob_match:
            res_id = int(blob_match.group(2))
            if not any(re.search(rf"\bres {res_id:#x}\b", line) for line in map_lines):
                reasons.append(f"QEMU's trace has no virtio_gpu_cmd_res_map_blob for res {res_id:#x}")
    else:
        if not first_map or (blob_match and int(first_map.group(1)) != int(blob_match.group(2))):
            reasons.append("no virtio_gpu_cmd_res_map_blob trace line and no driver map-blob log for the ring blob")
        if f"{tag}: pass host-backing-swap:" not in output:
            reasons.append("missing host-backing-swap proof on QEMU MAP_FIXED path")
    if reasons:
        return False, "; ".join(reasons) + f"; see {log}"
    map_note = (
        f"{len(map_lines)} map_blob"
        if map_lines
        else f"MAP_FIXED fast path (map_info {first_map.group(4)} {first_map.group(5)})"
    )
    return True, (f"{fields['render']}: window {win.group(4)} at {win.group(3)}; "
                  f"trace: {len(create_lines)} create_blob, {map_note}, {len(unref_lines)} unref; "
                  f"{output.count(f'{tag}: pass ')} probe checks passed")


def test_gpu_submit(vm, args):
    """Logs in as root on the serial console and runs gpu-probe-submit on a 3D device (gpu=venus unless --make names another): submits on ring 0 with FENCE_FD_OUT and polls the sync file it gets until signaled, merges two submission fences, signals binary and timeline sync objects named as out sync objects, waits on in sync objects and on a FENCE_FD_IN sync file that a sync object gates, checks a 300 ms poll timeout on an unsignaled sync file lands in 270..400 ms, verifies refusal paths, and confirms virtio_gpu_cmd_ctx_submit and a virtio_gpu_fence_resp for every submission in QEMU's trace."""
    tag = "gpu-probe-submit"
    if not root_shell(vm, args.timeout):
        return False, "no root serial login"
    status, output = guest_run(vm, "gs", tag, 120)
    _, schemes = guest_run(vm, "gsl", "ls /scheme", 30)
    log = vm.out / "gpu-submit.txt"
    log.write_text(f"exit {status}\n{output}\n[ls /scheme]\n{schemes}\n")
    reasons = []
    fields = dict(re.findall(rf"(?m)^{tag}: (render|context-name) (\S+)", output))
    failed = re.findall(rf"(?m)^{tag}: FAIL (\S+):", output)
    signaled = re.search(rf"(?m)^{tag}: submit-ring0 signaled wait_ms=(\d+)", output)
    timeout_poll = re.search(rf"(?m)^{tag}: timeout-poll ret=(-?\d+) revents=(0x[0-9a-f]+|0) elapsed_ms=(\d+)", output)
    if "render" not in fields:
        reasons.append("the probe found no render name")
    elif status != 0 or not re.search(rf"(?m)^{tag}: PASS\s*$", output):
        reasons.append(f"probe exit {status}, failed checks: {', '.join(failed) or 'no result line'}")
    if "render" in fields:
        listed = schemes.split()
        if fields["render"] not in listed or "display.virtio-gpu" not in listed:
            reasons.append("/scheme no longer lists the render and display names after the probe")
    if not signaled:
        reasons.append("ring 0 submission fence did not signal")
    if not timeout_poll:
        reasons.append("missing 300 ms unsignaled fence poll result")
    else:
        t_ret, t_revents, t_ms = int(timeout_poll.group(1)), int(timeout_poll.group(2), 16), int(timeout_poll.group(3))
        if t_ret != 0 or t_revents != 0 or not (270 <= t_ms <= 400):
            reasons.append(f"300 ms poll returned ret={t_ret} revents={t_revents:#x} in {t_ms} ms")

    ctx_name = fields.get("context-name")
    deadline = time.monotonic() + 10
    while True:
        trace = vm.trace()
        if any(re.search(r"\bvirtio_gpu_cmd_ctx_destroy\b", line) for line in trace) or time.monotonic() >= deadline:
            break
        time.sleep(0.5)
    relevant = [line for line in trace if re.search(
        r"\bvirtio_gpu_(features|cmd_ctx_create|cmd_ctx_submit|fence_resp|cmd_ctx_destroy)\b", line)]
    (vm.out / "gpu-submit-trace.txt").write_text("\n".join(relevant) + ("\n" if relevant else ""))
    if not any(re.search(r"\bvirtio_gpu_features\b.*\bvirgl 1\b", line) for line in relevant):
        reasons.append("QEMU's trace has no `virtio_gpu_features virgl 1`")
    created = []
    for line in relevant:
        if m := re.search(r"virtio_gpu_cmd_ctx_create\b.*?\bctx (0x[0-9a-f]+), name (.*)$", line):
            created.append((int(m.group(1), 16), m.group(2).strip()))
    matching_ctx = [ctx for ctx, name in created if ctx_name and name == ctx_name]
    if len(matching_ctx) != 1:
        reasons.append(f"{len(matching_ctx)} contexts named {ctx_name} in QEMU's trace, not 1")
        ctx_id = None
    else:
        ctx_id = matching_ctx[0]
    submits = [int(m.group(1), 16) for line in relevant
               if (m := re.search(r"virtio_gpu_cmd_ctx_submit\b.*?\bctx (0x[0-9a-f]+)", line))]
    fences = [int(m.group(1), 16) for line in relevant
              if (m := re.search(r"virtio_gpu_fence_resp\b.*?\bfence (0x[0-9a-f]+)", line))]
    if ctx_id is None or ctx_id not in submits:
        reasons.append(f"QEMU's trace has no virtio_gpu_cmd_ctx_submit for context {ctx_id}")
    if not any(fid != 0 for fid in fences):
        reasons.append("QEMU's trace has no non-zero virtio_gpu_fence_resp")
    elif ctx_id is not None and len(fences) < submits.count(ctx_id):
        reasons.append(f"{submits.count(ctx_id)} submissions but {len(fences)} virtio_gpu_fence_resp in QEMU's trace")
    if reasons:
        return False, "; ".join(reasons) + f"; see {log}"
    return True, (f"{fields['render']}: first sync file signaled in {signaled.group(1)} ms; "
                  f"300 ms timeout poll took {timeout_poll.group(3)} ms; "
                  f"trace: {submits.count(ctx_id)} submit(s) for ctx {ctx_id:#x}, {len(fences)} fence_resp; "
                  f"{output.count(f'{tag}: pass ')} probe checks passed")


def test_gpu_syncobj(vm, args):
    """Logs in as root on the serial console and runs gpu-probe-syncobj on a 3D device (gpu=venus unless --make names another): checks timeline waits, 300 ms deadline timing, sync-file export/merge/poll/import, and per-handle syncobj isolation."""
    tag = "gpu-probe-syncobj"
    if not root_shell(vm, args.timeout):
        return False, "no root serial login"
    status, output = guest_run(vm, "gs", tag, 120)
    _, schemes = guest_run(vm, "gl", "ls /scheme", 30)
    log = vm.out / "gpu-syncobj.txt"
    log.write_text(f"exit {status}\n{output}\n[ls /scheme]\n{schemes}\n")
    reasons = []
    render = re.search(rf"(?m)^{tag}: render (\S+)", output)
    timeout_ms = re.search(rf"(?m)^{tag}: timeout-ms (-?\d+)", output)
    wake_ms = re.search(rf"(?m)^{tag}: wake-delay-ms (-?\d+)", output)
    failed = re.findall(rf"(?m)^{tag}: FAIL (\S+):", output)
    if not render:
        reasons.append("the probe found no render name")
    elif status != 0 or not re.search(rf"(?m)^{tag}: PASS\s*$", output):
        reasons.append(f"probe exit {status}, failed checks: {', '.join(failed) or 'no result line'}")
    if render:
        listed = schemes.split()
        if render.group(1) not in listed or "display.virtio-gpu" not in listed:
            reasons.append("/scheme no longer lists the render and display names after the probe")
    if not timeout_ms or not (270 <= int(timeout_ms.group(1)) <= 400):
        reasons.append(f"300 ms deadline took {timeout_ms.group(1) if timeout_ms else 'missing'} ms")
    if not wake_ms or not (0 <= int(wake_ms.group(1)) <= 50):
        reasons.append(f"point-3 wake delay was {wake_ms.group(1) if wake_ms else 'missing'} ms")
    if reasons:
        return False, "; ".join(reasons) + f"; see {log}"
    return True, (f"{render.group(1)}: timeout {timeout_ms.group(1)} ms, point-3 wake {wake_ms.group(1)} ms, "
                  f"{output.count(f'{tag}: pass ')} probe checks passed")


def frame_color_match(frame, rgb):
    want = bytes(rgb)
    total = frame.width * frame.height
    if total == 0:
        return 0.0
    matched = sum(1 for offset in range(0, len(frame.rgb), 3) if frame.rgb[offset:offset + 3] == want)
    return matched / total


def test_gpu_flip(vm, args):
    """Logs in as root on the serial console and runs gpu-probe-flip: 600 page flips deliver 600 flip-complete events with monotonic timestamps and sequence numbers, median interval recorded, and screenshots after even (600) and odd (601) flips show the matching solid framebuffer colors via screendump (2D) or VNC (3D)."""
    tag = "gpu-probe-flip"
    if not root_shell(vm, args.timeout):
        return False, "no root serial login"
    since = vm.mark()
    vm.type("bash -c 'gpu-probe-flip --hold; echo gf-end-$?'\n")
    found = vm.wait_serial(rf"(?m)(?:^{tag}: READY even|gf-end-(\d+))", 180, since)
    if not found:
        output = vm.text(since)
        (vm.out / "gpu-flip.txt").write_text(f"exit None\n{output}\n")
        return False, f"probe timed out before READY even; see {vm.out / 'gpu-flip.txt'}"
    even_ratio = odd_ratio = None
    even_src = odd_src = None
    if found.group(1) is None:
        time.sleep(0.3)
        even_frame = vm.screenshot()
        even_src = vm.last_capture_source
        even_frame.save_png(vm.out / "even.png")
        even_ratio = frame_color_match(even_frame, (255, 96, 32))
        odd_mark = vm.mark()
        vm.type("\n")
        found_odd = vm.wait_serial(rf"(?m)(?:^{tag}: READY odd|gf-end-(\d+))", 60, odd_mark)
        if not found_odd:
            output = vm.text(since)
            (vm.out / "gpu-flip.txt").write_text(f"exit None\n{output}\n")
            return False, f"probe timed out before READY odd; see {vm.out / 'gpu-flip.txt'}"
        if found_odd.group(1) is None:
            time.sleep(0.3)
            odd_frame = vm.screenshot()
            odd_src = vm.last_capture_source
            odd_frame.save_png(vm.out / "odd.png")
            odd_ratio = frame_color_match(odd_frame, (32, 160, 255))
            end_mark = vm.mark()
            vm.type("\n")
            found_end = vm.wait_serial(r"gf-end-(\d+)", 60, end_mark)
            status = int(found_end.group(1)) if found_end else None
        else:
            status = int(found_odd.group(1))
    else:
        status = int(found.group(1))
    output = vm.text(since)
    _, schemes = guest_run(vm, "gfl", "ls /scheme", 30)
    log = vm.out / "gpu-flip.txt"
    log.write_text(f"exit {status}\n{output}\n[ls /scheme]\n{schemes}\n")
    reasons = []
    failed = re.findall(rf"(?m)^{tag}: FAIL (\S+):.*", output)
    stats = re.search(
        rf"(?m)^{tag}: flips (\d+) events (\d+) median_us (\d+) min_us (\d+) max_us (\d+) seq_start (\d+) seq_end (\d+)",
        output,
    )
    if status != 0 or not re.search(rf"(?m)^{tag}: PASS\s*$", output):
        reasons.append(f"probe exit {status}, failed: {'; '.join(failed) or 'no result line'}")
    if not stats or int(stats.group(1)) != 600 or int(stats.group(2)) != 600:
        reasons.append("did not observe 600 events for 600 flips")
    if even_ratio is None or even_ratio < 0.99:
        reasons.append(f"even frame color match {even_ratio} < 0.99")
    if odd_ratio is None or odd_ratio < 0.99:
        reasons.append(f"odd frame color match {odd_ratio} < 0.99")
    want_src = "vnc" if vm.is_3d else "screendump"
    if even_src != want_src or odd_src != want_src:
        reasons.append(f"capture sources ({even_src}, {odd_src}) != {want_src}")
    if "display.virtio-gpu" not in schemes.split():
        reasons.append("/scheme no longer lists display.virtio-gpu")
    if reasons:
        return False, "; ".join(reasons) + f"; see {log}"
    return True, (f"600 flips -> 600 events, median {stats.group(3)} us (min {stats.group(4)}, max {stats.group(5)}), "
                  f"even match {even_ratio:.4f} ({even_src}), odd match {odd_ratio:.4f} ({odd_src})")


VK_EXACT_TRACE = ("virtio_gpu_*",)


def test_vk_exact(vm, args):
    """Logs in as root on the serial console and runs gpu-probe vk-exact against Venus's ICD with debug logs on a 3D device (gpu=venus unless --make names another)."""
    if not root_shell(vm, args.timeout):
        return False, "no root serial login"
    icd = f"/usr/share/vulkan/icd.d/virtio_icd.{args.arch}.json"
    cmd = (f"VK_DRIVER_FILES={icd} MESA_LOG_LEVEL=debug VN_DEBUG=init,result "
           "gpu-probe vk-exact 2>&1")
    status, output = guest_run(vm, "vk", cmd, 120)
    log = vm.out / "vk-exact.txt"
    log.write_text(f"exit {status}\n{output}\n")
    time.sleep(0.5)
    trace = [line for line in vm.trace() if "virtio_gpu" in line]
    trace_log = vm.out / "vk-exact-trace.txt"
    trace_log.write_text("\n".join(trace) + ("\n" if trace else ""))
    reasons = []
    if status != 0:
        reasons.append(f"probe exit {status}")
    expected_pixels = "GPU_PIXELS api=vulkan triangle=17540 mismatches=0 hash=47972063c7a47bfb"
    if expected_pixels not in output:
        reasons.append("missing exact vulkan pixels")
    if "GPU_VK_EXACT_OK" not in output:
        reasons.append("missing GPU_VK_EXACT_OK")
    if not any(re.search(r"\bvirtio_gpu_features\b.*\bvirgl 1\b", line) for line in trace):
        reasons.append("QEMU trace has no virtio_gpu_features virgl 1")
    if not any("virtio_gpu_cmd_ctx_submit" in line for line in trace):
        reasons.append("QEMU trace has no virtio_gpu_cmd_ctx_submit")
    if reasons:
        venus_lines = [line.strip() for line in output.splitlines() if "MESA-VIRTIO:" in line or "GPU_VK" in line or "GPU_ERROR" in line]
        summary = " | ".join(venus_lines[:8])
        extra = f" ({summary})" if summary else ""
        return False, "; ".join(reasons) + extra + f"; see {log} and {trace_log}"
    return True, f"Venus rendered exact vulkan frame with host submit trace; see {log} and {trace_log}"


LOCK_PROBE_CHECKS = (
    "fcntl-conflict", "getlk-holder", "fcntl-shared", "fcntl-split-merge", "seek-cur-and-end",
    "release-on-close", "release-on-close-other-fd", "path-lookups-keep-locks", "release-on-exit", "release-on-kill",
    "ofd-shared-by-fork", "ofd-survives-child-exit", "ofd-conflicts-with-process-lock",
    "ofd-released-on-close", "process-lock-not-inherited", "setlkw-wakes", "ofd-setlkw-wakes",
    "setlkw-eintr", "setlkw-edeadlk", "flock-exclusive-nb", "flock-shared", "flock-wakes",
    "flock-owned-by-description", "flock-independent-of-fcntl", "flock-release-on-close",
)


def test_file_locks(vm, args):
    """Advisory file locks on /tmp, as `user`: lock-probe checks fcntl record locks (conflicts, F_GETLK's holder and pid, splitting and merging, SEEK_CUR and SEEK_END, release on close of any descriptor, on exit and on SIGKILL but not on stat(), access() or closing an O_PATH descriptor, OFD against process ownership across fork, F_SETLKW and F_OFD_SETLKW waking, EINTR, EDEADLK) and flock (LOCK_NB, waiting, ownership by the open description, independence from fcntl); then two sqlite3 processes each commit 100 read-modify-write transactions to one database, in WAL and in rollback-journal mode, with a 20 s busy timeout; then two processes write a WAL database in /home/user and exit, and a read-only connection in a new process reads it, and again after a reboot."""
    ok, note = user_probe(vm, args, "lock-probe", LOCK_PROBE_CHECKS, timeout=240)
    if note and note.startswith("no serial login"):
        return False, note
    problems = [] if ok else [note]
    report, log = [], []
    for mode in ("wal", "delete"):
        status, out = guest_run(vm, f"sq{mode}", f"lock-probe-sqlite {mode} 100", 900)
        log.append(f"[{mode}] exit {status}\n{out}\n")
        found = re.search(rf"sqlite-{mode}: status (\d+) (\d+) result (\S+) (\d+) (\d+) (\d+)", out)
        seconds = re.search(rf"sqlite-{mode}: journal (\S+) seconds (\d+)", out)
        verdict = found.groups() if found else None
        if verdict != ("0", "0", "ok", "200", "200", "200") or re.search(r"locked|busy|malformed", out, re.I):
            problems.append(f"sqlite {mode}: " + (" ".join(verdict) if verdict else "no result"))
        else:
            report.append(f"sqlite {seconds.group(1) if seconds else mode}: 200 of 200 updates in {seconds.group(2) if seconds else '?'} s")
    persist = "/home/user/lock-probe-persist.db"
    reads = []
    for step, line in (("create", f"lock-probe-sqlite create {persist}"), ("read", f"lock-probe-sqlite read {persist}"),
                       ("reboot", None), ("read", f"lock-probe-sqlite read {persist}")):
        if line is None:
            trouble = reboot_to_user(vm, args)
            log.append(f"[reboot] {trouble or 'login as user again'}\n")
            if trouble:
                problems.append(trouble)
                break
            continue
        status, out = guest_run(vm, f"sq{step}", line, 120)
        log.append(f"[{step}] exit {status}\n{out}\n")
        if step == "read":
            reads.append(status == 0 and re.search(r"sqlite-read: rows 100 5050", out) is not None)
            if not reads[-1]:
                failure = re.search(r"sqlite-read: .*", out)
                problems.append(f"read-only reader {len(reads)}: " + (failure.group(0).strip() if failure else f"exit {status}"))
        elif status != 0:
            problems.append(f"sqlite create: exit {status}")
    if reads == [True, True]:
        report.append("read-only reader of a WAL database two exited processes wrote: 100 rows, and after a reboot")
    (vm.out / "file-locks-sqlite.txt").write_text("".join(log))
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    return True, f"lock-probe passed {len(LOCK_PROBE_CHECKS)} checks; " + "; ".join(report)


def reboot_to_user(vm, args):
    """From `user`'s serial login shell: logs out, reboots as root with `shutdown -r`, and logs in as `user` again; returns a problem or None."""
    since = vm.mark()
    vm.type("exit\n")
    ok, err = login(vm, "root", "password", r"root:.*# ", since, timeout=args.timeout)
    if not ok:
        return "no login prompt after logging out" if err == "no login prompt" else err
    since = vm.mark()
    vm.type("shutdown -r\n")
    if not vm.wait_serial(r"login: ", args.timeout * 2, since):
        return "no login prompt after the reboot"
    time.sleep(PROMPT_REDRAW_S)
    vm.type("user\n")
    time.sleep(PROMPT_REDRAW_S)
    return None


SCOPE_MARKER = "ferrion-scope-secret-5c7d"
SCOPE_ROUTES = ["start-folder", "absolute", "scheme-path", "dotdot", "dotdot-many", "stat", "symlink-absolute",
                "symlink-relative", "symlink-dir", "symlink-root", "symlink-scheme", "symlink-chain",
                "symlink-dir-write", "openat-dotdot", "openat-absolute", "chdir-up", "list-outside", "hardlink-in",
                "hardlink-out", "rename-in", "rename-out", "write-out", "delete-out", "race-in-session",
                "inherited-fd", "home-config", "env-home", "env-token", "sessions-folder", "scheme-sudo",
                "scheme-file-scope", "scheme-linux-session", "scheme-pty", "scheme-debug", "scheme-shm", "scheme-log",
                "raw-disk"]
SCOPE_OPTIONAL = {"scheme-linux-session", "raw-disk", "symlink-scheme", "scheme-debug", "scheme-shm"}
SCOPE_STALE_ROUTES = ["stale-dir", "stale-link"]
SCOPE_LINKS = ("cd /home/user/scope && ln -s /home/user/.ssh to-ssh && ln -s inside in && mkdir -p d d2/sub && "
               "ln -s ../.. d/up && ln -s ../in d/back && ln -s ../../x d2/sub/l && mv d2/sub/l l2 && mv p/q/old old2")
SCOPE_LINKS_OUT = {"/home/user/scope/to-ssh", "/home/user/scope/d/up", "/home/user/scope/l2", "/home/user/scope/old2"}
SCOPE_SCHEMES = {"event", "memory", "pipe", "time", "rand", "null", "zero", "sys", "proc", "uds_stream", "uds_dgram",
                 "eventfd", "timerfd", "ip", "icmp", "tcp", "udp", "file"}
SCOPE_TOP = {"bin", "dev", "etc", "home", "include", "lib", "share", "ssl", "tmp", "usr"}
SCOPE_VARS = {"PATH", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "COLUMNS", "LINES", "USER", "LOGNAME", "HOME", "TMPDIR"}


def scope_routes(output):
    return dict(re.findall(r"ROUTE (\S+) (REACHED|REFUSED)", output))


def scope_setup(vm):
    """Creates, as `user`, the folder a session gets (~/scope) and the files outside it the probe goes after."""
    return guest_run(vm, "scset", "rm -rf /home/user/scope /home/user/outside; mkdir -p /home/user/scope "
                     f"/home/user/outside /home/user/.ssh && echo {SCOPE_MARKER} > /home/user/outside/secret.txt && "
                     f"echo {SCOPE_MARKER} > /home/user/.ssh/id_probe && echo victim > /home/user/outside/victim.txt && "
                     "mkdir -p /home/user/.local/state/ferrion-run/decoy-route", 60)


def scope_escape(vm, tag, launcher):
    line = (f"cd /home/user/outside && PROBE_TOKEN={SCOPE_MARKER} {launcher}ferrion-run-probe escape /home/user/scope "
            f"/home/user/outside/secret.txt {SCOPE_MARKER} /home/user/outside/victim.txt /home/user "
            "3</home/user/outside/secret.txt")
    return guest_run(vm, tag, line, 600)


def test_agent_scope(vm, args):
    """Security roadmap S12, storage scopes (product roadmap, agent items 1 and 3): as `user`, ferrion-run-probe escape tries every route out of a folder (absolute and /scheme/file paths, .., symlinks in and out, openat on a folder descriptor, chdir, listing, hard links, renames, writes and deletes outside, a rename-and-symlink race inside the session and between two sessions, an inherited descriptor, the starting folder, the caller's home, environment and ferrion-run's own sessions, and the sudo, file-scope, linux-session, pty, debug, shm, log and disk schemes). Each route must reach the secret without the launcher and be refused under `ferrion-run --files ~/scope`; so must stale-dir and stale-link, a folder and a file descriptor kept while their folder is renamed and a symlink to the secret's folder takes its name (an openat and a flinkat through them), which reach only a decoy without the launcher; as root, a disk must be readable without it and not under it. Then the session's namespace, its top folder and its fresh home and environment (AS2), an explicit read-only share, the hiding of ferrion-run's sessions inside a granted home, also against renaming a folder above them (unhide-rename), a session started by a program that leaks a handle to its own file table (foreign-filetable: without the launcher the probe can copy that table and so take every descriptor in it; under it the session must start and hold no such handle), and the list of symlinks the session left that lead outside its folder (written there, written and then moved, and one that was there before the session and that it moved to a depth where its relative target leaves the folder), printed when it ends."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    log, controls, problems, notes = [], [], [], []

    def run(tag, line, timeout=120):
        reply = guest_run(vm, tag, line, timeout)
        status, output = reply
        log.append(f"$ {line}\nexit {status}\n{output}\n")
        return reply

    status, _ = run("scinit", "ferrion-run true")
    if status != 0:
        problems.append(f"ferrion-run true exited {status}")
    scope_setup(vm)
    status, output = scope_escape(vm, "scfree", "")
    log.append(f"[without ferrion-run] exit {status}\n{output}\n")
    free = scope_routes(output)
    run("screset", "echo victim > /home/user/outside/victim.txt; rm -rf /home/user/scope; mkdir /home/user/scope")
    status, output = scope_escape(vm, "scjail", "ferrion-run --files /home/user/scope ")
    log.append(f"[under ferrion-run --files ~/scope] exit {status}\n{output}\n")
    jailed = scope_routes(output)
    run("scdecoyr", "rmdir /home/user/.local/state/ferrion-run/decoy-route")
    counted = []
    for route in SCOPE_ROUTES:
        if free.get(route) != "REACHED":
            (notes if route in SCOPE_OPTIONAL else controls).append(f"{route} {free.get(route, 'missing')}")
            continue
        counted.append(route)
        if jailed.get(route) != "REFUSED":
            problems.append(f"route {route} {jailed.get(route, 'not run')} under ferrion-run")
    for route in SCOPE_STALE_ROUTES:
        counted.append(route)
        if jailed.get(route) != "REFUSED":
            problems.append(f"route {route} {jailed.get(route, 'not run')} under ferrion-run")
        notes.append(f"{route} without the launcher: {free.get(route, 'not run')}")
    status, output = run("scleft", "ls -A /home/user/outside")
    left = set(output.split())
    if left != {"secret.txt", "victim.txt"}:
        problems.append(f"the session changed the folder outside its scope: {sorted(left)}")

    probe = "ferrion-run-probe race-{} /home/user/scope {} 15"
    swap, read = probe.format("swap", "/home/user/outside"), probe.format("read", f"secret.txt {SCOPE_MARKER}")
    _, output = run("scrace0", f"{swap} > /dev/null & {read}; wait", 120)
    if "ROUTE race-sessions REACHED" not in output:
        controls.append("race-sessions without ferrion-run")
    else:
        _, output = run("scrace1", f"ferrion-run --files /home/user/scope {swap} > /dev/null & "
                                    f"ferrion-run --files /home/user/scope {read}; wait", 180)
        counted.append("race-sessions")
        if "ROUTE race-sessions REFUSED" not in output:
            problems.append("route race-sessions reached the secret under ferrion-run")
    _, output = run("scft0", "ferrion-run-probe hold-filetable ferrion-run-probe foreign-filetable")
    if "ROUTE foreign-filetable REACHED" not in output:
        controls.append("foreign-filetable without ferrion-run")
    else:
        status, output = run("scft1", "ferrion-run-probe hold-filetable ferrion-run --files /home/user/scope "
                                      "ferrion-run-probe foreign-filetable")
        counted.append("foreign-filetable")
        if status != 0 or "ROUTE foreign-filetable REFUSED" not in output:
            problems.append(f"route foreign-filetable: a session that inherited another process's file table exited {status}"
                            + (" and held that table" if "ROUTE foreign-filetable REACHED" in output else ""))

    _, output = run("scns", "ferrion-run ferrion-run-probe schemes")
    found = re.search(r"SCHEMES (.*)", output)
    namespace = found.group(1).split() if found else []
    if not found or set(namespace) - SCOPE_SCHEMES or "file" not in namespace:
        problems.append(f"the session's namespace holds {namespace}")
    _, output = run("sctop", "ferrion-run ls -A /")
    top = set(output.split())
    if top - SCOPE_TOP or "usr" not in top:
        problems.append(f"the session's / holds {sorted(top)}")

    status, before = run("scst0", "ls /home/user/.local/state/ferrion-run | wc -l")
    _, output = run("schome", "ferrion-run ferrion-run-probe home")
    found = re.search(r"HOME (\S+) entries Ok\((\d+)\) tmp Ok\((\d+)\) writable (\w+) vars (\S*)", output)
    if not found or not re.fullmatch(r"/home/user/\.local/state/ferrion-run/\d+-\d+/home", found.group(1)) \
            or found.group(2, 3, 4) != ("0", "0", "true") or set(found.group(5).split(",")) - SCOPE_VARS:
        problems.append("the session's home is not a fresh, empty, writable folder of its own with only the "
                        f"passed variables: {found.group(0) if found else output.strip()[-200:]}")
    status, after = run("scst1", "ls /home/user/.local/state/ferrion-run | wc -l")
    if before.split()[-1:] != after.split()[-1:]:
        problems.append(f"a session left its folder behind ({before.split()[-1:]} then {after.split()[-1:]})")
    status, output = run("scshare", "ferrion-run --read-only /home/user/.ssh/id_probe cat /home/user/.ssh/id_probe")
    if status != 0 or SCOPE_MARKER not in output:
        problems.append(f"an explicit read-only share was not readable (exit {status})")
    reply = run("scshare-w", "ferrion-run --read-only /home/user/.ssh/id_probe bash -c \"echo x >> /home/user/.ssh/id_probe\"")
    if not refused(reply):
        problems.append("a read-only share was writable" if isinstance(reply, Answered) else "a read-only share write gave no answer")
    decoy = "/home/user/.local/state/ferrion-run/decoy-0"
    run("scdecoy", f"mkdir -p {decoy} && echo {SCOPE_MARKER} > {decoy}/x")
    status, output = run("schide", "ferrion-run --files /home/user bash -c "
                                   "\"ls -A /home/user/.local/state/ferrion-run; echo home=\\$HOME\"")
    if "decoy-0" in output.split() or "home=/home/user/.local/state/ferrion-run/" not in output:
        problems.append("inside a session given the whole home, another session's folder was listed or its own home was missing")
    reply = run("schide2", f"ferrion-run --files /home/user cat {decoy}/x")
    if not refused(reply) or SCOPE_MARKER in reply.out:
        problems.append(
            "another session's folder was readable inside a session given the whole home"
            if isinstance(reply, Answered)
            else "another session's folder check gave no answer"
        )
    status, output = run("schide3", "ferrion-run --files /home/user bash -c "
                                    "\"mv /home/user/.local /home/user/.local-moved; mv /home/user/.local/state /home/user/.local/state-moved; "
                                    "cat /home/user/.local-moved/state/ferrion-run/decoy-0/x /home/user/.local/state-moved/ferrion-run/decoy-0/x\"")
    _, moved = run("schide3m", "for d in /home/user/.local-moved /home/user/.local/state-moved; do "
                               "test -e $d && echo $((40+2))MOVED; done; true")
    run("schide3r", "mv /home/user/.local-moved /home/user/.local 2>/dev/null; mv /home/user/.local/state-moved /home/user/.local/state 2>/dev/null; true")
    counted.append("unhide-rename")
    if SCOPE_MARKER in output or "42MOVED" in moved:
        problems.append("route unhide-rename: a session given the whole home renamed a folder above ferrion-run's sessions"
                        + (" and read another session's folder" if SCOPE_MARKER in output else ""))
    run("scdecoy2", f"rm -rf {decoy}")
    run("sclnprep", "rm -rf /home/user/scope && mkdir -p /home/user/scope/p/q && ln -s ../../in /home/user/scope/p/q/old")
    status, output = run("sclinks", f"ferrion-run --files /home/user/scope bash -c \"{SCOPE_LINKS}\"")
    listed = set(re.findall(r"leads outside /home/user/scope: (\S+) ->", output))
    if status != 0 or listed != SCOPE_LINKS_OUT:
        problems.append(f"symlinks leading outside the folder: listed {sorted(listed)}, want {sorted(SCOPE_LINKS_OUT)} (exit {status})")
    _, output = run("sclinks2", "ls -l /home/user/scope /home/user/scope/d")
    if output.count(" -> ") < 5:
        problems.append("the session's symlinks did not stay in the folder")
    run("sclnkrt0", "rm -f /home/user/scope/rtlink /home/user/scope/extlink && ln -s /home/user/outside/secret.txt /home/user/scope/rtlink && ln -s /scheme/null /home/user/scope/extlink")
    status, output = run("sclnkrt1", "ferrion-run --files /home/user/scope ferrion-run-probe retarget-link /home/user/scope/rtlink /home/user/scope/extlink")
    if status != 0 or "RETARGET_OK EXT=/scheme/null" not in output or "leads outside /home/user/scope: /home/user/scope/rtlink ->" not in output:
        problems.append(f"retargeted symlink was not reported (exit {status}, {output.strip()[-160:]})")
    status, output = run("schome2", "ferrion-run --files /home/user cat /home/user/outside/secret.txt")
    if SCOPE_MARKER not in output:
        problems.append(f"a session given the whole home could not read a file in it (exit {status})")

    run("sclock0", "touch /home/user/scope/lockf")
    run("sclock1", "ferrion-run --files /home/user/scope bash -c \"ferrion-run-probe lock-hold /home/user/scope/lockf 4 & sleep 1\"")
    run("sclock2", "sleep 5")
    status, output = run("sclock3", "ferrion-run --files /home/user/scope ferrion-run-probe lock-try /home/user/scope/lockf")
    if status != 0 or "LOCK_ACQUIRED" not in output:
        problems.append(f"flock held past session end leaked (exit {status}, {output.strip()[-120:]})")

    since = vm.mark()
    vm.type("exit\n")
    ok, err = login(vm, "root", "password", r"root:.*# ", since, timeout=args.timeout)
    if not ok:
        problems.append("no login prompt after logging out" if err == "no login prompt" else err)
    else:
        _, output = run("scdisk0", "ferrion-run-probe disk")
        if "ROUTE raw-disk REACHED" not in output:
            controls.append("raw-disk as root without ferrion-run")
        else:
            counted.append("raw-disk (root)")
            _, output = run("scdisk1", "ferrion-run ferrion-run-probe disk")
            if "ROUTE raw-disk REFUSED" not in output:
                problems.append("route raw-disk (root) reached a disk under ferrion-run")
            _, output = run("scdisk2", "ferrion-run ferrion-run-probe schemes")
            found = re.search(r"SCHEMES (.*)", output)
            rootns = found.group(1).split() if found else []
            if not found or set(rootns) - SCOPE_SCHEMES:
                problems.append(f"root's session namespace holds {rootns}")
    (vm.out / "agent-scope.txt").write_text("".join(log))
    summary = (f"{len(counted)} routes, {len(counted) - sum(1 for p in problems if p.startswith('route '))} "
               f"refused under ferrion-run; namespace {' '.join(namespace)}")
    if notes:
        summary += "; not on this image: " + ", ".join(notes)
    if controls:
        return False, "control without the launcher failed, so the run proves nothing: " + "; ".join(controls) + f"; see {vm.out}"
    if problems:
        return False, "; ".join(problems) + f"; {summary}; see {vm.out}"
    return True, summary


CLONE_TREE = ("cd /home/user/clone-seed && echo one > a.txt && echo two > b.txt && echo gone > del.txt && "
              "echo move > mv-from.txt && echo echo hi > script.sh && mkdir -p olddir lowerdir/in && "
              "echo old > olddir/x && echo z > lowerdir/in/z && ln -s a.txt link1")
CLONE_EDIT = ("cd {} && echo added > new.txt && echo more >> a.txt && printf replaced > b.txt && rm del.txt && "
              "mv mv-from.txt mv-to.txt && mkdir -p sub/deep && echo deep > sub/deep/f && ln -s new.txt link2 && "
              "chmod +x script.sh && rm -r olddir && mkdir olddir && echo fresh > olddir/fresh && mv lowerdir renameddir && "
              "rm link1 && ln -s b.txt link1 && ln -s /home/user/.ssh to-ssh && ln -s . dot && ln -s dot/../clone-b via")


def test_agent_clone(vm, args):
    """Security roadmap S12 and product roadmap agent item 3, clone mode: as `user`, a scripted edit session (new, appended, replaced, deleted, renamed and chmod-ed files, a new folder tree, a deleted and recreated folder, a renamed folder, symlinks) runs under `ferrion-run --clone DIR`; DIR must be unchanged after it, `ferrion-run diff` must list the changes, and `ferrion-run apply` must leave DIR identical (paths, modes, link targets, contents) to the same script run directly on a copy. A second clone session then `ferrion-run discard` must leave DIR as it was. The end of the session and `diff` must list the two symlinks that lead outside DIR: an absolute one, and `via`, which leaves DIR only through `dot`, another symlink the session added, so it is resolved in the merged view of the layer over DIR. Then clone-stale: a descriptor opened read-only on a file in DIR, whose folder the session then replaces by a symlink to a folder outside, must not get its mode changed, which would copy the file up through that symlink; without the launcher the change succeeds and writes nothing outside."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    log, problems = [], []

    def run(tag, line, timeout=180):
        status, output = guest_run(vm, tag, line, timeout)
        log.append(f"$ {line}\nexit {status}\n{output}\n")
        return status, output

    def tree(tag, path):
        _, output = run(tag, f"ferrion-run-probe tree-hash {path}")
        found = re.search(r"TREE (\S+ \d+)", output)
        return found.group(1) if found else output.strip()[-120:]

    run("clprep", "rm -rf /home/user/clone-seed /home/user/clone-a /home/user/clone-b && mkdir /home/user/clone-seed && "
                  f"{CLONE_TREE} && tar -cf /home/user/clone-seed.tar -C /home/user/clone-seed .")
    reset = ("rm -rf /home/user/clone-{0} && mkdir /home/user/clone-{0} && "
             "tar -xf /home/user/clone-seed.tar -C /home/user/clone-{0}")
    run("clra", reset.format("a"))
    run("clrb", reset.format("b"))
    seed = tree("clh0", "/home/user/clone-a")
    status, _ = run("cldirect", f"bash -c \"{CLONE_EDIT.format('/home/user/clone-b')}\"")
    direct = tree("clhd", "/home/user/clone-b")
    if status != 0 or direct == seed:
        problems.append(f"the edit script did not run directly (exit {status})")
    session_line = f"ferrion-run --clone /home/user/clone-a bash -c \"{CLONE_EDIT.format('/home/user/clone-a')}\""
    status, output = run("clsession", session_line, 300)
    session = re.search(r"session (\d+-\d+) keeps", output)
    if status != 0 or not session:
        problems.append(f"the clone session failed (exit {status})")
    ended = set(re.findall(r"leads outside /home/user/clone-a: /home/user/clone-a/(\S+) ->", output))
    if ended != {"to-ssh", "via"}:
        problems.append(f"the end of the clone session listed {sorted(ended)} as leading outside the folder, want ['to-ssh', 'via']")
    after = tree("clh1", "/home/user/clone-a")
    if after != seed:
        problems.append(f"the clone session wrote into DIR ({seed} became {after})")
    applied = discarded = "not run"
    if session:
        _, diff = run("cldiff", f"ferrion-run diff {session.group(1)}")
        changes = re.findall(r"^[AMD] \S+", diff, re.M)
        if len(changes) < 10:
            problems.append(f"ferrion-run diff listed {len(changes)} changes")
        outside = sorted(re.findall(r"^! (\S+) -> ", diff, re.M))
        if outside != ["to-ssh", "via"]:
            problems.append(f"ferrion-run diff listed {outside} as leading outside the folder, want ['to-ssh', 'via']")
        status, _ = run("clapply", f"ferrion-run apply {session.group(1)}")
        applied = tree("clh2", "/home/user/clone-a")
        if status != 0 or applied != direct:
            problems.append(f"after apply DIR is {applied}, the direct run gave {direct} (exit {status})")
        run("clra2", reset.format("a"))
        status, output = run("clsession2", session_line, 300)
        second = re.search(r"session (\d+-\d+) keeps", output)
        if second:
            status, _ = run("cldiscard", f"ferrion-run discard {second.group(1)}")
            discarded = tree("clh3", "/home/user/clone-a")
            if status != 0 or discarded != seed:
                problems.append(f"after discard DIR is {discarded}, not {seed} (exit {status})")
            _, output = run("clgone", f"ferrion-run diff {second.group(1)}")
            if "no session" not in output:
                problems.append("a discarded session still had a layer")
        else:
            problems.append("the second clone session failed")
    run("clesc0", "rm -rf /home/user/cl-esc /home/user/cl-esc2 /home/user/clone-outside && mkdir -p "
                  "/home/user/cl-esc/clone-d /home/user/cl-esc2/clone-d /home/user/clone-outside && "
                  "echo planted > /home/user/cl-esc/clone-d/clone-planted && echo planted > /home/user/cl-esc2/clone-d/clone-planted")
    _, free = run("clesc1", "ferrion-run-probe clone-escape /home/user/cl-esc2 /home/user/clone-outside")
    _, jailed = run("clesc2", "ferrion-run --clone /home/user/cl-esc ferrion-run-probe clone-escape /home/user/cl-esc "
                              "/home/user/clone-outside")
    _, left = run("clesc3", "ls -A /home/user/clone-outside | wc -l")
    escaped = re.search(r"session (\d+-\d+) keeps", jailed)
    if escaped:
        run("clesc4", f"ferrion-run discard {escaped.group(1)}")
    stale = "REFUSED" if "ROUTE clone-stale REFUSED" in jailed else "REACHED" if "ROUTE clone-stale REACHED" in jailed else "not run"
    if "ROUTE clone-stale REACHED" not in free:
        problems.append("control: clone-escape could not change its descriptor's mode without the launcher")
    if stale != "REFUSED" or left.split()[-1:] != ["0"]:
        problems.append(f"route clone-stale {stale} under ferrion-run --clone, {left.split()[-1:]} files written outside the folder")

    run("clrn0", "rm -rf /home/user/cl-rn-a /home/user/cl-rn-b && mkdir -p /home/user/cl-rn-a/d /home/user/cl-rn-b/d && echo keep > /home/user/cl-rn-a/f && echo keep > /home/user/cl-rn-b/f")
    _, rn_direct = run("clrn1", "ferrion-run-probe rename-onto-dir /home/user/cl-rn-b/f /home/user/cl-rn-b/d")
    _, rn_clone = run("clrn2", "ferrion-run --clone /home/user/cl-rn-a ferrion-run-probe rename-onto-dir /home/user/cl-rn-a/f /home/user/cl-rn-a/d")
    rn_sess = re.search(r"session (\d+-\d+) keeps", rn_clone)
    if rn_sess:
        run("clrn3", f"ferrion-run apply {rn_sess.group(1)}")
    _, rn_dir = run("clrn4", "test -d /home/user/cl-rn-a/d && test -f /home/user/cl-rn-a/f && echo RN_INTACT")
    rn_direct_line = re.search(r"RENAME (?:OK|ERR \d+)", rn_direct)
    rn_clone_line = re.search(r"RENAME (?:OK|ERR \d+)", rn_clone)
    if not rn_direct_line or not rn_clone_line or rn_direct_line.group(0) != rn_clone_line.group(0) or "RN_INTACT" not in rn_dir:
        problems.append(f"rename file onto lower dir: direct={rn_direct_line.group(0) if rn_direct_line else None} clone={rn_clone_line.group(0) if rn_clone_line else None} intact={'RN_INTACT' in rn_dir}")

    run("clop0", "rm -rf /home/user/cl-op && mkdir -p /home/user/cl-op/d && echo lower > /home/user/cl-op/d/x")
    status, op_out = run("clop1", "ferrion-run --clone /home/user/cl-op bash -c \"rm /home/user/cl-op/d/x && rmdir /home/user/cl-op/d && mkdir /home/user/cl-op/e && mv /home/user/cl-op/e /home/user/cl-op/d && printf OP_COUNT= && ls -A /home/user/cl-op/d | wc -l\"")
    op_sess = re.search(r"session (\d+-\d+) keeps", op_out)
    if op_sess:
        run("clop2", f"ferrion-run apply {op_sess.group(1)}")
    _, op_after = run("clop3", "test -d /home/user/cl-op/d && test ! -e /home/user/cl-op/d/x && echo OP_CLEAN")
    if status != 0 or "OP_COUNT=0" not in op_out or "OP_CLEAN" not in op_after:
        problems.append(f"rename dir onto whited-out dir lost opaque (exit={status}, op_out={op_out.strip()[-120:]}, clean={'OP_CLEAN' in op_after})")

    run("cllk0", "rm -rf /home/user/cl-lk && mkdir -p /home/user/cl-lk && echo one > /home/user/cl-lk/f")
    status, lk_out = run("cllk1", "ferrion-run --clone /home/user/cl-lk ferrion-run-probe lock-fchmod /home/user/cl-lk/f")
    lk_sess = re.search(r"session (\d+-\d+) keeps", lk_out)
    if lk_sess:
        run("cllk2", f"ferrion-run discard {lk_sess.group(1)}")
    status2, lk_try = run("cllk3", "ferrion-run --clone /home/user/cl-lk ferrion-run-probe lock-try /home/user/cl-lk/f")
    lk_sess2 = re.search(r"session (\d+-\d+) keeps", lk_try)
    if lk_sess2:
        run("cllk4", f"ferrion-run discard {lk_sess2.group(1)}")
    if status != 0 or "LOCK_FCHMOD OK" not in lk_out or status2 != 0 or "LOCK_ACQUIRED" not in lk_try:
        problems.append(f"clone lock+fchmod leaked lock (step1={status}, step2={status2}, {lk_try.strip()[-120:]})")

    run("clen0", "rm -rf /home/user/cl-enospc /home/user/.ballast /home/user/.reserve && mkdir -p /home/user/cl-enospc && ferrion-run-probe fill /home/user/cl-enospc/big.bin 2048")
    _, big_before = run("clen1", "sha256sum /home/user/cl-enospc/big.bin")
    big_before_hash = re.search(r"([0-9a-f]{64})", big_before)
    enospc_sess = None
    en_status = None
    try:
        run("clen2", "ferrion-run-probe fill-disk /home/user/.ballast /home/user/.reserve; df -k /home/user | tail -n 1", 180)
        en_status, en_out = run("clen4", "ferrion-run --clone /home/user/cl-enospc bash -c \"echo x >> /home/user/cl-enospc/big.bin\"", 120)
        enospc_sess = re.search(r"session (\d+-\d+) keeps", en_out)
    finally:
        run("clen5", "rm -f /home/user/.ballast /home/user/.reserve")
    if en_status == 0:
        problems.append("clone copy-up with full disk unexpectedly succeeded")
    if enospc_sess:
        _, en_diff = run("clen6", f"ferrion-run diff {enospc_sess.group(1)}")
        if "big.bin" in en_diff:
            problems.append(f"failed copy-up listed in diff: {en_diff.strip()[-160:]}")
        run("clen7", f"ferrion-run apply {enospc_sess.group(1)}")
    _, big_after = run("clen8", "sha256sum /home/user/cl-enospc/big.bin")
    big_after_hash = re.search(r"([0-9a-f]{64})", big_after)
    if not big_before_hash or not big_after_hash or big_before_hash.group(1) != big_after_hash.group(1):
        problems.append(f"failed copy-up corrupted lower file ({big_before_hash.group(1) if big_before_hash else None} -> {big_after_hash.group(1) if big_after_hash else None})")

    (vm.out / "agent-clone.txt").write_text("".join(log))
    summary = (f"seed {seed}; direct {direct}; after the session {after}; after apply {applied}; after discard {discarded}; "
               f"clone-stale {stale}")
    if problems:
        return False, "; ".join(problems) + f"; {summary}; see {vm.out}"
    return True, summary


SCOPE_WORK = ("rm -rf {0}/w && mkdir {0}/w && cd {0}/w && tar -xf /home/user/bench/src.tar && git init -q && "
              "git add -A && git status --short | wc -l")


OVERHEAD_LOAD_LIMIT = 12.0


def test_agent_scope_overhead(vm, args):
    """S12 overhead: as `user`, a file-heavy workload (unpack a tarball of 100 files of about 8 KB in 10 folders, written by bash builtins so that making them starts no process, then git init, git add -A and git status) runs directly and under `ferrion-run --files`, alternating, three times each; passes when the median under the proxy is within 20% of the median without it. Reports both and the host's load before and after each run, and marks the result host-limited when a load is above 12."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    log = []

    def run(tag, line, timeout=900):
        status, output = guest_run(vm, tag, line, timeout)
        log.append(f"$ {line}\nexit {status}\n{output}\n")
        return status, output

    status, output = run("ovprep", "rm -rf /home/user/bench /home/user/benchsrc /home/user/wa /home/user/wb && "
                                   "mkdir -p /home/user/bench /home/user/wa /home/user/wb /home/user/benchsrc/include && "
                                   "cd /home/user/benchsrc/include && mkdir d{1..10} && for d in {1..10}; do "
                                   "for f in {1..10}; do printf \"$d $f %s\\n\" {1..1000} > d$d/f$f.h; done; done && "
                                   "tar -cf /home/user/bench/src.tar -C /home/user/benchsrc include && "
                                   "find /home/user/benchsrc -type f | wc -l", 600)
    if status != 0 or not re.search(r"^\s*100\s*$", output, re.M):
        (vm.out / "agent-scope-overhead.txt").write_text("".join(log))
        return False, f"the workload's tree was not made (exit {status}); see {vm.out}"
    times = {"direct": [], "proxy": []}
    counts = set()
    loads = []
    for round_ in range(3):
        for kind, line in (("direct", f"bash -c \"{SCOPE_WORK.format('/home/user/wa')}\""),
                           ("proxy", "ferrion-run --files /home/user/wb --read-only /home/user/bench "
                                     f"bash -c \"{SCOPE_WORK.format('/home/user/wb')}\"")):
            before = os.getloadavg()[0]
            status, output = run(f"ov{kind}{round_}", f"s=$EPOCHREALTIME; {line}; e=$EPOCHREALTIME; echo T $s $e")
            loads.append((before, os.getloadavg()[0]))
            log.append(f"host load1 {loads[-1][0]:.2f} before, {loads[-1][1]:.2f} after\n")
            found = re.search(r"(\d+)\s+T (\d+[.,]\d+) (\d+[.,]\d+)", output)
            if status != 0 or not found:
                (vm.out / "agent-scope-overhead.txt").write_text("".join(log))
                return False, f"{kind} run {round_ + 1} failed (exit {status}); see {vm.out}"
            counts.add(found.group(1))
            start, end = (float(found.group(i).replace(",", ".")) for i in (2, 3))
            times[kind].append(end - start)
    (vm.out / "agent-scope-overhead.txt").write_text("".join(log))
    direct, proxy = sorted(times["direct"])[1], sorted(times["proxy"])[1]
    ratio = proxy / direct
    summary = (f"direct {', '.join(f'{t:.2f}' for t in times['direct'])} s, under ferrion-run "
               f"{', '.join(f'{t:.2f}' for t in times['proxy'])} s; medians {direct:.2f} s and {proxy:.2f} s, "
               f"{(ratio - 1) * 100:+.1f}%; git status lines {sorted(counts)}; host load1 before and after each run "
               f"{', '.join(f'{a:.1f}/{b:.1f}' for a, b in loads)}")
    if max(max(pair) for pair in loads) > OVERHEAD_LOAD_LIMIT:
        summary += f"; host-limited: load1 above {OVERHEAD_LOAD_LIMIT:g}"
    if len(counts) != 1:
        return False, f"the runs saw different trees; {summary}"
    return ratio <= 1.2, summary


def test_hardened_malloc(vm, args):
    """Logs in as `user` on the serial console, measures sparse PROT_NONE reservation cost (32 MiB to 32 GiB), probes PROT_NONE/mremap/madvise/getrandom/mprotect, runs hardened_malloc's 58-binary upstream test suite on 32 MiB default and light builds, times upstream 32 GiB x 4 arenas launch/exit, and benchmarks dlmalloc vs hardened_malloc default vs light."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    vm.type("test -x /usr/bin/hardened-malloc-probe && test -s /usr/share/licenses/hardened-malloc/LICENSE && echo HM_READY || echo HM_MISSING\n")
    ready = vm.wait_serial(r"\r?\n(HM_READY|HM_MISSING)\r?\n", 20, since)
    if not ready or ready.group(1) != "HM_READY":
        return False, f"hardened-malloc or hardened-malloc-probe missing on image; see {vm.out / 'serial.log'}"
    sections = []
    problems = []
    metrics = {"loadavg_start": list(os.getloadavg())}
    steps = (
        ("license", "test -s /usr/share/licenses/hardened-malloc/LICENSE && head -n 1 /usr/share/licenses/hardened-malloc/LICENSE", 30),
        ("sparse", "hardened-malloc-probe sparse", 240),
        ("prot-none", "hardened-malloc-probe prot-none", 60),
        ("syscalls", "hardened-malloc-probe syscalls", 60),
        ("suite-default", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/small hardened-malloc-probe run-suite /usr/libexec/hardened-malloc/test", 360),
        ("suite-light", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/light-small hardened-malloc-probe run-suite /usr/libexec/hardened-malloc/test", 360),
        ("nop-dlmalloc", "hardened-malloc-probe timed-exec 15000 hardened-malloc-bench-dlmalloc dlmalloc --nop", 45),
        ("nop-default-small", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/small hardened-malloc-probe timed-exec 15000 hardened-malloc-bench-hm hm-default-small --nop", 45),
        ("nop-light-small", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/light-small hardened-malloc-probe timed-exec 15000 hardened-malloc-bench-hm hm-light-small --nop", 45),
        ("nop-default-32g", "LD_LIBRARY_PATH=/usr/lib hardened-malloc-probe timed-exec 20000 hardened-malloc-bench-hm hm-default-32g --nop", 60),
        ("bench-dlmalloc", "hardened-malloc-bench-dlmalloc dlmalloc 2>/dev/null", 180),
        ("bench-default", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/small hardened-malloc-bench-hm hm-default 2>/dev/null", 180),
        ("bench-light", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/light-small hardened-malloc-bench-hm hm-light 2>/dev/null", 180),
    )
    outputs = {}
    for tag, cmd, step_timeout in steps:
        status, out = guest_run(vm, f"hm{tag.replace('-', '')}", cmd, step_timeout)
        outputs[tag] = (status, out)
        sections.append(f"=== {tag} (exit {status}) ===\n{out}\n")
        if status != 0:
            problems.append(f"{tag} exit {status}")
    metrics["loadavg_end"] = list(os.getloadavg())
    (vm.out / "hardened-malloc.txt").write_text("".join(sections))
    sparse_rows = re.findall(r"sparse size=(\d+) status=(\S+)[^\r\n]*", outputs["sparse"][1])
    if len(sparse_rows) != 5 or any(st != "OK" for _, st in sparse_rows):
        problems.append(f"sparse rows={len(sparse_rows)}")
    def_sum = re.search(r"suite-summary passed=(\d+) failed=(\d+) total=(\d+)", outputs["suite-default"][1])
    light_sum = re.search(r"suite-summary passed=(\d+) failed=(\d+) total=(\d+)", outputs["suite-light"][1])
    if not def_sum or int(def_sum.group(3)) != 58 or int(def_sum.group(1)) < 29:
        problems.append("default suite below 29/58 strict baseline")
    if not light_sum or int(light_sum.group(3)) != 58 or int(light_sum.group(1)) < 29:
        problems.append("light suite below 29/58 strict baseline")
    abort_detected = len(re.findall(r"suite \S+ match=(?:PASS|FAIL) status=-(?:4|6) expected=-6 [^\r\n]*fatal allocator error:", outputs["suite-default"][1]))
    if abort_detected != 27:
        problems.append(f"default abort detections {abort_detected}/27")
    for btag, blabel in (("bench-dlmalloc", "dlmalloc"), ("bench-default", "hm-default"), ("bench-light", "hm-light")):
        if f"bench label={blabel} " not in outputs[btag][1]:
            problems.append(f"missing {btag}")
    if "timed-exec cmd=" not in outputs["nop-default-32g"][1]:
        problems.append("missing nop-default-32g")
    metrics["suite_default"] = dict(zip(("passed", "failed", "total"), map(int, def_sum.groups()))) if def_sum else None
    metrics["suite_default_abort_detected"] = abort_detected
    metrics["suite_light"] = dict(zip(("passed", "failed", "total"), map(int, light_sum.groups()))) if light_sum else None
    (vm.out / "hardened-malloc.json").write_text(json.dumps(metrics, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'hardened-malloc.txt'}"
    return True, (
        f"default suite {def_sum.group(1)}/{def_sum.group(3)} strict + {abort_detected}/27 abort-detected, "
        f"light suite {light_sum.group(1)}/{light_sum.group(3)}, "
        f"sparse 32MiB..32GiB + syscalls + benches measured; see {vm.out / 'hardened-malloc.txt'}"
    )


def test_hardened_malloc_hotpath(vm, args):
    """Checks pthread_mutex concurrency, private futex wake key matching, zero uncontended futex_wake syscalls, and uncontended mutex latency threshold (<= 150 ns/pair)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    vm.type("test -x /usr/bin/hardened-malloc-probe && echo HMH_READY || echo HMH_MISSING\n")
    ready = vm.wait_serial(r"\r?\n(HMH_READY|HMH_MISSING)\r?\n", 20, since)
    if not ready or ready.group(1) != "HMH_READY":
        return False, f"hardened-malloc-probe missing on image; see {vm.out / 'serial.log'}"
    sections = []
    problems = []
    metrics = {"loadavg_start": list(os.getloadavg()), "mutex_bench": [], "bench_default": [], "bench_light": []}
    st_stress, out_stress = guest_run(vm, "hmhstress", "hardened-malloc-probe mutex-stress", 60)
    sections.append(f"=== mutex-stress (exit {st_stress}) ===\n{out_stress}\n")
    if st_stress != 0 or "ok=1" not in out_stress:
        problems.append(f"mutex-stress failed (exit {st_stress})")
    fc_enabled_list = []
    static_ns_list = []
    init_ns_list = []
    wake_calls_list = []
    futex_wake_ns_list = []
    for run_idx in range(1, 6):
        st_b, out_b = guest_run(vm, f"hmhmb{run_idx}", "hardened-malloc-probe mutex-bench", 60)
        sections.append(f"=== mutex-bench {run_idx} (exit {st_b}) ===\n{out_b}\n")
        m = re.search(
            r"mutex-bench fc_enabled=(\d+) static_ns=(\d+) init_ns=(\d+) wake_calls_10k=(\d+) total_sc_10k=(\d+) futex_wake_ns=(\d+)",
            out_b,
        )
        if st_b != 0 or not m:
            problems.append(f"mutex-bench run {run_idx} missing output")
            continue
        row = {
            "fc_enabled": int(m.group(1)),
            "static_ns": int(m.group(2)),
            "init_ns": int(m.group(3)),
            "wake_calls_10k": int(m.group(4)),
            "total_sc_10k": int(m.group(5)),
            "futex_wake_ns": int(m.group(6)),
        }
        metrics["mutex_bench"].append(row)
        fc_enabled_list.append(row["fc_enabled"])
        static_ns_list.append(row["static_ns"])
        init_ns_list.append(row["init_ns"])
        wake_calls_list.append(row["wake_calls_10k"])
        futex_wake_ns_list.append(row["futex_wake_ns"])
    for key, cmd in (
        ("bench_default", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/small hardened-malloc-bench-hm hm-default 2>/dev/null"),
        ("bench_light", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/light-small hardened-malloc-bench-hm hm-light 2>/dev/null"),
    ):
        for run_idx in range(1, 6):
            st_r, out_r = guest_run(vm, f"hmh{key[-3:]}{run_idx}", cmd, 180)
            sections.append(f"=== {key} {run_idx} (exit {st_r}) ===\n{out_r}\n")
            bm = re.search(r"bench label=\S+ small_ms=(\d+) med_ms=(\d+) large_ms=(\d+) realloc_ms=(\d+)", out_r)
            if st_r != 0 or not bm:
                problems.append(f"{key} run {run_idx} failed")
                continue
            metrics[key].append({
                "small_ms": int(bm.group(1)),
                "med_ms": int(bm.group(2)),
                "large_ms": int(bm.group(3)),
                "realloc_ms": int(bm.group(4)),
            })
    metrics["loadavg_end"] = list(os.getloadavg())
    (vm.out / "hardened-malloc-hotpath.txt").write_text("".join(sections))
    if static_ns_list:
        med_static = sorted(static_ns_list)[len(static_ns_list) // 2]
        med_init = sorted(init_ns_list)[len(init_ns_list) // 2]
        min_wakes = min(wake_calls_list)
        max_wakes = max(wake_calls_list)
        med_fw = sorted(futex_wake_ns_list)[len(futex_wake_ns_list) // 2]
        all_fc_enabled = all(v == 1 for v in fc_enabled_list)
        metrics["fc_enabled_all"] = all_fc_enabled
        metrics["median_static_ns"] = med_static
        metrics["median_init_ns"] = med_init
        metrics["min_wake_calls_10k"] = min_wakes
        metrics["max_wake_calls_10k"] = max_wakes
        metrics["median_futex_wake_ns"] = med_fw
        if not all_fc_enabled:
            problems.append(f"foreign-call syscall counter not enabled ({fc_enabled_list})")
        if min_wakes != 1 or max_wakes != 1:
            problems.append(f"counted futex_wake calls {min_wakes}..{max_wakes} != 1 (1 control + 0 unlock)")
        if med_static > 150 or med_init > 150:
            problems.append(f"mutex latency static={med_static}ns init={med_init}ns > 150ns")
    else:
        med_static = med_init = med_fw = -1
        max_wakes = -1
    (vm.out / "hardened-malloc-hotpath.json").write_text(json.dumps(metrics, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'hardened-malloc-hotpath.txt'}"
    return True, (
        f"mutex-stress ok, uncontended static={med_static}ns init={med_init}ns (<=150ns), "
        f"fc_enabled=1 wake_calls_10k={max_wakes} (1 control + 0 unlock), futex_wake={med_fw}ns; "
        f"see {vm.out / 'hardened-malloc-hotpath.txt'}"
    )


def test_hardened_malloc_sparse(vm, args):
    """Checks sparse page-table walks, page-table reclaim, PROT_NONE zero eager mapping, resident /scheme/sys/context reporting, fork/CoW/mprotect on 12.25 TiB reservations, and 5-run 32 GiB munmap (<1 ms) + default 12.25 TiB hardened_malloc start-to-exit (<=2x dlmalloc)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    vm.type("test -x /usr/bin/hardened-malloc-probe && echo HM_READY || echo HM_MISSING\n")
    ready = vm.wait_serial(r"\r?\n(HM_READY|HM_MISSING)\r?\n", 20, since)
    if not ready or ready.group(1) != "HM_READY":
        return False, f"hardened-malloc-probe missing on image; see {vm.out / 'serial.log'}"
    sections = []
    problems = []
    load_start = list(os.getloadavg())
    st_pn, out_pn = guest_run(vm, "hmspn", "hardened-malloc-probe prot-none", 60)
    sections.append(f"=== prot-none (exit {st_pn}) ===\n{out_pn}\n")
    if st_pn != 0:
        problems.append(f"prot-none exit {st_pn}")
    for case in ("mmap-none-read", "mmap-none-write", "mprotect-none-read", "mprotect-none-write"):
        if f"prot-none case={case} status=-11 timed_out=0" not in out_pn:
            problems.append(f"prot-none {case} failed")
    st_ops, out_ops = guest_run(vm, "hmsops", "hardened-malloc-probe sparse-ops", 180)
    sections.append(f"=== sparse-ops (exit {st_ops}) ===\n{out_ops}\n")
    if st_ops != 0:
        problems.append(f"sparse-ops exit {st_ops}")
    m_full = re.search(
        r"table-reclaim-full mmap_frames=(-?\d+) touched_frames=(-?\d+) unmap_rc=(-?\d+) unmap_us=(\d+) net_frames=(-?\d+)",
        out_ops,
    )
    if not m_full:
        problems.append("missing table-reclaim-full")
    else:
        mf, tf, urc, uus, nf = map(int, m_full.groups())
        if mf != 0 or tf < 632 or urc != 0 or nf != 0:
            problems.append(f"table-reclaim-full mmap_frames={mf} touched={tf} rc={urc} net={nf}")
    m_part = re.search(
        r"table-reclaim-partial mid_rc=(-?\d+) left_ok=(\d+) right_ok=(\d+) u_left=(-?\d+) u_right=(-?\d+) net_frames=(-?\d+)",
        out_ops,
    )
    if not m_part:
        problems.append("missing table-reclaim-partial")
    else:
        mrc, lok, rok, ul, ur, pnf = map(int, m_part.groups())
        if mrc != 0 or lok != 1 or rok != 1 or ul != 0 or ur != 0 or pnf != 0:
            problems.append(f"table-reclaim-partial mid={mrc} left={lok} right={rok} u={ul}/{ur} net={pnf}")
    m_ro = re.search(
        r"mprotect-ro-sparse mp_rc=(-?\d+) child_status=(-?\d+) timed_out=(\d+) parent_ok=(\d+) unmap_rc=(-?\d+)",
        out_ops,
    )
    if not m_ro:
        problems.append("missing mprotect-ro-sparse")
    else:
        rmp, rst, rto, pok, rur = map(int, m_ro.groups())
        if rmp != 0 or rst != -11 or rto != 0 or pok != 1 or rur != 0:
            problems.append(f"mprotect-ro-sparse mp={rmp} st={rst} to={rto} pok={pok} ur={rur}")
    m_bfk = re.search(r"fork-baseline status=(-?\d+) timed_out=(\d+) fork_us=(\d+)", out_ops)
    fork_baseline_us = None
    if not m_bfk:
        problems.append("missing fork-baseline")
    else:
        bfk_st, bfk_to, bfk_us = map(int, m_bfk.groups())
        fork_baseline_us = bfk_us
        if bfk_st != 0 or bfk_to != 0:
            problems.append(f"fork-baseline st={bfk_st} to={bfk_to}")
    m_12t = re.search(
        r"sparse-12t mmap_us=(\d+) mmap_frames=(-?\d+) memstat_rc=(-?\d+) memstat_bytes=(\d+) memstat_us=(\d+) "
        r"context_rc=(-?\d+) priv_found=(\d+) priv_kib=(\d+) context_us=(\d+) mp_slab_rc=(-?\d+) mp_slab_us=(\d+) "
        r"mp_32g_rc=(-?\d+) mp_32g_us=(\d+) fork_status=(-?\d+) cow_ok=(\d+) fork_us=(\d+) unmap_rc=(-?\d+) unmap_us=(\d+)",
        out_ops,
    )
    sparse_12t_metrics = None
    if not m_12t:
        problems.append("missing sparse-12t")
    else:
        vals = list(map(int, m_12t.groups()))
        keys = (
            "mmap_us", "mmap_frames", "memstat_rc", "memstat_bytes", "memstat_us",
            "context_rc", "priv_found", "priv_kib", "context_us", "mp_slab_rc", "mp_slab_us",
            "mp_32g_rc", "mp_32g_us", "fork_status", "cow_ok", "fork_us", "unmap_rc", "unmap_us",
        )
        sparse_12t_metrics = dict(zip(keys, vals))
        if sparse_12t_metrics["mmap_frames"] != 0:
            problems.append(f"sparse-12t mmap_frames={sparse_12t_metrics['mmap_frames']}")
        if sparse_12t_metrics["memstat_rc"] != 0 or sparse_12t_metrics["memstat_us"] >= 500000:
            problems.append(f"sparse-12t memstat rc={sparse_12t_metrics['memstat_rc']} us={sparse_12t_metrics['memstat_us']}")
        if (
            sparse_12t_metrics["context_rc"] != 0
            or sparse_12t_metrics["priv_found"] != 1
            or sparse_12t_metrics["priv_kib"] >= 65536
            or sparse_12t_metrics["context_us"] >= 500000
        ):
            problems.append(
                f"sparse-12t context rc={sparse_12t_metrics['context_rc']} found={sparse_12t_metrics['priv_found']} "
                f"priv_kib={sparse_12t_metrics['priv_kib']} us={sparse_12t_metrics['context_us']}"
            )
        if sparse_12t_metrics["mp_slab_rc"] != 0 or sparse_12t_metrics["mp_32g_rc"] != 0 or sparse_12t_metrics["mp_32g_us"] >= 5000:
            problems.append(f"sparse-12t mprotect 32g_us={sparse_12t_metrics['mp_32g_us']}")
        if sparse_12t_metrics["fork_status"] != 0 or sparse_12t_metrics["cow_ok"] != 1 or sparse_12t_metrics["fork_us"] >= 200000:
            problems.append(
                f"sparse-12t fork st={sparse_12t_metrics['fork_status']} cow={sparse_12t_metrics['cow_ok']} us={sparse_12t_metrics['fork_us']}"
            )
        if sparse_12t_metrics["unmap_rc"] != 0 or sparse_12t_metrics["unmap_us"] >= 1000:
            problems.append(f"sparse-12t unmap rc={sparse_12t_metrics['unmap_rc']} us={sparse_12t_metrics['unmap_us']}")

    timed_runs = []
    for idx in range(1, 6):
        l_before = os.getloadavg()[0]
        st_sp, out_sp = guest_run(vm, f"hmsp{idx}", "hardened-malloc-probe sparse", 240)
        st_dl, out_dl = guest_run(
            vm,
            f"hmdl{idx}",
            "hardened-malloc-probe timed-exec 15000 hardened-malloc-bench-dlmalloc dlmalloc --nop",
            45,
        )
        st_hm, out_hm = guest_run(
            vm,
            f"hmhm{idx}",
            "LD_LIBRARY_PATH=/usr/lib hardened-malloc-probe timed-exec 60000 hardened-malloc-bench-hm hm-default-32g --nop",
            90,
        )
        l_after = os.getloadavg()[0]
        sections.append(f"=== run {idx} sparse (exit {st_sp}) ===\n{out_sp}\n")
        sections.append(f"=== run {idx} nop-dlmalloc (exit {st_dl}) ===\n{out_dl}\n")
        sections.append(f"=== run {idx} nop-hm-32g (exit {st_hm}) ===\n{out_hm}\n")
        m_32g = re.search(
            r"sparse size=34359738368 status=(\S+) mmap_us=(\d+) mmap_frames=(-?\d+) .*? unmap_rc=(-?\d+) unmap_us=(\d+) unmap_frames=(-?\d+) net_frames=(-?\d+)",
            out_sp,
        )
        m_dl = re.search(r"timed-exec cmd=\S+ status=(-?\d+) timed_out=(\d+) ms=(\d+)", out_dl)
        m_hm = re.search(r"timed-exec cmd=\S+ status=(-?\d+) timed_out=(\d+) ms=(\d+)", out_hm)
        run_rec = {
            "run": idx,
            "load_before": round(l_before, 2),
            "load_after": round(l_after, 2),
            "sparse_32g_unmap_us": int(m_32g.group(5)) if m_32g else None,
            "sparse_32g_mmap_frames": int(m_32g.group(3)) if m_32g else None,
            "sparse_32g_net_frames": int(m_32g.group(7)) if m_32g else None,
            "dlmalloc_nop_ms": int(m_dl.group(3)) if m_dl else None,
            "hm_32g_nop_ms": int(m_hm.group(3)) if m_hm else None,
        }
        timed_runs.append(run_rec)
        if not m_32g or int(m_32g.group(3)) != 0 or int(m_32g.group(4)) != 0 or int(m_32g.group(7)) != 0:
            problems.append(f"run {idx} sparse 32g invariants failed")
        if not m_dl or int(m_dl.group(1)) != 0 or int(m_dl.group(2)) != 0:
            problems.append(f"run {idx} dlmalloc --nop failed")
        if not m_hm or int(m_hm.group(1)) != 0 or int(m_hm.group(2)) != 0:
            problems.append(f"run {idx} hm-default-32g --nop failed")

    unmap_vals = sorted(r["sparse_32g_unmap_us"] for r in timed_runs if r["sparse_32g_unmap_us"] is not None)
    dl_vals = sorted(r["dlmalloc_nop_ms"] for r in timed_runs if r["dlmalloc_nop_ms"] is not None)
    hm_vals = sorted(r["hm_32g_nop_ms"] for r in timed_runs if r["hm_32g_nop_ms"] is not None)
    med_unmap = unmap_vals[len(unmap_vals) // 2] if unmap_vals else None
    med_dl = dl_vals[len(dl_vals) // 2] if dl_vals else None
    med_hm = hm_vals[len(hm_vals) // 2] if hm_vals else None
    if med_unmap is None or med_unmap >= 1000:
        problems.append(f"median 32g unmap {med_unmap}us >= 1000us")
    if med_dl is None or med_hm is None or med_hm > 2 * med_dl:
        problems.append(f"median hm-32g {med_hm}ms > 2x dlmalloc {med_dl}ms")

    metrics = {
        "loadavg_start": load_start,
        "loadavg_end": list(os.getloadavg()),
        "fork_baseline_us": fork_baseline_us,
        "sparse_12t": sparse_12t_metrics,
        "runs": timed_runs,
        "medians": {
            "sparse_32g_unmap_us": med_unmap,
            "dlmalloc_nop_ms": med_dl,
            "hm_32g_nop_ms": med_hm,
        },
    }
    (vm.out / "hardened-malloc-sparse.txt").write_text("".join(sections))
    (vm.out / "hardened-malloc-sparse.json").write_text(json.dumps(metrics, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'hardened-malloc-sparse.txt'}"
    return True, (
        f"32g_unmap_med={med_unmap}us, dlmalloc_med={med_dl}ms, hm_32g_med={med_hm}ms, "
        f"12t_fork={sparse_12t_metrics['fork_us']}us, priv={sparse_12t_metrics['priv_kib']}KiB; "
        f"see {vm.out / 'hardened-malloc-sparse.txt'}"
    )


def test_hardened_malloc_pt_order(vm, args):
    """Runs hardened-malloc-probe discard first, then checks that emptied user page-table frames are freed only after the TLB shootdown completes (0 early-table-free log entries since boot) and runs the multi-threaded remap/unmap smoke test."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    sections = []
    problems = []
    st_disc, out_disc = guest_run(vm, "hmptd", "hardened-malloc-probe discard", 90)
    sections.append(f"=== discard (exit {st_disc}) ===\n{out_disc}\n")
    if st_disc != 0:
        problems.append(f"discard exit {st_disc}")
    st_pt, out_pt = guest_run(vm, "hmpto", "hardened-malloc-probe pt-order", 120)
    sections.append(f"=== pt-order (exit {st_pt}) ===\n{out_pt}\n")
    if st_pt != 0:
        problems.append(f"pt-order exit {st_pt}")
    m_pt = re.search(
        r"pt-order log_rc_before=(-?\d+) log_rc_after=(-?\d+) bytes_before=(\d+) bytes_after=(\d+) "
        r"unmap_rc=(-?\d+) unmap_us=(\d+) before=(\d+) after=(\d+) delta=(\d+)",
        out_pt,
    )
    pt_metrics = None
    if not m_pt:
        problems.append("missing pt-order output")
    else:
        l0, l1, b0, b1, urc, uus, bef, aft, delta = map(int, m_pt.groups())
        pt_metrics = {
            "log_rc_before": l0,
            "log_rc_after": l1,
            "bytes_before": b0,
            "bytes_after": b1,
            "unmap_rc": urc,
            "unmap_us": uus,
            "before": bef,
            "after": aft,
            "delta": delta,
        }
        if l0 != 0 or l1 != 0 or b0 == 0 or b1 == 0 or urc != 0 or bef != 0 or aft != 0 or delta != 0:
            problems.append(
                f"pt-order log={l0}/{l1} bytes={b0}/{b1} unmap_rc={urc} before={bef} after={aft} delta={delta}"
            )
    st_mt, out_mt = guest_run(vm, "hmmts", "hardened-malloc-probe mt-smoke", 60)
    sections.append(f"=== mt-smoke (exit {st_mt}) ===\n{out_mt}\n")
    if st_mt != 0:
        problems.append(f"mt-smoke exit {st_mt}")
    m_mt = re.search(
        r"mt-smoke ok=(\d+) workers=(\d+) churn_iters=(\d+) total_touches=(\d+) unmap_rc=(-?\d+)",
        out_mt,
    )
    mt_metrics = None
    if not m_mt:
        problems.append("missing mt-smoke output")
    else:
        ok, wk, ch, tch, urc = map(int, m_mt.groups())
        mt_metrics = {"ok": ok, "workers": wk, "churn_iters": ch, "total_touches": tch, "unmap_rc": urc}
        if ok != 1 or wk != 4 or ch == 0 or tch == 0 or urc != 0:
            problems.append(f"mt-smoke ok={ok} ch={ch} tch={tch} urc={urc}")
    (vm.out / "hardened-malloc-pt-order.txt").write_text("".join(sections))
    (vm.out / "hardened-malloc-pt-order.json").write_text(
        json.dumps({"discard_exit": st_disc, "pt_order": pt_metrics, "mt_smoke": mt_metrics}, indent=2) + "\n"
    )
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'hardened-malloc-pt-order.txt'}"
    return True, (
        f"discard ok, early-table-free before={pt_metrics['before']} after={pt_metrics['after']} delta={pt_metrics['delta']}, "
        f"unmap={pt_metrics['unmap_us']}us, mt-smoke churn={mt_metrics['churn_iters']} touches={mt_metrics['total_touches']}; "
        f"see {vm.out / 'hardened-malloc-pt-order.txt'}"
    )


def test_hardened_malloc_discard(vm, args):
    """Logs in as `user` on the serial console, verifies in-place anonymous page discard via madvise (MADV_DONTNEED, MADV_DONTNEED_LOCKED, MADV_FREE), posix_madvise non-destruction, MAP_SHARED refusal, POSIX abort() SIGABRT with empty stderr, 58/58 strict test_smc.py on 32 MiB default, and 5-run median retained memory within 2x Linux."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    vm.type("test -x /usr/bin/hardened-malloc-probe && echo HM_READY || echo HM_MISSING\n")
    ready = vm.wait_serial(r"\r?\n(HM_READY|HM_MISSING)\r?\n", 20, since)
    if not ready or ready.group(1) != "HM_READY":
        return False, f"hardened-malloc-probe missing on image; see {vm.out / 'serial.log'}"
    sections = []
    problems = []
    metrics = {"loadavg_start": list(os.getloadavg())}
    steps = [
        ("syscalls", "hardened-malloc-probe syscalls 2>/tmp/hm_sys_err; rc=$?; echo STDERR_BYTES=$(wc -c < /tmp/hm_sys_err); (exit $rc)", 60),
        ("discard", "hardened-malloc-probe discard", 90),
        ("suite-default", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/small hardened-malloc-probe run-suite /usr/libexec/hardened-malloc/test", 360),
    ]
    for r in range(1, 6):
        steps.append((f"bench-dlmalloc-{r}", "hardened-malloc-bench-dlmalloc dlmalloc 2>/tmp/hm_err; rc=$?; echo STDERR_BYTES=$(wc -c < /tmp/hm_err); (exit $rc)", 120))
        steps.append((f"bench-default-{r}", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/small hardened-malloc-bench-hm hm-default 2>/tmp/hm_err; rc=$?; echo STDERR_BYTES=$(wc -c < /tmp/hm_err); (exit $rc)", 120))
        steps.append((f"bench-light-{r}", "LD_LIBRARY_PATH=/usr/lib/hardened-malloc/light-small hardened-malloc-bench-hm hm-light 2>/tmp/hm_err; rc=$?; echo STDERR_BYTES=$(wc -c < /tmp/hm_err); (exit $rc)", 120))
    outputs = {}
    for tag, cmd, step_timeout in steps:
        status, out = guest_run(vm, f"hmd{tag.replace('-', '')}", cmd, step_timeout)
        outputs[tag] = (status, out)
        sections.append(f"=== {tag} (exit {status}) ===\n{out}\n")
        if status != 0:
            problems.append(f"{tag} exit {status}")
    metrics["loadavg_end"] = list(os.getloadavg())
    (vm.out / "hardened-malloc-discard.txt").write_text("".join(sections))

    dout = outputs["discard"][1]
    for adv in ("MADV_DONTNEED", "MADV_DONTNEED_LOCKED", "MADV_FREE"):
        m = re.search(rf"discard-advice name={adv} rc=(-?\d+) errno=(\d+) stderr_bytes=(\d+) frames_delta=(-?\d+) all_zero=(\d+) rewritable=(\d+)", dout)
        if not m:
            problems.append(f"missing {adv}")
        else:
            rc, _, sb, fd, az, rw = map(int, m.groups())
            if rc != 0 or sb != 0 or fd > -32 or az != 1 or rw != 1:
                problems.append(f"{adv} rc={rc} stderr={sb} frames_delta={fd} zero={az} rw={rw}")
    m = re.search(r"discard-subrange rc=(-?\d+) errno=(\d+) stderr_bytes=(\d+) outer_kept=(\d+) inner_zero=(\d+)", dout)
    if not m or tuple(map(int, m.groups())) != (0, 0, 0, 1, 1):
        problems.append("subrange failed")
    m = re.search(r"discard-prot-none rc=(-?\d+) errno=(\d+) stderr_bytes=(\d+) frames_delta=(-?\d+) read_status=(-?\d+) all_zero=(\d+)", dout)
    if not m:
        problems.append("missing prot-none")
    else:
        rc, _, sb, fd, rs, az = map(int, m.groups())
        if rc != 0 or sb != 0 or fd > -32 or rs != -11 or az != 1:
            problems.append(f"prot-none rc={rc} sb={sb} fd={fd} rs={rs} az={az}")
    m = re.search(r"discard-posix-madvise rc=(-?\d+) data_kept=(\d+)", dout)
    if not m or tuple(map(int, m.groups())) != (0, 1):
        problems.append("posix_madvise altered data")
    m = re.search(r"discard-shared rc=(-?\d+) errno=(\d+) stderr_bytes=(\d+) data_kept=(\d+)", dout)
    if not m or tuple(map(int, m.groups())) != (0, 0, 0, 1):
        problems.append("MAP_SHARED discard must succeed without zeroing")
    m = re.search(r"discard-errors unalign=(-?\d+):(\d+) zero=(-?\d+):(\d+) bad=(-?\d+):(\d+) wipe=(-?\d+):(\d+) hole=(-?\d+):(\d+) hole_zero=(\d+)", dout)
    if not m or tuple(map(int, m.groups())) != (-1, 22, 0, 0, -1, 22, -1, 22, -1, 12, 1):
        problems.append("discard error cases mismatch")
    m = re.search(r"discard-sparse rc=(-?\d+) errno=(\d+) elapsed_us=(\d+) zero=(\d+)", dout)
    if not m:
        problems.append("missing sparse discard")
    else:
        rc, _, us, z = map(int, m.groups())
        metrics["sparse_discard_1g_us"] = us
        if rc != 0 or z != 1 or us > 10000:
            problems.append(f"sparse 1G discard rc={rc} us={us} zero={z}")
    for cname, exp_out in (("default", ""), ("blocked", ""), ("ignored", ""), ("caught", "caught\\n")):
        m = re.search(rf'discard-abort case={cname} status=(-?\d+) timed_out=(\d+) stdout_bytes=(\d+) stderr_bytes=(\d+) stdout="([^"]*)" stderr="([^"]*)"', dout)
        if not m:
            problems.append(f"missing abort {cname}")
        else:
            st, to, _, sb = map(int, m.groups()[:4])
            so = m.group(5)
            if st != -6 or to != 0 or sb != 0 or so != exp_out:
                problems.append(f"abort {cname} st={st} to={to} sb={sb} so={so!r}")

    def_sum = re.search(r"suite-summary passed=(\d+) failed=(\d+) total=(\d+)", outputs["suite-default"][1])
    if not def_sum or int(def_sum.group(3)) != 58 or int(def_sum.group(1)) != 58:
        problems.append(f"default suite not 58/58 strict ({def_sum.groups() if def_sum else 'missing'})")

    LINUX_RETAINED_KIB_DEFAULT = 14868
    LINUX_RETAINED_KIB_LIGHT = 2608
    bench_retained = {}
    bench_timings = {}
    for variant, label, max_kib in (
        ("default", "hm-default", 2 * LINUX_RETAINED_KIB_DEFAULT),
        ("light", "hm-light", 2 * LINUX_RETAINED_KIB_LIGHT),
        ("dlmalloc", "dlmalloc", 4096),
    ):
        vals = []
        smalls = []
        meds = []
        larges = []
        reallocs = []
        for r in range(1, 6):
            bout = outputs[f"bench-{variant}-{r}"][1]
            bm = re.search(rf"bench label={label} small_ms=(\d+) med_ms=(\d+) large_ms=(\d+) realloc_ms=(\d+) small_peak_kib=(-?\d+) med_peak_kib=(-?\d+) large_peak_kib=(-?\d+) retained_kib=(-?\d+)", bout)
            em = re.search(r"STDERR_BYTES=\s*(\d+)", bout)
            if not bm:
                problems.append(f"missing bench-{variant}-{r}")
            else:
                smalls.append(int(bm.group(1)))
                meds.append(int(bm.group(2)))
                larges.append(int(bm.group(3)))
                reallocs.append(int(bm.group(4)))
                vals.append(int(bm.group(8)))
            if not em or int(em.group(1)) != 0:
                problems.append(f"bench-{variant}-{r} stderr_bytes={em.group(1) if em else 'missing'}")
        if len(vals) == 5:
            med_ret = sorted(vals)[2]
            bench_retained[variant] = {"runs": vals, "median": med_ret}
            bench_timings[variant] = {
                "small_ms": {"runs": smalls, "median": sorted(smalls)[2]},
                "med_ms": {"runs": meds, "median": sorted(meds)[2]},
                "large_ms": {"runs": larges, "median": sorted(larges)[2]},
                "realloc_ms": {"runs": reallocs, "median": sorted(reallocs)[2]},
            }
            if med_ret > max_kib:
                problems.append(f"{variant} retained_kib median {med_ret} > {max_kib}")
    sys_em = re.search(r"STDERR_BYTES=\s*(\d+)", outputs["syscalls"][1])
    if not sys_em or int(sys_em.group(1)) != 0:
        problems.append(f"syscalls stderr_bytes={sys_em.group(1) if sys_em else 'missing'}")
    metrics["suite_default"] = dict(zip(("passed", "failed", "total"), map(int, def_sum.groups()))) if def_sum else None
    metrics["retained_kib"] = bench_retained
    metrics["timings_ms"] = bench_timings
    (vm.out / "hardened-malloc-discard.json").write_text(json.dumps(metrics, indent=2) + "\n")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'hardened-malloc-discard.txt'}"
    return True, (
        f"default suite 58/58 strict, retained_kib median default={bench_retained['default']['median']} "
        f"light={bench_retained['light']['median']} dlmalloc={bench_retained['dlmalloc']['median']}, "
        f"sparse 1G discard={metrics.get('sparse_discard_1g_us')}us; see {vm.out / 'hardened-malloc-discard.txt'}"
    )


OLD_STACK_GUARD_CONST = 0xD048C37519FCADFE
STACK_GUARD_PROBES = (
    ("static", "/usr/bin/stack-guard-probe-static", False),
    ("dynamic", "/usr/bin/stack-guard-probe-dynamic", True),
    ("pie", "/usr/bin/stack-guard-probe-pie", True),
)


def parse_hex_fields(text, prefix):
    match = re.search(rf"{re.escape(prefix)}[^\r\n]*", text)
    if not match:
        return {}
    return {k: int(v, 16) for k, v in re.findall(r"([a-z_]+)=0x([0-9a-fA-F]+)", match.group(0))}


def test_canary_random(vm, args):
    """Two runs and a parent/exec'd child of static, dynamic and PIE probes print distinct non-constant __stack_chk_guard values matching AT_RANDOM and DSO constructors (use --config stack-guard)."""
    if not root_shell(vm, args.timeout):
        return False, f"no root serial login within {args.timeout}s"
    summary = []
    for label, binary, has_dso in STACK_GUARD_PROBES:
        guards = []
        for run_idx in (1, 2):
            status, out = run_status(vm, f"{binary} guard", 30)
            (vm.out / f"{label}-guard-{run_idx}.txt").write_text(f"exit {status}\n{out}\n")
            if status != 0:
                return False, f"{label} guard run {run_idx} failed (exit {status})"
            fields = parse_hex_fields(out, "stack-guard-probe:")
            guard = fields.get("guard")
            if guard is None:
                return False, f"{label} guard run {run_idx} printed no guard"
            if guard == 0 or guard == OLD_STACK_GUARD_CONST:
                return False, f"{label} run {run_idx} guard is constant/zero: 0x{guard:016x}"
            if fields.get("exe_ctor") != guard:
                return False, f"{label} run {run_idx} exe_ctor 0x{fields.get('exe_ctor', 0):016x} != guard 0x{guard:016x}"
            if fields.get("aux") != guard:
                return False, f"{label} run {run_idx} AT_RANDOM 0x{fields.get('aux', 0):016x} != guard 0x{guard:016x}"
            if has_dso:
                if fields.get("dso_ctor") != guard or fields.get("dso_live") != guard:
                    return False, (
                        f"{label} run {run_idx} dso guard mismatch "
                        f"(ctor=0x{fields.get('dso_ctor', 0):016x}, live=0x{fields.get('dso_live', 0):016x}, guard=0x{guard:016x})"
                    )
            guards.append(guard)
        if guards[0] == guards[1]:
            return False, f"{label} two runs shared guard 0x{guards[0]:016x}"
        status, out = run_status(vm, f"{binary} exec", 30)
        (vm.out / f"{label}-exec.txt").write_text(f"exit {status}\n{out}\n")
        if status != 0:
            return False, f"{label} exec check failed (exit {status})"
        parent = parse_hex_fields(out, "stack-guard-probe: parent=").get("parent")
        child = parse_hex_fields(out, "stack-guard-probe: child=").get("child")
        if parent is None or child is None:
            return False, f"{label} exec printed no parent/child guards"
        if parent in (0, OLD_STACK_GUARD_CONST) or child in (0, OLD_STACK_GUARD_CONST):
            return False, f"{label} exec guard constant/zero (parent=0x{parent:016x}, child=0x{child:016x})"
        if parent == child:
            return False, f"{label} parent and exec'd child shared guard 0x{parent:016x}"
        summary.append(f"{label}: run1=0x{guards[0]:016x} run2=0x{guards[1]:016x} parent=0x{parent:016x} child=0x{child:016x}")
    return True, "; ".join(summary)


def test_stack_smash_aborts(vm, args):
    """Overflows in -fstack-protector-strong functions (static, dynamic, DSO, PIE) with both pattern bytes and the old constant guard abort through __stack_chk_fail (use --config stack-guard)."""
    if not root_shell(vm, args.timeout):
        return False, f"no root serial login within {args.timeout}s"
    summary = []
    for label, binary, has_dso in STACK_GUARD_PROBES:
        status, out = run_status(vm, f"{binary} smash", 30)
        (vm.out / f"{label}-smash.txt").write_text(f"exit {status}\n{out}\n")
        cases = ["smash-pattern", "smash-const"]
        if has_dso:
            cases.extend(["dso-smash-pattern", "dso-smash-const"])
        sigs = []
        for case in cases:
            if f"stack-guard-probe: {case} SURVIVED" in out:
                return False, f"{label} {case} survived stack overflow"
            found = re.search(rf"stack-guard-probe: {re.escape(case)} ABORTED sig=(\d+)", out)
            if not found:
                return False, f"{label} {case} did not abort via signal (exit {status})"
            sigs.append(f"{case}:sig={found.group(1)}")
        if status != 0:
            return False, f"{label} smash exited {status}"
        summary.append(f"{label}({', '.join(sigs)})")
    return True, "; ".join(summary)


FORTIFY_CASES = ("memcpy", "memmove", "memset", "strcpy", "strcat", "sprintf", "snprintf", "read")


def test_fortify_chk_aborts(vm, args):
    """Runtime-length overflows in -O2 -D_FORTIFY_SOURCE=2 code (static and dynamic) abort through __*_chk / __chk_fail while valid bounded calls succeed (use --config stack-guard)."""
    if not root_shell(vm, args.timeout):
        return False, f"no root serial login within {args.timeout}s"
    summary = []
    for label, binary in (("static", "/usr/bin/fortify-probe-static"), ("dynamic", "/usr/bin/fortify-probe-dynamic")):
        status, out = run_status(vm, binary, 30)
        (vm.out / f"fortify-{label}.txt").write_text(f"exit {status}\n{out}\n")
        if "fortify-probe: valid OK" not in out:
            return False, f"{label} valid calls failed (exit {status})"
        sigs = []
        for case in FORTIFY_CASES:
            if f"fortify-probe: {case} SURVIVED" in out:
                return False, f"{label} {case} overflow survived (not fortified)"
            found = re.search(rf"fortify-probe: {re.escape(case)} ABORTED sig=(\d+)", out)
            if not found:
                return False, f"{label} {case} did not abort via signal (exit {status})"
            sigs.append(f"{case}:sig={found.group(1)}")
        if status != 0:
            return False, f"{label} fortify-probe exited {status}"
        summary.append(f"{label}({len(sigs)}/{len(FORTIFY_CASES)} aborted)")
    return True, "; ".join(summary)

IOMMU_TRACE = ["vtd_reg_dmar_root", "vtd_dmar_enable", "vtd_reg_write_gcmd", "vtd_ir_enable",
               "vtd_dmar_translate", "vtd_dmar_fault", "iommu_testdev_dma_result"]
IOMMU_TESTDEV = "1b36:0005"
IOMMU_NVME = "1b36:0010"
IOMMU_TRANSLATE = re.compile(r"vtd_dmar_translate dev ([0-9a-f]{2}):([0-9a-f]{2})\.([0-9a-f]{2}) "
                             r"iova 0x([0-9a-f]+) -> gpa 0x([0-9a-f]+)")
IOMMU_STATUS_LINE = re.compile(r"^(?:backend|translation|mode|interrupt-remapping|unit|faults|fault-overflows|"
                               r"pinned-forever): .*$|^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7] domain .*$", re.M)
IOMMU_FAULT = re.compile(r"vtd_dmar_fault sid 0x([0-9a-f]+) fault (\d+) addr 0x([0-9a-f]+) write (\d)")


def iommu_mode(args):
    values = [assignment.split("=", 1)[1] for assignment in args.make if assignment.startswith("iommu=")]
    return values[-1] if values else "yes"


def pci_functions(vm):
    """Maps vendor:device to the bus:device.function QEMU's monitor lists it at."""
    found = {}
    reply = vm.command("info pci")
    for bus, device, function, ids in re.findall(
            r"Bus\s+(\d+), device\s+(\d+), function (\d+):\s*\n\s*.*?PCI device ([0-9a-f]{4}:[0-9a-f]{4})", reply):
        found.setdefault(ids, (int(bus), int(device), int(function)))
    return found


def pcap_udp(raw):
    """Returns (source port, destination port, payload) for each IPv4 UDP frame of an Ethernet pcap, or None."""
    order = {b"\xd4\xc3\xb2\xa1": "little", b"\xa1\xb2\xc3\xd4": "big"}.get(raw[:4])
    if order is None or len(raw) < 24 or int.from_bytes(raw[20:24], order) != 1:
        return None
    packets, pos = [], 24
    while pos + 16 <= len(raw):
        size = int.from_bytes(raw[pos + 8:pos + 12], order)
        frame = raw[pos + 16:pos + 16 + size]
        pos += 16 + size
        if len(frame) < 42 or frame[12:14] != b"\x08\x00":
            continue
        ip = frame[14:]
        ihl = (ip[0] & 15) * 4
        if ip[0] >> 4 != 4 or ihl < 20 or ip[9] != 17 or len(ip) < ihl + 8:
            continue
        udp = ip[ihl:]
        packets.append((int.from_bytes(udp[:2], "big"), int.from_bytes(udp[2:4], "big"), udp[8:]))
    return packets


def iommu_status_text(vm):
    status, output = run_status(vm, "cat /scheme/sys/iommu", 60)
    (vm.out / "iommu-status.txt").write_text(output)
    return status, output


def test_iommu(vm, args):
    """Slice 1 of S18: with iommu=yes the trace shows the root table set and translation turned on once, the boot disk's DMA translated one-to-one, no fault, and the guest reports translation on in identity mode and gets a DHCP lease; with --make iommu=no the guest reports backend none and the unit stays untouched."""
    mode = iommu_mode(args)
    capture = vm.out / "iommu.pcap"
    reply = vm.command(f"object_add filter-dump,id=ferrion-iommu,netdev=net0,maxlen=512,file={capture.resolve()}")
    if "error" in reply.lower():
        return False, f"could not capture net0: {reply}"
    try:
        if not root_shell(vm, args.timeout):
            return False, f"no root shell on the serial console within {args.timeout}s"
        status, text = iommu_status_text(vm)
        run_status(vm, "bash -c 'deadline=$((SECONDS+60)); "
                       "until [[ -s /etc/net/dns && \"$(cat /etc/net/dns)\" != 9.9.9.9 ]] || (( SECONDS >= deadline )); "
                       "do sleep 1; done'", 90)
        devices = pci_functions(vm)
    finally:
        vm.command("object_del ferrion-iommu")
    lines = [match.group(0).strip() for match in IOMMU_STATUS_LINE.finditer(text)]
    problems = []
    if status != 0:
        problems.append(f"cat /scheme/sys/iommu exited {status}")
    if mode == "no":
        if "backend: none" not in lines or "translation: off" not in lines:
            problems.append(f"status file says {lines[:3]}, expected backend none and translation off")
        if vm.trace("vtd_dmar_enable") or vm.trace("vtd_reg_dmar_root"):
            problems.append("the trace shows a VT-d unit without iommu=yes")
    else:
        for want in ("backend: vt-d", "translation: on", "mode: identity"):
            if want not in lines:
                problems.append(f"status file lacks {want!r}")
        events = vm.trace()
        roots = [i for i, line in enumerate(events) if "vtd_reg_dmar_root" in line]
        enables = [i for i, line in enumerate(events) if re.search(r"vtd_dmar_enable enable 1\b", line)]
        if len(enables) != 1:
            problems.append(f"{len(enables)} 'vtd_dmar_enable enable 1' lines, expected one")
        elif not roots or roots[0] > enables[0]:
            problems.append("no vtd_reg_dmar_root line before translation was turned on")
        nvme = devices.get(IOMMU_NVME)
        translated = []
        if nvme is None:
            problems.append(f"QEMU lists no NVMe function ({IOMMU_NVME})")
        else:
            for line in events:
                match = IOMMU_TRANSLATE.search(line)
                if match and tuple(int(match.group(i), 16) for i in (1, 2, 3)) == nvme:
                    translated.append((int(match.group(4), 16), int(match.group(5), 16)))
            moved = [(iova, gpa) for iova, gpa in translated if iova != gpa]
            if not translated:
                problems.append(f"no vtd_dmar_translate line for the boot disk at {nvme[0]:02x}:{nvme[1]:02x}.{nvme[2]}")
            elif moved:
                problems.append(f"{len(moved)} boot-disk translations moved an address, first {moved[0][0]:#x} -> {moved[0][1]:#x}")
        faults = vm.trace("vtd_dmar_fault")
        if faults:
            problems.append(f"{len(faults)} vtd_dmar_fault lines, first {faults[0].strip()}")
        (vm.out / "iommu-trace-summary.txt").write_text(
            f"root lines: {len(roots)}\nenable lines: {len(enables)}\nnvme: {nvme}\n"
            f"nvme translations: {len(translated)}\nfaults: {len(faults)}\n")
    packets = pcap_udp(capture.read_bytes()) if capture.exists() else None
    if packets is None:
        problems.append("the net0 capture is not an Ethernet pcap")
    else:
        dhcp = [(src, dst) for src, dst, payload in packets
                if {src, dst} == {67, 68} and len(payload) >= 240 and payload[236:240] == b"\x63\x82\x53\x63"]
        if (68, 67) not in dhcp or (67, 68) not in dhcp:
            problems.append(f"no DHCP exchange on net0 (saw {len(dhcp)} DHCP packets)")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    summary = [line for line in lines if line.startswith(("backend", "translation", "mode"))]
    extra = "" if mode == "no" else f", {len(translated)} one-to-one boot-disk translations, no fault"
    return True, f"{', '.join(summary)}{extra}, DHCP exchange captured"


def test_dma_outside_grant(vm, args):
    """Slice 2 of S18 (probe dma-outside-grant, image of --config iommu-test, iommu=test): iommu-probe has QEMU's iommu-testdev write to buffer A, allocated through the device's DMA handle, and to buffer B, allocated the old way; A succeeds, B is refused with a fault naming the device and B's address, B is unchanged, and the status the probe read lists the device's domain with one fault."""
    found = vm.wait_serial(r"iommu-probe: granted: (\w+), not granted: (\w+)", args.timeout)
    devices = pci_functions(vm)
    text = vm.text()
    (vm.out / "iommu-probe.txt").write_text("\n".join(line for line in text.splitlines() if "iommu" in line) + "\n")
    if not found:
        return False, f"iommu-probe printed no verdict within {args.timeout}s; see {vm.out / 'serial.log'}"
    problems = []
    if (found.group(1), found.group(2)) != ("ok", "blocked"):
        problems.append(f"probe: granted {found.group(1)}, not granted {found.group(2)}")
    testdev = devices.get(IOMMU_TESTDEV)
    if testdev is None:
        return False, f"QEMU lists no iommu-testdev ({IOMMU_TESTDEV})"
    sid = testdev[0] << 8 | testdev[1] << 3 | testdev[2]
    name = f"{testdev[0]:02x}:{testdev[1]:02x}.{testdev[2]}"
    b_address = re.search(r"iommu-probe: buffer B at 0x([0-9a-f]+)", text)
    results = []
    for line in vm.trace("iommu_testdev_dma_result"):
        match = re.search(r"(0x[0-9a-f]+|\b[0-9]+\b)\s*$", line.strip())
        if match:
            results.append(int(match.group(1), 0))
    if results[-2:] != [0, 0xDEAD0002]:
        problems.append(f"testdev results {[hex(r) for r in results]}, expected 0x0 then 0xdead0002 last")
    faults = [m for m in (IOMMU_FAULT.search(line) for line in vm.trace("vtd_dmar_fault")) if m]
    mine = [m for m in faults if int(m.group(1), 16) == sid]
    if not b_address:
        problems.append("the probe printed no address for buffer B")
    elif not any(int(m.group(3), 16) & ~0xFFF == int(b_address.group(1), 16) & ~0xFFF for m in mine):
        problems.append(f"no vtd_dmar_fault for sid {sid:#x} at B {b_address.group(1)}; saw {[m.group(0) for m in faults]}")
    if len(faults) != len(mine):
        problems.append(f"{len(faults) - len(mine)} faults from other devices")
    status = re.search(rf"iommu-probe: status: 0000:{re.escape(name)} domain (\d+) pages (\d+) faults (\d+)", text)
    if not status:
        problems.append(f"the probe's status snapshot lists no domain for {name}")
    elif status.group(3) != "1":
        problems.append(f"status lists {status.group(3)} faults for {name}, expected 1")
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    return True, (f"granted: ok, not granted: blocked; testdev {name} results 0x0 then 0xdead0002, "
                  f"fault at B {b_address.group(1)}, status domain {status.group(1)} pages {status.group(2)} faults 1")


TESTS = {"boot": test_boot, "shells": test_shells, "logout": test_logout, "ls-loop": test_ls_loop,
         "signal-handle": test_signal_handle, "screenshots": test_screenshots,
         "tpm": test_tpm, "lockdown": test_lockdown, "login-namespace": test_login_namespace,
         "dev-parity": test_dev_parity, "firefox-headless": test_firefox_headless, "rand-seed": test_rand_seed,
         "firefox-cjk": test_firefox_cjk,
         "ferrion-run": test_ferrion_run}
TESTS["ferrion-cli"] = test_ferrion_cli
TESTS["update-today"] = test_update_today
TESTS["update-index"] = test_update_index
TESTS["driver-namespaces"] = test_driver_namespaces
TESTS["scheme-create"] = test_scheme_create
TESTS["nsmgr-bounds"] = test_nsmgr_bounds
TESTS["graphics-bounds"] = test_graphics_bounds
TESTS["gpu-heads"] = test_gpu_heads
TESTS["sudo-notty"] = test_sudo_notty
TESTS["scheme-short-read"] = test_scheme_short_read
TESTS["event-poll"] = test_event_poll
TESTS["desktop-regions"] = test_desktop_regions
TESTS["shm-reuse"] = test_shm_reuse
TESTS["shm-read-back"] = test_shm_read_back
TESTS["resolver"] = test_resolver
TESTS["cli-tools"] = test_cli_tools
TESTS["dlopen-ctor"] = test_dlopen_ctor
TESTS["dlopen-tls"] = test_dlopen_tls
TESTS["cond-clock"] = test_cond_clock
TESTS["fork-wake"] = test_fork_wake
TESTS["ipc-fd"] = test_ipc_fd
TESTS["jit-signal"] = test_jit_signal
TESTS["fdtable-race"] = test_fdtable_race
TESTS["fork-filetable"] = test_fork_filetable
TESTS["exec-handles"] = test_exec_handles
TESTS["nvme-queue"] = test_nvme_queue
TESTS["ring-wake"] = test_ring_wake
TESTS["firefox-js"] = test_firefox_js
TESTS["fd-primitives"] = test_fd_primitives
TESTS["foreign-call"] = test_foreign_call
TESTS["linux-hello"] = test_linux_hello
TESTS["linux-busybox"] = test_linux_busybox
TESTS["linux-dynamic"] = test_linux_dynamic
TESTS["linux-python"] = test_linux_python
TESTS["linux-net"] = test_linux_net
TESTS["dso-atexit"] = test_dso_atexit
TESTS["relro-got-readonly"] = test_relro_got_readonly
TESTS["mesa-cpu"] = test_mesa_cpu
TESTS["gl-exact"] = test_gl_exact
TESTS["gtk-window"] = test_gtk_window
TESTS["gtk-window-wayland"] = test_gtk_window_wayland
TESTS["firefox-window"] = test_firefox_window
TESTS["firefox-start"] = test_firefox_start
TESTS["directory-fsync"] = test_directory_fsync
TESTS["file-locks"] = test_file_locks
TESTS["gpu-context"] = test_gpu_context
TESTS["gpu-blob"] = test_gpu_blob
TESTS["gpu-submit"] = test_gpu_submit
TESTS["gpu-syncobj"] = test_gpu_syncobj
TESTS["gpu-flip"] = test_gpu_flip
TESTS["vk-exact"] = test_vk_exact
TESTS["agent-scope"] = test_agent_scope
TESTS["agent-clone"] = test_agent_clone
TESTS["agent-scope-overhead"] = test_agent_scope_overhead
TESTS["hardened-malloc"] = test_hardened_malloc
TESTS["hardened-malloc-hotpath"] = test_hardened_malloc_hotpath
TESTS["hardened-malloc-sparse"] = test_hardened_malloc_sparse
TESTS["hardened-malloc-pt-order"] = test_hardened_malloc_pt_order
TESTS["hardened-malloc-discard"] = test_hardened_malloc_discard
TESTS["canary-random"] = test_canary_random
TESTS["stack-smash-aborts"] = test_stack_smash_aborts
TESTS["fortify-chk-aborts"] = test_fortify_chk_aborts
TESTS["iommu"] = test_iommu
TESTS["dma-outside-grant"] = test_dma_outside_grant
# These boot copies of their own images instead of the shared one, and are called with (args, out).
OWN_IMAGE = {"disk-keys": test_disk_keys, "disk-install": test_disk_install}
OWN_IMAGE["one-password"] = test_one_password
OWN_IMAGE["key-slot-roll"] = test_key_slot_roll
OWN_IMAGE["wayland-client"] = run_wayland_client
OWN_IMAGE["pointer-latency"] = run_pointer_latency
OWN_IMAGE["desktop-columns"] = run_desktop_columns
OWN_IMAGE["desktop-columns-timing"] = run_desktop_columns_timing
OWN_IMAGE["display-handoff"] = run_display_handoff
OWN_IMAGE["mkfs-reuse"] = test_mkfs_reuse
OWN_IMAGE["boot-gc-generation"] = test_boot_gc_generation
OWN_IMAGE["x2apic-cpus"] = test_x2apic_cpus
def test_net_basics(args, out):
    """Lab check of NW1's nine network basics items, one line per item, on the card --make net= picks."""
    import net_lab
    return net_lab.run_net_basics(
        args,
        out,
        {
            "VM": VM,
            "Blocked": Blocked,
            "NET_CARDS": NET_CARDS,
            "net_card": net_card,
            "root_shell": root_shell,
            "run_status": run_status,
            "dns_reply": dns_reply,
            "consecutive_run": consecutive_run,
            "pcg_prediction": pcg_prediction,
        },
    )


def test_net_perf(args, out):
    """Lab (topology A) and QEMU user-network (topology B) baseline measurements with iperf3 and net-probe."""
    import net_lab
    return net_lab.run_net_perf(
        args,
        out,
        {
            "VM": VM,
            "Blocked": Blocked,
            "NET_CARDS": NET_CARDS,
            "net_card": net_card,
            "root_shell": root_shell,
            "run_status": run_status,
            "dns_reply": dns_reply,
            "command_output": command_output,
        },
    )


# These start their own boots of the shared image, with a peer on QEMU's network hub.
OWN_IMAGE["net-hardening"] = test_net_hardening
OWN_IMAGE["net-calls"] = test_net_calls
OWN_IMAGE["net-basics"] = test_net_basics
OWN_IMAGE["net-perf"] = test_net_perf
# These boot with their own sound device, recorded by QEMU's wav backend.
OWN_IMAGE["audio-baseline"] = test_audio_baseline
OWN_IMAGE["audio-ring"] = test_audio_ring
OWN_IMAGE["audio-apps"] = test_audio_apps
OWN_IMAGE["audio-snoop"] = test_audio_snoop
TESTS.update(OWN_IMAGE)


def main():
    parser = argparse.ArgumentParser(
        description="Boot a FerrionOS image in QEMU and check it from the outside.",
        epilog="tests: " + "; ".join(f"{name}: {fn.__doc__}" for name, fn in TESTS.items()))
    parser.add_argument("tests", nargs="*", metavar="test", help="one or more of the tests below")
    parser.add_argument("--arch", default="x86_64")
    parser.add_argument("--config", default="desktop")
    parser.add_argument("--repeat", type=int, default=1, help="boot this many times per test")
    parser.add_argument("--timeout", type=float, default=60, help="seconds a step may take")
    parser.add_argument("--host-load-limit", type=float, default=(os.cpu_count() or 1) / 2,
                        help="desktop-columns-timing: maximum load1 for eligible timings")
    parser.add_argument("--host-load-wait", type=float, default=600,
                        help="desktop-columns-timing: seconds to obtain each eligible boot")
    parser.add_argument("--timing-budget", type=float, default=1800,
                        help="desktop-columns-timing: total seconds allowed across all boots")
    parser.add_argument("--out", type=Path, default=None, help="default: build/vm-test/<arch>/<config>")
    parser.add_argument("--make", action="append", default=[], metavar="VAR=VALUE",
                        help="extra make variable for `make qemu`, such as net=no; repeatable")
    parser.add_argument("--trace", action="append", default=[], metavar="EVENT",
                        help="enable a QEMU trace event or pattern, such as virtio_gpu_*; repeatable")
    parser.add_argument("--display", action="append", default=[], metavar="SCHEME",
                        help="graphics-bounds: check only this display scheme, such as display.virtio-gpu; repeatable")
    parser.add_argument("--check-wav", type=Path, metavar="WAV",
                        help="boot nothing: compare a recording from QEMU's wav backend with --signal and print the "
                             "checker's verdict; exit 0 only when the signal is exact in one piece")
    parser.add_argument("--signal", choices=("rig", "seq"), default="rig",
                        help="--check-wav: the rig's counter signal (plans/audio.md) or audio-baseline's seq bytes")
    parser.add_argument("--venus", action="store_true",
                        help="gl-exact: select Venus's Vulkan ICD via VK_DRIVER_FILES and expect Zink on Venus")
    parser.add_argument("--venus-icd", default="",
                        help="gl-exact --venus: override VK_DRIVER_FILES path")
    parser.add_argument("--lab", action="store_true",
                        help="run inside an unprivileged user and network namespace with no route out")
    parser.add_argument("--in-lab", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--compare-net-perf", action="store_true",
                        help="compare two net-perf.json pass files against the within-noise rule without booting")
    parser.add_argument("--legacy-unrecorded-nice", action="store_true",
                        help="--compare-net-perf: permit comparing pre-guard passes whose QEMU nice was not recorded")
    parser.add_argument("--window-deadline", default=None, metavar="TIME",
                        help="net-perf: quiet-window deadline (HH:MM:SS or ISO-8601); ending past it marks host-limited")
    if len(sys.argv) == 1:
        parser.print_help()
        sys.exit(2)
    args = parser.parse_args()
    if args.legacy_unrecorded_nice and not args.compare_net_perf:
        parser.error("--legacy-unrecorded-nice requires --compare-net-perf")
    if args.compare_net_perf:
        if len(args.tests) != 2:
            parser.error("--compare-net-perf requires exactly two pass files")
        pass_paths = [Path(p) for p in args.tests]
        for path in pass_paths:
            if not path.is_file():
                parser.error(f"--compare-net-perf: file not found: {path}")
        import net_lab
        p1 = json.loads(pass_paths[0].read_text())
        p2 = json.loads(pass_paths[1].read_text())
        ok, checked, mismatches = net_lab.compare_perf_passes(
            p1, p2, allow_legacy_unrecorded_nice=args.legacy_unrecorded_nice
        )
        legacy_note = (
            " (nice not recorded)"
            if args.legacy_unrecorded_nice
            and net_lab.is_legacy_pre_guard_pass(p1)
            and net_lab.is_legacy_pre_guard_pass(p2)
            else ""
        )
        if ok is None:
            print(f"BLOCKED compare-net-perf: {'; '.join(mismatches)}")
            sys.exit(2)
        if ok:
            print(f"PASS compare-net-perf: {checked} measures within noise{legacy_note}")
            sys.exit(0)
        print(f"FAIL compare-net-perf: {len(mismatches)}/{checked} outside noise{legacy_note}: {'; '.join(mismatches)}")
        sys.exit(1)
    if args.check_wav:
        report = check_wav(rig_signal() if args.signal == "rig" else seq_signal()[0], args.check_wav.read_bytes())
        print(f"{'EXACT' if report['one_piece'] and not report['foreign'] else 'NOT EXACT'} "
              f"{args.check_wav}: {wav_summary(report)}")
        sys.exit(0 if report["one_piece"] and not report["foreign"] else 1)
    unknown = [name for name in args.tests if name not in TESTS]
    if unknown or not args.tests:
        parser.error(f"unknown tests {unknown}" if unknown else "name a test, or --check-wav")
    if "rand-seed" in args.tests and args.arch != "x86_64":
        parser.error("rand-seed requires x86_64 with KVM and a host CPU RNG")
    if args.host_load_limit < 0 or args.host_load_wait <= 0 or args.timing_budget <= 0:
        parser.error("load limit must be nonnegative and timing budgets must be positive")
    for assignment in args.make:
        if not re.match(r"^[A-Za-z_][A-Za-z_0-9]*=", assignment):
            parser.error("--make requires VAR=VALUE, not a make target or option")
        if assignment.split("=", 1)[0] in ("QEMUFLAGS", "QEMU_DISPLAY", "ARCH", "CONFIG_NAME", "BUILD"):
            parser.error("use --arch, --config and --trace; the harness controls QEMUFLAGS, QEMU_DISPLAY and BUILD")
    out_root = args.out or ROOT / "build" / "vm-test" / args.arch / args.config
    import net_lab
    need_lab = args.lab or any(name in net_lab.LAB_TESTS for name in args.tests)
    if need_lab and not args.in_lab:
        try:
            net_lab.reexec_in_lab(sys.argv, Blocked, args.tests)
        except Blocked as error:
            for name in args.tests:
                for run in range(1, args.repeat + 1):
                    out = out_root / f"{name}-{run}"
                    out.mkdir(parents=True, exist_ok=True)
                    result = {"test": name, "run": run, "verdict": "BLOCKED", "note": str(error), "host": {},
                              "make": list(args.make), "capture_sources": [], "last_capture_source": None,
                              "trace_events": list(args.trace), "trace_lines": 0}
                    (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
                    print(f"BLOCKED {name} {run}/{args.repeat}: {error}", flush=True)
            sys.exit(2)
    if args.in_lab:
        try:
            net_lab.bring_up_loopback(Blocked)
        except Blocked as error:
            print(f"BLOCKED lab setup: {error}", flush=True)
            sys.exit(2)
    def on_signal(signum, _frame):
        raise Interrupted(signum)

    for sig in HANDLED_SIGNALS:
        signal.signal(sig, on_signal)

    failures = blocked = 0
    for name in args.tests:
        for run in range(1, args.repeat + 1):
            out = out_root / f"{name}-{run}"
            out.mkdir(parents=True, exist_ok=True)
            vm = None
            interrupted = None
            host = {}
            captures = []
            last_capture = None
            trace_lines = 0
            make = ("gpu=multi",) if name == "gpu-heads" else ()
            run_args = args
            if name in ("gpu-context", "gpu-submit"):
                if not any(assignment.startswith("gpu=") for assignment in args.make):
                    make = ("gpu=venus",)
                extra_trace = GPU_CONTEXT_TRACE if name == "gpu-context" else GPU_SUBMIT_TRACE
                run_args = argparse.Namespace(**{**vars(args), "trace": [*args.trace, *extra_trace]})
            elif name == "gpu-blob":
                if not any(assignment.startswith("gpu=") for assignment in args.make):
                    make = ("gpu=venus",)
                run_args = argparse.Namespace(**{**vars(args), "trace": [*args.trace, *GPU_BLOB_TRACE]})
            elif name == "gpu-syncobj":
                if not any(assignment.startswith("gpu=") for assignment in args.make):
                    make = ("gpu=venus",)
            elif name == "gpu-flip":
                if not any(assignment.startswith("gpu=") for assignment in args.make):
                    make = ("gpu=virtio",)
            elif name == "vk-exact":
                if not any(assignment.startswith("gpu=") for assignment in args.make):
                    make = ("gpu=venus",)
                run_args = argparse.Namespace(**{**vars(args), "trace": [*args.trace, *VK_EXACT_TRACE]})
            elif name in ("iommu", "dma-outside-grant"):
                if not any(assignment.startswith("iommu=") for assignment in args.make):
                    make = ("iommu=yes",) if name == "iommu" else ("iommu=test",)
                run_args = argparse.Namespace(**{**vars(args), "trace": [*args.trace, *IOMMU_TRACE]})
            elif name == "gl-exact" and args.venus:
                if not any(assignment.startswith("gpu=") for assignment in args.make):
                    make = ("gpu=venus",)
                run_args = argparse.Namespace(**{**vars(args), "trace": [*args.trace, *GL_EXACT_VENUS_TRACE]})
            try:
                qemu, gpu, vk_driver_files, render_node = qemu_settings(args, make)
                host = host_versions(qemu)
                host["vk_driver_files"] = vk_driver_files or None
                host["render_node"] = (render_node or boot_render_node()) if gpu in ("venus", "virtio-gl") else None
                require_3d_host(qemu, gpu, args.arch)
                if name in OWN_IMAGE:
                    ok, note = OWN_IMAGE[name](args, out)
                else:
                    vm = VM(run_args, out, tpm=name == "tpm", make=make,
                            qemu_flags=RAND_FIRMWARE if name == "rand-seed" else "")
                    ok, note = TESTS[name](vm, run_args)
                verdict = "PASS" if ok else "FAIL"
            except Blocked as error:
                verdict, note = "BLOCKED", str(error)
            except Interrupted as error:
                interrupted = error
                verdict, note = "INTERRUPTED", str(error)
                for sig in HANDLED_SIGNALS:
                    signal.signal(sig, signal.SIG_IGN)
            except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
                verdict, note = "FAIL", str(error)
            finally:
                if vm is not None:
                    vm.close()
                    captures = sorted(vm.capture_sources)
                    last_capture = vm.last_capture_source
                    trace_lines = len(vm.trace())
            host["load_average"] = [round(load, 2) for load in os.getloadavg()]
            host["qemu_priority"] = vm.priority if vm is not None else None
            failures += verdict == "FAIL"
            blocked += verdict == "BLOCKED"
            result = {"test": name, "run": run, "verdict": verdict, "note": note, "host": host,
                      "make": [*args.make, *make], "capture_sources": captures, "last_capture_source": last_capture,
                      "trace_events": run_args.trace, "trace_lines": trace_lines}
            if interrupted is not None:
                result["signal"] = interrupted.signum
                result["signal_name"] = interrupted.signal_name
            if name in ("desktop-columns", "desktop-columns-timing") and (out / "measurements.json").exists():
                result["desktop_columns"] = json.loads((out / "measurements.json").read_text())
                if (out / "host-load.json").exists():
                    from desktop_load import annotate, load_status

                    samples = json.loads((out / "host-load.json").read_text())
                    status = load_status(samples, result["desktop_columns"], args.host_load_limit)
                    annotate(result["desktop_columns"], status)
                    result["desktop_columns"]["host_load_monitor"] = samples
            if name == "desktop-columns-timing" and (out / "timing-status.json").exists():
                result["timing_host_gate"] = json.loads((out / "timing-status.json").read_text())
            (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            print(f"{verdict} {name} {run}/{args.repeat}: {note}", flush=True)
            if interrupted is not None:
                sys.exit(128 + interrupted.signum)
            if name == "desktop-columns-timing" and verdict == "BLOCKED":
                break
        if name == "net-perf" and args.repeat >= 2:
            p1_path = out_root / "net-perf-1" / "net-perf.json"
            p2_path = out_root / "net-perf-2" / "net-perf.json"
            if p1_path.exists() and p2_path.exists():
                ok_cmp, checked_cmp, mismatches_cmp = net_lab.compare_perf_passes(
                    json.loads(p1_path.read_text()), json.loads(p2_path.read_text())
                )
                if ok_cmp is None:
                    blocked += 1
                    print(f"BLOCKED net-perf repeatability: {'; '.join(mismatches_cmp)}", flush=True)
                elif ok_cmp:
                    print(f"PASS net-perf repeatability: {checked_cmp} measures within noise", flush=True)
                else:
                    failures += 1
                    print(
                        f"FAIL net-perf repeatability: {len(mismatches_cmp)}/{checked_cmp} outside noise: "
                        f"{'; '.join(mismatches_cmp)}",
                        flush=True,
                    )
    sys.exit(1 if failures else 2 if blocked else 0)


if __name__ == "__main__":
    main()
