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
#           driver holds can open only memory and pipe (audiod: memory and
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
# --check-wav FILE boots nothing: it runs the same checker on a recording of the
# rig's counter signal (plans/audio.md, "The rig"), or of the seq bytes with
# --signal seq, and exits 0 only when the signal is exact in one piece.
# Every run boots with -snapshot, so the image is never changed (disk-keys and
# disk-install boot copies in a temporary directory instead), and `make` is
# told the image is up to date, so a run never starts a build. Screenshots
# (PNG) and the serial log of each run go under --out. Run from the
# repository root. Python standard library only.
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


class Blocked(RuntimeError):
    """The host cannot run the requested graphics device."""


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


class RFB:
    """Read raw RGB frames from QEMU's private, unauthenticated Unix VNC socket."""

    def __init__(self, sock):
        self.sock = sock
        self.deadline = None
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
        try:
            while True:
                width, height = self.width, self.height
                if len(self.rgb) != width * height * 3:
                    self.rgb = bytearray(width * height * 3)
                missing = bytearray(b"\x01") * (width * height)
                left = width * height
                resized = False
                while left and not resized:
                    self.sock.sendall(struct.pack("!BBHHHH", 3, 0, 0, 0, width, height))
                    resized, covered = self._update(width, height, missing)
                    left -= covered
                if not resized:
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
        covered = 0
        for _ in range(struct.unpack("!H", self.read(2))[0]):
            x, y, w, h, encoding = struct.unpack("!HHHHi", self.read(12))
            if encoding == -223:
                self.width, self.height = w, h
                resized = True
                continue
            if encoding != 0:
                raise RuntimeError(f"unexpected VNC encoding {encoding}")
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
        return resized, covered


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
        self.scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-"))
        self.monitor_path = self.scratch / "monitor.sock"
        self.vnc_path = self.scratch / "vnc.sock"
        self.tpm = Swtpm() if tpm else None
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
        self.proc = subprocess.Popen(command, cwd=ROOT, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.STDOUT, start_new_session=True)
        self.serial = bytearray()
        self.lock = threading.Lock()
        self.typing = threading.Lock()
        self.log = open(out / "serial.log", "wb")
        self.reader = threading.Thread(target=self._read_serial, daemon=True)
        self.reader.start()
        try:
            self.monitor = self._connect_monitor()
            if self.is_3d:
                self.vnc = self._connect_vnc()
                self.rfb = RFB(self.vnc)
        except (RuntimeError, OSError):
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

    def kill(self):
        """Stops QEMU at once, as pulling the plug would."""
        os.killpg(self.proc.pid, signal.SIGKILL)
        self.proc.wait(timeout=20)

    def close(self):
        if self.proc.poll() is None:
            try:
                if hasattr(self, "monitor"):
                    self.monitor.sendall(b"quit\n")
                else:
                    os.killpg(self.proc.pid, signal.SIGTERM)
                self.proc.wait(timeout=20)
            except (OSError, subprocess.TimeoutExpired):
                os.killpg(self.proc.pid, signal.SIGTERM)
                self.proc.wait(timeout=20)
        if hasattr(self, "monitor"):
            self.monitor.close()
        if hasattr(self, "vnc"):
            self.vnc.close()
        self.reader.join(timeout=5)
        self.log.close()
        if self.tpm:
            self.tpm.close()
        for path in self.scratch.iterdir():
            path.unlink()
        self.scratch.rmdir()


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


def test_fd_primitives(vm, args):
    """Runs the public eventfd/timerfd probe as user: counter and timer I/O, errors, poll/epoll, shared dup/fork state and flags across exec; signalfd is not implemented or tested."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    since = vm.mark()
    # The shell, not the probe, reports exit; the typed command cannot match it.
    vm.type("bash -c 'fd-primitives-probe; echo fd-primitives-exit-$?'\n")
    found = vm.wait_serial(r"(?s)\n(.*?)\nfd-primitives-exit-(\d+)\s", 30, since)
    output = found.group(1) if found else vm.text(since)
    status = int(found.group(2)) if found else None
    log = vm.out / "fd-primitives.txt"
    log.write_text(f"exit {status}\n{output}\n")
    if status is None:
        return False, f"descriptor probe did not exit within 30s; see {log}"
    if status != 0 or "fd-primitives-probe: FAIL" in output:
        return False, f"descriptor probe failed (exit {status}); see {log}"
    expected = (
        "eventfd-counter-errors", "eventfd-semaphore", "eventfd-poll-epoll",
        "eventfd-dup-fork-blocking", "timerfd-errors",
        "timerfd-monotonic-relative-absolute-readiness",
        "timerfd-realtime-relative-absolute-readiness",
        "timerfd-gettime-interval-disarm-rearm", "timerfd-dup-fork-blocking",
        "descriptor-flags-exec",
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
    if results["probe"][0] != 0:
        return False, f"probe exit {results['probe'][0]}: {'; '.join(failed) or 'no result line'}; see {log}"
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


def guest_run(vm, tag, line, timeout):
    """Runs a line through bash on the serial console; returns (exit status or None, output)."""
    since = vm.mark()
    # The status is echoed inside bash: `user`'s login shell is not POSIX.
    vm.type(f"bash -c '{line}; echo {tag}-end-$?'\n")
    found = vm.wait_serial(rf"(?s)\n(.*){tag}-end-(\d+)", timeout, since)
    return (int(found.group(2)), found.group(1)) if found else (None, vm.text(since))


def test_foreign_call(vm, args):
    """Linux layer LC1: as `user`, foreign-call-probe checks the kernel's foreign-call hook (bad registrations refused, registers, flags, XMM0 and FS/GS intact, per-thread, cleared by exec, native signals at the boundary), then `linux-run` runs linux-probe, a C-library-free Linux program checking FS, XMM and register crossings, Linux signal frames, the alternate stack, masking and clone. Both print their call costs."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    results = {}
    for tag, line in (("fcp", "foreign-call-probe"),
                      ("lp", "linux-run /usr/share/foreign-call-probe/linux-probe")):
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
                                f"linux-run {d}/{name} >/tmp/lh.out 2>/tmp/lh.err; s=$?; "
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
                                "linux-run /tmp/e32", 60)
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
    status, out = guest_run(vm, "bb", f"cd /tmp && EXPECT_UID=$UID linux-run {d}/busybox sh {d}/lc2.sh >/tmp/bb.out 2>/tmp/bb.err </dev/null; "
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
    status, out = guest_run(vm, "tty", f"linux-run {d}/busybox sh {d}/tty.sh", 120)
    log.append(f"[tty.sh] exit {status}\n{out}\n")
    if "echo off" not in out or "termios ok" not in out:
        problems.append("stty under busybox: no `echo off` and `termios ok`")
    else:
        report.append("termios round trip")
    status, out = guest_run(vm, "lp2", "linux-run /usr/share/foreign-call-probe/linux-probe lc2", 300)
    log.append(f"[linux-probe lc2] exit {status}\n{out}\n")
    lines = re.findall(r"linux-probe: (?:pass|FAIL) .*", out)
    failed = [line.strip() for line in lines if ": FAIL " in line]
    if status != 0 or failed or not lines:
        problems.append(f"linux-probe lc2 exit {status}: {'; '.join(failed) or 'no FAIL line'}")
    else:
        report.append(f"linux-probe lc2 {len(lines)} passed")
    status, out = guest_run(vm, "kl", f"linux-run {d}/busybox sh -c \"sleep 30 & sleep 31\" & l=$!; sleep 3; "
                            "echo kl-before $(grep -c linux-run /scheme/sys/context); kill -9 $l; sleep 3; "
                            "echo kl-after $(grep -c linux-run /scheme/sys/context); grep linux-run /scheme/sys/context", 120)
    log.append(f"[kill launcher] exit {status}\n{out}\n")
    found = re.search(r"kl-before (\d+).*kl-after (\d+)", out, re.S)
    report.append(f"killed launcher: linux-run processes {found.group(1)} before, {found.group(2)} after" if found
                  else "killed launcher: no count")
    (vm.out / "linux-busybox.txt").write_text("".join(log))
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out / 'linux-busybox.txt'}"
    return True, "; ".join(report)


def test_linux_dynamic(vm, args):
    """Linux layer LC3: as `user`, `linux-run --session` starts a Linux session (linux-sessiond gives it the pinned Debian root read-only and a memory file system) and runs Debian's dash on the LC3 fixture (hash-checked in its recipe stage first), fed on standard input: the dynamic glibc hello, the four-thread program, a posix_spawn of a missing file that must report ENOENT, the session's checks (no DISPLAY, no /tmp/.X11-unix, no /lib/ld-linux.so.2, a write into /usr refused) and the core tools script over 30 tools with find, grep, tar and gzip. Its stdout, stderr and status must equal what Linux gave. Then, from the native login, `ls /scheme` must show neither `linux-root` nor `linux-mem`."""
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
    status, out = guest_run(vm, "ld", "cd /tmp && linux-run --session /bin/dash -c "
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

    since = vm.mark()
    # Serial logins do not inherit the compositor's graphical environment.
    if backend == "wayland":
        environment = ("until test -f /tmp/gtk-window-session; do sleep 1; done; "
                       ". /tmp/gtk-window-session; GDK_BACKEND=wayland ")
    else:
        environment = "GDK_BACKEND=orbital ORBITAL_DISPLAY=/scheme/orbital "
    vm.type(f"bash -c '{environment}gtk-window-probe; echo GTKRC-$((40+2))=$?'\n")
    opened = vm.wait_serial(r"GTK-WINDOW: display=(\S+) type=(\S+)", 30, since)
    if not opened:
        vm.screenshot().save_png(vm.out / "gtk-window-failed.png")
        return False, "GTK did not open a display; see serial.log"
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
    return True, f"{backend} GTK window painted and closed: {counts}; see gtk-window.png"


def test_gtk_window_wayland(vm, args):
    """Checks GTK's Wayland window inside the nested cosmic-comp session (use --config firefox-wayland)."""
    return test_gtk_window(vm, args, backend="wayland")


def run_wayland_client(args, out, *, pointer_latency=False):
    """Checks the DD1 pixman compositor with a cursor-capable virtio display and QMP input."""
    with tempfile.TemporaryDirectory(prefix="ferrion-dd-qmp-") as directory:
        path = Path(directory) / "qmp.sock"
        boot_load = os.getloadavg()
        vm = VM(args, out, qemu_flags=f"-qmp unix:{path},server=on,wait=off", make=("gpu=virtio",))
        try:
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
            vm.close()

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


def test_desktop_regions(vm, args):
    """Compares the desktop's region probes with flat/first-run.svg within two RGB steps."""
    import xml.etree.ElementTree as ET

    fixture = ROOT / "scripts/desktop-fixtures/flat/first-run.svg"
    svg = ET.parse(fixture).getroot()
    ns = "{http://www.w3.org/2000/svg}"
    regions = [
        rect for group in svg.iter(ns + "g") if group.get("id") == "regions"
        for rect in group.iter(ns + "rect")
    ]
    if not regions:
        return False, f"no regions in {fixture.name}"
    probes = []
    for region in regions:
        points = []
        for probe in region.attrib["data-probes"].split():
            place, color = probe.split(":")
            x, y = map(int, place.split(","))
            expected = bytes.fromhex(color.removeprefix("#"))
            if len(expected) != 3:
                raise ValueError(f"invalid RGB probe in {region.attrib['id']}")
            points.append((x, y, expected))
        # A region whose content changes (the status area's clock) carries no probes.
        if points:
            probes.append((region.attrib["id"], points))

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
        differences = []
        for x, y, expected in points:
            if not (0 <= x < frame.width and 0 <= y < frame.height):
                differences.append(f"({x},{y}) outside {frame.width}x{frame.height}")
                continue
            at = (y * frame.width + x) * 3
            seen = frame.rgb[at:at + 3]
            if any(abs(a - b) > 2 for a, b in zip(seen, expected)):
                differences.append(f"({x},{y}) #{seen.hex()}, expected #{expected.hex()}")
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
DRIVER_NAMESPACES["audiod"] = {"memory", "rand"}
# Running on the x86_64 desktop under `make qemu` once the login appears; the others start only with
# their device (a USB HID device, disk or hub, which `make qemu` does not attach, virtio-gpu,
# redoxerd's config), exit after one job (rtcd) or are a command (usbctl).
DRIVER_NAMESPACES_RUNNING = ["pcid", "nvmed", "inputd", "ihdad", "fbcond", "fbbootlogd", "audiod"]
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
    """Every namespace handle a running driver holds names only memory and pipe (audiod: memory and rand), read from /scheme/sys/iostat; Enter on the login screen still reaches the desktop."""
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


def test_firefox_headless(vm, args):
    """Logs in as `user` and has `firefox --headless --screenshot` render a local page and then an HTTPS page to PNG, which travels back over the serial console (use --config firefox)."""
    if not vm.wait_serial(r"login: ", args.timeout):
        return False, f"no serial login prompt within {args.timeout}s"
    vm.type("user\n")
    time.sleep(3)
    vm.type("bash --norc --noprofile --noediting\n")
    time.sleep(1)

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
    status, output = run("inherit", f"ferrion-run-probe hold-socket {address} -- "
                                    "ferrion-run --no-network ferrion-run-probe use-fd /held-launched")
    if "/held-launched" in seen or status != 1 or re.search(r"descriptor \S+ usable:", output):
        problems.append(f"a socket opened before the launch worked in the program (exit {status})")

    status, output = run("stdout", "ferrion-run --no-network true > /tmp/ferrion-run-stdout")
    if status != 125 or "file on disk" not in output:
        problems.append(f"a disk file as standard output was not refused (exit {status})")
    status, output = run("stdin", f"ferrion-run-probe hold-socket {address} --stdin -- ferrion-run --no-network true")
    if status != 125 or "is a socket" not in output:
        problems.append(f"a socket as standard input was not refused (exit {status})")
    status, output = run("files", "ferrion-run --files /tmp ls")
    if status != 125 or "file-access proxy" not in output:
        problems.append(f"--files was not refused (exit {status})")

    (vm.out / "ferrion-run.txt").write_text("".join(transcript) + f"host saw: {seen}\n")
    if controls:
        return False, "control without the launcher failed, so the run proves nothing: " + "; ".join(controls) + f"; see {vm.out}"
    if problems:
        return False, "; ".join(problems) + f"; see {vm.out}"
    return True, f"--no-network curl fails with {error.group(0).strip()!r}; held socket and profile denied; see {vm.out}"


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


def raw_key_slots(image):
    """The 64 key slots of the newest RedoxFS header in a raw image, as bytes."""
    best = None
    with open(image, "rb") as f:
        offset = 0
        while block := f.read(4096):
            if block[:8] == b"RedoxFS\0" and int.from_bytes(block[8:16], "little") == 8:
                generation = int.from_bytes(block[40:48], "little")
                if best is None or generation > best[0]:
                    best = (generation, block)
            offset += 4096
            if offset >= 64 << 20 and best:  # the ring sits at the start of the partition
                break
    if best is None:
        raise RuntimeError(f"no RedoxFS header in {image}")
    data = best[1]
    return [data[KEY_SLOTS_AT + 48 * i:KEY_SLOTS_AT + 48 * (i + 1)] for i in range(64)]


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
    if not vm.wait_serial(rf"RedoxFS password \({attempt}/\d+\)", timeout):
        return False
    sendkeys(vm.command, passphrase + "\n")
    return True


def boot_copy(args, out, make):
    """Boots a copy of an image, given by DISK= and EXTRA_DISK=, that keeps what the guest writes."""
    out.mkdir(parents=True, exist_ok=True)
    return VM(args, out, make=make, snapshot=False)


def root_shell(vm, timeout):
    if not vm.wait_serial(r"login: ", timeout):
        return False
    vm.type("root\n")
    if not vm.wait_serial(r"(?i)password: ", 20):
        return False
    vm.type("password\n")
    return bool(vm.wait_serial(r"root:.*# ", 20))


def run_status(vm, line, timeout=120):
    """Runs a line in the guest's root shell; returns (exit status, output) or (None, output)."""
    since = vm.mark()
    vm.type(f"{line}; echo vm-test-status-$?\n")
    found = vm.wait_serial(r"(?s)\n(.*)vm-test-status-(\d+)", timeout, since)
    if not found:
        return None, vm.text(since)
    return int(found.group(2)), found.group(1)


MKFS_REUSE_MARK = b"FERRION-MKFS-REUSE"


def mkfs_reuse_mount(vm, disk, name, flags=""):
    """Mounts the volume on `disk` at /scheme/<name>; returns (mounted, entry count or None)."""
    run_status(vm, f"bash -c 'redoxfs -d {flags} {disk} {name} </dev/null >/tmp/{name}.log 2>&1 & "
                   "echo $! > /tmp/mr.pid'; sleep 3")
    _, output = run_status(vm, f"bash -c 'if ls /scheme/{name} >/dev/null 2>&1; then echo mr-state-$((40+2))"
                               f" $(ls -A /scheme/{name} | wc -l); else echo mr-state-$((40+1)); fi'")
    found = re.search(r"mr-state-42 +(\d+)|mr-state-41", output)
    if not found or found.group(1) is None:
        run_status(vm, f"cat /tmp/{name}.log")
        return False, None
    return True, int(found.group(1))


def mkfs_reuse_unmount(vm):
    # SIGTERM is only seen after the scheme's next request; every commit is already on the disk
    # (the mount's disk cache writes through), so the daemon is stopped outright.
    run_status(vm, "bash -c 'kill -9 $(cat /tmp/mr.pid)'; sleep 2")


def test_mkfs_reuse(args, out):
    """redoxfs-mkfs over a volume that was mounted and written creates a volume whose next mount is new and empty, and redoxfs-mkfs --encrypt over an unencrypted volume asks for its passphrase (the extra disk, a 64 MiB scratch file)."""
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-vm-test-mkfs-"))
    extra = scratch / "extra.img"
    try:
        extra.write_bytes(MKFS_REUSE_MARK)
        os.truncate(extra, 64 << 20)
        vm = VM(args, out, make=[f"EXTRA_DISK={extra}"])
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

            status, output = run_status(vm, f"redoxfs-mkfs {disk}")
            if status != 0:
                return False, f"the first redoxfs-mkfs failed ({status}): {output.strip()[-200:]}"
            if not mkfs_reuse_mount(vm, disk, "mkfsreuse1")[0]:
                return False, f"the first volume did not mount; see {vm.out}"
            run_status(vm, "bash -c 'echo old-volume > /scheme/mkfsreuse1/old.txt; mkdir /scheme/mkfsreuse1/d; "
                           "for i in 1 2 3 4 5 6 7 8 9 10; do echo $i > /scheme/mkfsreuse1/d/$i; done'")
            mkfs_reuse_unmount(vm)
            status, output = run_status(vm, f"redoxfs-mkfs {disk}")
            if status != 0:
                return False, f"the second redoxfs-mkfs failed ({status}): {output.strip()[-200:]}"
            mounted, entries = mkfs_reuse_mount(vm, disk, "mkfsreuse2")
            if not mounted:
                return False, f"the new volume did not mount (an older header won); see {vm.out}"
            if entries != 0:
                return False, f"the mount shows {entries} entries: the old volume, not the new one"
            run_status(vm, "echo plain-volume > /scheme/mkfsreuse2/plain.txt")
            mkfs_reuse_unmount(vm)

            status, output = run_status(vm, f"printf %s mkfs-reuse-pass > /tmp/mr.key; "
                                            f"redoxfs-mkfs --encrypt --key-file /tmp/mr.key {disk}")
            if status != 0:
                return False, f"redoxfs-mkfs --encrypt failed ({status}): {output.strip()[-200:]}"
            if mkfs_reuse_mount(vm, disk, "mkfsreuse3")[0]:
                return False, "the encrypted volume mounted with no passphrase: an older, unencrypted header won"
            mounted, entries = mkfs_reuse_mount(vm, disk, "mkfsreuse4", "--key-file /tmp/mr.key")
            if not mounted or entries != 0:
                return False, f"the encrypted volume with its passphrase: mounted={mounted}, entries={entries}"
            mkfs_reuse_unmount(vm)
        finally:
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
            since = vm.mark()
            vm.type("redox_installer_tui\n")
            # lived logs "removing handle: ..." on the same console as the installer closes its
            # handles; that line can land between a drive's number and its path.
            choice = vm.wait_serial(r"(\d+): (?:removing handle[^\n]*\n)?\S+: 2\.00 GB", args.timeout, since)
            if not choice or not vm.wait_serial(r"Select a drive", 10, since):
                return False, f"the installer did not offer the 2 GB disk; see {vm.out}"
            vm.type(choice.group(1) + "\n")
            if not vm.wait_serial(r"Encrypt this disk\? \[Y/n\]", 30, since):
                return False, "the installer did not ask about encryption"
            vm.type("\n")
            for prompt in (r"Disk passphrase: ", r"Confirm passphrase: "):
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
                    status, output = run_status(vm, f"grep -c -e encrypt_disk -e {installed} -e {passphrase} /filesystem.toml")
                    if status != 1:
                        return False, f"the installed filesystem.toml holds a passphrase ({status}): {output!r}"
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
        return True, "; ".join(notes) + "; two slots in use; no file name in the raw disk"
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
    """Issue 12: desktop text filters and tar's gzip/directory options work."""
    if not root_shell(vm, args.timeout):
        return False, "no root shell on the serial console"
    script = r'''set -euo pipefail
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
'''
    encoded = base64.b64encode(script.encode()).decode()
    status, output = run_status(
        vm, f"bash -c 'printf %s {encoded} | base64 -d | bash'", timeout=60)
    (vm.out / "cli-tools.txt").write_text(output)
    expected = ("cli-sed-ok", "cli-awk-ok", "cli-tar-ok")
    missing = [marker for marker in expected if not re.search(rf"(?m)^{marker}$", output)]
    if status != 0 or missing:
        return False, f"cli-tools exit {status}; missing {missing}; see {vm.out / 'cli-tools.txt'}"
    return True, "sed substitution, awk arithmetic and tar -czf/-xzf -C passed"


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
        vm = VM(args, boot_out, qemu_flags=peer.qemu_flags(card), make=("net=no",))
        log = None
        try:
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
    vm = VM(args, out, qemu_flags=peer.qemu_flags(card), make=("net=no",))

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


TESTS = {"boot": test_boot, "shells": test_shells, "logout": test_logout, "ls-loop": test_ls_loop,
         "signal-handle": test_signal_handle, "screenshots": test_screenshots,
         "tpm": test_tpm, "lockdown": test_lockdown, "login-namespace": test_login_namespace,
         "dev-parity": test_dev_parity, "firefox-headless": test_firefox_headless, "rand-seed": test_rand_seed,
         "ferrion-run": test_ferrion_run}
TESTS["ferrion-cli"] = test_ferrion_cli
TESTS["update-today"] = test_update_today
TESTS["driver-namespaces"] = test_driver_namespaces
TESTS["scheme-create"] = test_scheme_create
TESTS["nsmgr-bounds"] = test_nsmgr_bounds
TESTS["graphics-bounds"] = test_graphics_bounds
TESTS["gpu-heads"] = test_gpu_heads
TESTS["sudo-notty"] = test_sudo_notty
TESTS["scheme-short-read"] = test_scheme_short_read
TESTS["event-poll"] = test_event_poll
TESTS["desktop-regions"] = test_desktop_regions
TESTS["resolver"] = test_resolver
TESTS["cli-tools"] = test_cli_tools
TESTS["dlopen-ctor"] = test_dlopen_ctor
TESTS["dlopen-tls"] = test_dlopen_tls
TESTS["cond-clock"] = test_cond_clock
TESTS["fd-primitives"] = test_fd_primitives
TESTS["foreign-call"] = test_foreign_call
TESTS["linux-hello"] = test_linux_hello
TESTS["linux-busybox"] = test_linux_busybox
TESTS["linux-dynamic"] = test_linux_dynamic
TESTS["dso-atexit"] = test_dso_atexit
TESTS["gtk-window"] = test_gtk_window
TESTS["gtk-window-wayland"] = test_gtk_window_wayland
# These boot copies of their own images instead of the shared one, and are called with (args, out).
OWN_IMAGE = {"disk-keys": test_disk_keys, "disk-install": test_disk_install}
OWN_IMAGE["wayland-client"] = run_wayland_client
OWN_IMAGE["pointer-latency"] = run_pointer_latency
OWN_IMAGE["mkfs-reuse"] = test_mkfs_reuse
# These start their own boots of the shared image, with a peer on QEMU's network hub.
OWN_IMAGE["net-hardening"] = test_net_hardening
OWN_IMAGE["net-calls"] = test_net_calls
# These boot with their own sound device, recorded by QEMU's wav backend.
OWN_IMAGE["audio-baseline"] = test_audio_baseline
OWN_IMAGE["audio-ring"] = test_audio_ring
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
    if len(sys.argv) == 1:
        parser.print_help()
        sys.exit(2)
    args = parser.parse_args()
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
    for assignment in args.make:
        if not re.match(r"^[A-Za-z_][A-Za-z_0-9]*=", assignment):
            parser.error("--make requires VAR=VALUE, not a make target or option")
        if assignment.split("=", 1)[0] in ("QEMUFLAGS", "QEMU_DISPLAY", "ARCH", "CONFIG_NAME", "BUILD"):
            parser.error("use --arch, --config and --trace; the harness controls QEMUFLAGS, QEMU_DISPLAY and BUILD")
    out_root = args.out or ROOT / "build" / "vm-test" / args.arch / args.config
    failures = blocked = 0
    for name in args.tests:
        for run in range(1, args.repeat + 1):
            out = out_root / f"{name}-{run}"
            out.mkdir(parents=True, exist_ok=True)
            vm = None
            host = {}
            captures = []
            last_capture = None
            trace_lines = 0
            make = ("gpu=multi",) if name == "gpu-heads" else ()
            try:
                qemu, gpu, vk_driver_files, render_node = qemu_settings(args, make)
                host = host_versions(qemu)
                host["vk_driver_files"] = vk_driver_files or None
                host["render_node"] = (render_node or boot_render_node()) if gpu in ("venus", "virtio-gl") else None
                require_3d_host(qemu, gpu, args.arch)
                if name in OWN_IMAGE:
                    ok, note = OWN_IMAGE[name](args, out)
                else:
                    vm = VM(args, out, tpm=name == "tpm", make=make,
                            qemu_flags=RAND_FIRMWARE if name == "rand-seed" else "")
                    ok, note = TESTS[name](vm, args)
                verdict = "PASS" if ok else "FAIL"
            except Blocked as error:
                verdict, note = "BLOCKED", str(error)
            except (RuntimeError, OSError, ValueError, subprocess.SubprocessError) as error:
                verdict, note = "FAIL", str(error)
            finally:
                if vm is not None:
                    vm.close()
                    captures = sorted(vm.capture_sources)
                    last_capture = vm.last_capture_source
                    trace_lines = len(vm.trace())
            failures += verdict == "FAIL"
            blocked += verdict == "BLOCKED"
            result = {"test": name, "run": run, "verdict": verdict, "note": note, "host": host,
                      "make": [*args.make, *make], "capture_sources": captures, "last_capture_source": last_capture,
                      "trace_events": args.trace, "trace_lines": trace_lines}
            (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            print(f"{verdict} {name} {run}/{args.repeat}: {note}", flush=True)
    sys.exit(1 if failures else 2 if blocked else 0)


if __name__ == "__main__":
    main()
