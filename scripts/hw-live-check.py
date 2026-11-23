#!/usr/bin/env python3
"""Host-only first-boot gate; this does not establish hardware safety.

CLI: --kernel ELF --kernel-build BUILD --root ROOT --initfs INITFS
     --tree SOURCE [--tree SOURCE ...].
The caller must supply kernel, bootloader, base, installer and update trees.
BUILD is the kernel recipe build directory containing Cargo's target directory
and kernel.all/kernel, or a Cargo target/profile directory. Exactly one complete
bin-kernel fingerprint must exist. ELF must match a kernel output in BUILD.
Source traversal skips .git and target; build traversal includes target.
Source symlinks must resolve inside the scanned tree; build symlinks, special
files, unreadable inputs and exceeded bounds fail closed. Image symlinks outside
driver/configuration paths are not followed.
ROOT is the assembled filesystem; INITFS is the shipped initfs archive. Its
bytes must match ROOT/usr/lib/boot/initfs; archive inspection never extracts.

Callable API: check(kernel, kernel_build, trees, root, initfs), with Path
arguments and a list of Path trees, returns an nm-availability status string or
raises GateError, OSError, ValueError or a subprocess error.
main(argv=None) returns zero on acceptance, one on rejection;
argparse errors return two. Fault probes must plant independent copies of these
inputs; there is no bypass/probe flag.
"""

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import stat
import struct
import subprocess
import sys
import time
import tomllib

MAX_TREES = 16
MAX_DEPTH = 64
MAX_ENTRIES = 100_000
MAX_SOURCE_BYTES = 8 * 1024 * 1024
MAX_TOTAL_SOURCE_BYTES = 512 * 1024 * 1024
MAX_METADATA_BYTES = 256 * 1024
MAX_ELF_BYTES = 256 * 1024 * 1024
MAX_BUILD_OUTPUTS = 16
MAX_REFERENCES = 1024
MAX_NM_BYTES = 32 * 1024 * 1024
MAX_NM_READS = 131_072
NM_TIMEOUT = 30
CHUNK_BYTES = 64 * 1024
MAX_IMAGE_BYTES = 4 * 1024 * 1024 * 1024
MAX_INITFS_BYTES = 256 * 1024 * 1024
MAX_INITFS_INODES = 65535
MAX_NAME_BYTES = 255
INITFS_HEADER = struct.Struct("<8sIQHHHQ")
INITFS_INODE = struct.Struct("<III")
INITFS_DIRENT = struct.Struct("<HHI")
HARDWARE_POLICY = b"FERRION_HW_LIVE=1\nSTORAGE=off\nQUIET=0\n"
FORBIDDEN_PROGRAMS = frozenset((
    "nvmed", "ahcid", "ided", "virtio-blkd", "usbscsid", "bcm2835-sdhcid",
    "nbdd", "virtio-fsd", "installer", "redox_installer", "redox-installer",
))
ALLOWED_DRIVERS = frozenset(("xhcid", "usbhidd", "usbhubd", "usbctl"))
PCI_CONFIG = {"drivers": [{
    "name": "XHCI", "class": 12, "subclass": 3, "interface": 48,
    "command": ["xhcid"], "use_channel": True,
}]}
USB_CONFIG = {"drivers": [
    {"name": "USB HUB", "class": 9, "subclass": -1,
     "command": ["usbhubd", "$SCHEME", "$PORT", "$IF_NUM"]},
    {"name": "USB HID", "class": 3, "subclass": -1,
     "command": ["usbhidd", "$SCHEME", "$PORT", "$IF_NUM"]},
    {"name": "USB HID Xbox 360", "class": 255, "subclass": 93, "protocol": 1,
     "ids": {"0x045e": [0x028e]},
     "command": ["usbhidd", "$SCHEME", "$PORT", "$IF_NUM", "xbox-360"]},
]}
FORBIDDEN_FEATURES = frozenset(("system76_ec_debug", "lpss_debug"))
FORBIDDEN_SYMBOLS = re.compile(rb"system76_ec|lpss_debug")
VARIABLE_NAME = re.compile(r"\b(?:SetVariable|set_variable)\b")
FINGERPRINT_DIRECTORY = re.compile(r"kernel-([0-9a-f]{16})\Z")
BUILD_OUTPUT = re.compile(r"(?:kernel(?:\.all)?|kernel-[0-9a-f]{16})\Z")
LEXICAL_NOISE = re.compile(
    r'//[^\n]*|/\*.*?\*/|(?:br|cr|r)(?P<hashes>\#{0,255})".*?"(?P=hashes)'
    r'|"(?:\\.|[^"\\])*"|\'(?:\\(?:[^\n]|u\{[0-9a-fA-F]+\})|[^\'\\\n])\'',
    re.DOTALL,
)
RUST_FUNCTION_FIELD = re.compile(r"\s*:\s*(?:unsafe\s+)?(?:extern\s+)?fn\s*\(")
C_FIELD_PREFIX = re.compile(r"\s*[A-Z][A-Z0-9_]*\s+\Z")


class GateError(Exception):
    pass


def signature(info):
    return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


@contextmanager
def open_path(path, directory=False):
    parts = Path(os.path.abspath(path)).parts[1:]
    if len(parts) > MAX_DEPTH:
        raise GateError(f"path exceeds depth bound: {path}")
    fd = os.open(os.path.sep, os.O_RDONLY | os.O_DIRECTORY)
    try:
        for index, part in enumerate(parts):
            flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
            if index < len(parts) - 1 or directory:
                flags |= os.O_DIRECTORY
            child = os.open(part, flags, dir_fd=fd)
            os.close(fd)
            fd = child
        info = os.fstat(fd)
        expected = stat.S_ISDIR if directory else stat.S_ISREG
        if not expected(info.st_mode):
            raise GateError(f"unexpected input type: {path}")
        yield fd
    finally:
        os.close(fd)


def read_file(parent, name, limit):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    try:
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
            raise GateError(f"nonregular or oversized file: {name}")
        with os.fdopen(os.dup(fd), "rb") as stream:
            data = stream.read(limit + 1)
        if len(data) > limit or len(data) != before.st_size or signature(before) != signature(os.fstat(fd)):
            raise GateError(f"file changed or exceeded bound: {name}")
        return data
    finally:
        os.close(fd)


def walk_files(root, source):
    count = [0]
    skipped = {".git", "target"} if source else {".git", "vendor"}
    source_root = Path(root).resolve(strict=True)

    def visit(parent, parts):
        if len(parts) > MAX_DEPTH:
            raise GateError(f"tree exceeds depth bound: {root}")
        with os.scandir(parent) as entries:
            for entry in entries:
                count[0] += 1
                if count[0] > MAX_ENTRIES:
                    raise GateError(f"tree exceeds entry bound: {root}")
                info = entry.stat(follow_symlinks=False)
                if entry.name in skipped and stat.S_ISDIR(info.st_mode):
                    continue
                relative = parts + (entry.name,)
                if stat.S_ISDIR(info.st_mode):
                    child = os.open(entry.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
                    try:
                        yield from visit(child, relative)
                    finally:
                        os.close(child)
                elif stat.S_ISREG(info.st_mode):
                    yield parent, relative
                elif source and stat.S_ISLNK(info.st_mode):
                    resolved = source_root.joinpath(*relative).resolve(strict=True)
                    try:
                        target = resolved.relative_to(source_root)
                    except ValueError:
                        raise GateError(f"source symlink leaves the tree: {root.joinpath(*relative)}") from None
                    if any(part in skipped for part in target.parts):
                        raise GateError(f"source symlink enters an unscanned directory: {root.joinpath(*relative)}")
                else:
                    raise GateError(f"symlink or special file in tree: {root.joinpath(*relative)}")

    with open_path(root, directory=True) as fd:
        yield from visit(fd, ())


def elf_digest(fd):
    before = os.fstat(fd)
    if not stat.S_ISREG(before.st_mode) or not 4 <= before.st_size <= MAX_ELF_BYTES:
        raise GateError("kernel ELF is nonregular, empty or oversized")
    os.lseek(fd, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    remaining = before.st_size
    for index in range(MAX_ELF_BYTES // CHUNK_BYTES + 1):
        if remaining == 0:
            break
        chunk = os.read(fd, min(CHUNK_BYTES, remaining))
        if not chunk or (index == 0 and not chunk.startswith(b"\x7fELF")):
            raise GateError("kernel input is not a complete ELF")
        digest.update(chunk)
        remaining -= len(chunk)
    if remaining or signature(before) != signature(os.fstat(fd)):
        raise GateError("kernel ELF changed while reading")
    return digest.digest()


def parse_feature_list(record, key):
    value = record.get(key)
    if not isinstance(value, str):
        raise GateError(f"Cargo fingerprint lacks encoded {key}")
    values = json.loads(value)
    if not isinstance(values, list) or len(values) > MAX_REFERENCES or not all(isinstance(item, str) for item in values):
        raise GateError(f"invalid Cargo fingerprint {key}")
    if len(set(values)) != len(values):
        raise GateError(f"duplicate Cargo fingerprint {key}")
    return set(values)


def check_build(build, kernel_hash):
    records = 0
    outputs = 0
    matched = False
    for parent, parts in walk_files(build, source=False):
        name = parts[-1]
        if BUILD_OUTPUT.fullmatch(name):
            outputs += 1
            if outputs > MAX_BUILD_OUTPUTS:
                raise GateError("too many kernel output candidates")
            fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
            try:
                matched |= elf_digest(fd) == kernel_hash
            finally:
                os.close(fd)
        if name != "bin-kernel.json":
            continue
        if len(parts) < 3 or parts[-3] != ".fingerprint" or not FINGERPRINT_DIRECTORY.fullmatch(parts[-2]):
            raise GateError("kernel feature record is outside a Cargo fingerprint directory")
        records += 1
        if records > 1:
            raise GateError("ambiguous kernel Cargo fingerprints; use a clean build directory")
        record = json.loads(read_file(parent, name, MAX_METADATA_BYTES))
        if not isinstance(record, dict):
            raise GateError("invalid kernel Cargo fingerprint object")
        for field in ("rustc", "target", "profile", "path", "compile_kind"):
            if type(record.get(field)) is not int:
                raise GateError(f"kernel Cargo fingerprint lacks {field}")
        features = parse_feature_list(record, "features")
        declared = parse_feature_list(record, "declared_features")
        if not features <= declared or not FORBIDDEN_FEATURES <= declared:
            raise GateError("kernel Cargo feature declarations are incomplete or inconsistent")
        forbidden = features & FORBIDDEN_FEATURES
        if forbidden:
            raise GateError("forbidden enabled kernel features: " + ", ".join(sorted(forbidden)))
        marker = read_file(parent, "bin-kernel", MAX_METADATA_BYTES).strip()
        if not re.fullmatch(rb"[0-9a-f]{16}", marker):
            raise GateError("kernel Cargo fingerprint completion marker is invalid")
        if not read_file(parent, "dep-bin-kernel", MAX_METADATA_BYTES):
            raise GateError("kernel Cargo dependency record is empty")
        local = record.get("local")
        if not isinstance(local, list) or not 1 <= len(local) <= MAX_REFERENCES:
            raise GateError("kernel Cargo local dependency metadata is missing")
        expected_suffix = "/".join(parts[-3:-1]) + "/dep-bin-kernel"
        if not any(
            isinstance(item, dict)
            and isinstance(item.get("CheckDepInfo"), dict)
            and isinstance(item["CheckDepInfo"].get("dep_info"), str)
            and ("/" + item["CheckDepInfo"]["dep_info"]).endswith("/" + expected_suffix)
            for item in local
        ):
            raise GateError("kernel Cargo dependency metadata does not identify this fingerprint")
    if records != 1:
        raise GateError("no trustworthy kernel Cargo feature record")
    if not matched:
        raise GateError("supplied ELF does not match a kernel build output")


def check_symbols(fd):
    nm = shutil.which("nm")
    if nm is None:
        return "nm unavailable; symbol check skipped"
    descriptor = f"/proc/self/fd/{fd}"
    if not os.path.exists(descriptor):
        descriptor = f"/dev/fd/{fd}"
    before = signature(os.fstat(fd))
    process = subprocess.Popen([nm, "-a", "-C", descriptor], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, pass_fds=(fd,))
    assert process.stdout is not None
    deadline = time.monotonic() + NM_TIMEOUT
    total = 0
    tail = b""
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            for _ in range(MAX_NM_READS):
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not selector.select(remaining):
                    raise GateError("nm exceeded time bound")
                chunk = os.read(process.stdout.fileno(), CHUNK_BYTES)
                if not chunk:
                    break
                total += len(chunk)
                if total > MAX_NM_BYTES:
                    raise GateError("nm exceeded output bound")
                if FORBIDDEN_SYMBOLS.search(tail + chunk):
                    raise GateError("forbidden kernel debug symbol in ELF")
                tail = chunk[-64:]
            else:
                raise GateError("nm exceeded read bound")
        status = process.wait(timeout=max(0.001, deadline - time.monotonic()))
        if status != 0 or total == 0 or signature(os.fstat(fd)) != before:
            raise GateError("nm failed, returned no symbols or observed a changing ELF")
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=NM_TIMEOUT)
        process.stdout.close()
    return "nm symbol check passed"


def scan_source(data, label):
    text = data.decode("utf-8", errors="surrogateescape")
    if not VARIABLE_NAME.search(text):
        return
    clean = LEXICAL_NOISE.sub(lambda match: " " * len(match.group()), text)
    for index, match in enumerate(VARIABLE_NAME.finditer(clean)):
        if index >= MAX_REFERENCES:
            raise GateError(f"firmware reference bound exceeded: {label}")
        start, end = match.span()
        if RUST_FUNCTION_FIELD.match(clean, end):
            continue
        line_start = clean.rfind("\n", 0, start) + 1
        if C_FIELD_PREFIX.fullmatch(clean[line_start:start]) and re.match(r"\s*;", clean[end:]):
            continue
        line = text.count("\n", 0, start) + 1
        raise GateError(f"UEFI variable call or suspicious reference: {label}:{line}: {match.group()}")


def check_image(root):
    root = Path(root)
    with open_path(root, directory=True) as fd:
        if read_file(fd, "hw-live", MAX_METADATA_BYTES) != HARDWARE_POLICY:
            raise GateError("image /hw-live policy differs from required fixed bytes")
    driver_path = ("usr", "lib", "drivers")
    pci_path = ("usr", "lib", "pcid.d")
    usb_path = ("usr", "lib", "xhcid.d")
    configs = {
        pci_path + ("xhcid.toml",): PCI_CONFIG,
        usb_path + ("drivers.toml",): USB_CONFIG,
    }
    required_dirs = [driver_path, pci_path, usb_path]
    for parts in required_dirs:
        with open_path(root.joinpath(*parts), directory=True):
            pass
    seen_configs = set()
    counts = [0, 0]

    def visit(parent, parts):
        if len(parts) > MAX_DEPTH:
            raise GateError(f"image exceeds depth bound: {root}")
        with os.scandir(parent) as entries:
            for entry in entries:
                counts[0] += 1
                if counts[0] > MAX_ENTRIES:
                    raise GateError(f"image exceeds entry bound: {root}")
                relative = parts + (entry.name,)
                label = root.joinpath(*relative)
                info = entry.stat(follow_symlinks=False)
                if entry.name in FORBIDDEN_PROGRAMS:
                    raise GateError(f"forbidden image program: {label}")
                in_drivers = "drivers" in parts
                in_configs = any(part in ("pcid.d", "xhcid.d") for part in parts)
                if in_drivers and (
                    parts != driver_path or entry.name not in ALLOWED_DRIVERS
                    or not stat.S_ISREG(info.st_mode) or not info.st_mode & 0o111
                ):
                    raise GateError(f"unexpected image driver: {label}")
                if in_configs and relative not in configs:
                    raise GateError(f"unexpected image driver configuration: {label}")
                if relative in configs:
                    if not stat.S_ISREG(info.st_mode):
                        raise GateError(f"nonregular image driver configuration: {label}")
                    data = read_file(parent, entry.name, MAX_METADATA_BYTES)
                    config = tomllib.loads(data.decode("utf-8"))
                    drivers = config.get("drivers", [])
                    if not isinstance(drivers, list) or len(drivers) > MAX_REFERENCES:
                        raise GateError(f"invalid driver configuration: {label}")
                    for driver in drivers:
                        if not isinstance(driver, dict):
                            raise GateError(f"invalid driver entry: {label}")
                        if driver.get("class") == (1 if "pcid.d" in parts else 8):
                            raise GateError(f"storage class in image driver configuration: {label}")
                    if config != configs[relative]:
                        raise GateError(f"image driver configuration is not allowlisted: {label}")
                    seen_configs.add(relative)
                if stat.S_ISDIR(info.st_mode):
                    if entry.name in ("drivers", "pcid.d", "xhcid.d") and relative not in required_dirs:
                        raise GateError(f"unexpected image driver directory: {label}")
                    child = os.open(entry.name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
                    try:
                        visit(child, relative)
                    finally:
                        os.close(child)
                elif stat.S_ISREG(info.st_mode):
                    counts[1] += info.st_size
                    if info.st_size > MAX_ELF_BYTES or counts[1] > MAX_IMAGE_BYTES:
                        raise GateError(f"image exceeds file or total byte bound: {label}")
                elif stat.S_ISLNK(info.st_mode):
                    if entry.name in ("drivers", "pcid.d", "xhcid.d"):
                        raise GateError(f"symlinked image driver directory: {label}")
                else:
                    raise GateError(f"special file in image: {label}")

    with open_path(root, directory=True) as fd:
        visit(fd, ())
    if seen_configs != set(configs):
        raise GateError(f"required image driver configuration missing: {root}")


def read_archive(path):
    path = Path(os.path.abspath(path))
    with open_path(path.parent, directory=True) as fd:
        return read_file(fd, path.name, MAX_INITFS_BYTES)


def check_initfs(initfs, root):
    data = read_archive(initfs)
    shipped = Path(root) / "usr/lib/boot/initfs"
    if os.path.abspath(initfs) != os.path.abspath(shipped) and data != read_archive(shipped):
        raise GateError("supplied initfs does not match the shipped archive")
    if len(data) < INITFS_HEADER.size:
        raise GateError("truncated initfs header")
    magic, table, size, page_size, root_inode, inode_count, bootstrap = INITFS_HEADER.unpack_from(data)
    if (
        magic != b"RedoxFtw" or size != len(data) or page_size != 4096
        or not bootstrap or not 1 <= inode_count <= MAX_INITFS_INODES
        or root_inode >= inode_count or table < INITFS_HEADER.size
        or table + inode_count * INITFS_INODE.size > len(data)
    ):
        raise GateError("invalid initfs header or inode table")
    seen = set()
    found_dirs = set()
    found_config = False
    total = 0

    def span(offset, length):
        if offset < INITFS_HEADER.size or offset + length > len(data):
            raise GateError("initfs extent is outside archive bounds")
        return memoryview(data)[offset:offset + length]

    def visit(inode, parts):
        nonlocal found_config, total
        if len(parts) > MAX_DEPTH or len(seen) >= MAX_ENTRIES:
            raise GateError("initfs exceeds traversal bounds")
        if inode >= inode_count or inode in seen:
            raise GateError("initfs has an invalid, repeated or cyclic inode")
        seen.add(inode)
        kind, length, offset = INITFS_INODE.unpack_from(data, table + inode * INITFS_INODE.size)
        contents = span(offset, length)
        if kind not in (0, 1, 2, 3) or (not parts and kind != 2):
            raise GateError("invalid initfs inode type")
        label = "/" + "/".join(parts)
        if parts and parts[-1] in FORBIDDEN_PROGRAMS:
            raise GateError(f"forbidden initfs program: {label}")
        if "drivers" in parts[:-1]:
            raise GateError(f"unexpected initfs driver: {label}")
        if any(part in ("pcid.d", "xhcid.d") for part in parts[:-1]):
            if parts != ("lib", "pcid.d", "initfs.toml") or kind != 0 or length > MAX_METADATA_BYTES:
                raise GateError(f"unexpected initfs driver configuration: {label}")
            if tomllib.loads(bytes(contents).decode("utf-8")) != {}:
                raise GateError("initfs PCI configuration must be empty")
            found_config = True
        if parts and parts[-1] in ("drivers", "pcid.d", "xhcid.d"):
            if kind != 2 or parts not in (("lib", "drivers"), ("lib", "pcid.d")):
                raise GateError(f"unexpected initfs driver directory: {label}")
            found_dirs.add(parts)
        if kind == 2:
            if length % INITFS_DIRENT.size or length // INITFS_DIRENT.size > MAX_ENTRIES:
                raise GateError("invalid initfs directory extent")
            names = set()
            for index in range(length // INITFS_DIRENT.size):
                child, name_length, name_offset = INITFS_DIRENT.unpack_from(contents, index * INITFS_DIRENT.size)
                if not 1 <= name_length <= MAX_NAME_BYTES:
                    raise GateError("invalid initfs entry name length")
                name = bytes(span(name_offset, name_length)).decode("utf-8")
                if name in (".", "..") or "/" in name or "\0" in name or name in names:
                    raise GateError("unsafe or duplicate initfs entry name")
                names.add(name)
                visit(child, parts + (name,))
        else:
            total += length
            if length > MAX_ELF_BYTES or total > MAX_IMAGE_BYTES:
                raise GateError("initfs content exceeds byte bounds")
            if kind == 3 and (length > MAX_NAME_BYTES * MAX_DEPTH or 0 in contents):
                raise GateError("invalid initfs symbolic link")

    visit(root_inode, ())
    if len(seen) != inode_count:
        raise GateError("initfs contains unreachable inodes")
    if found_dirs != {("lib", "drivers"), ("lib", "pcid.d")} or not found_config:
        raise GateError("initfs lacks required empty driver directory or PCI configuration")


def check(kernel, kernel_build, trees, root, initfs):
    kernel = Path(kernel)
    kernel_build = Path(kernel_build)
    if not 1 <= len(trees) <= MAX_TREES:
        raise GateError(f"supply between 1 and {MAX_TREES} source trees")
    check_initfs(initfs, root)
    check_image(root)
    with open_path(kernel) as fd:
        kernel_hash = elf_digest(fd)
        check_build(kernel_build, kernel_hash)
        symbol_status = check_symbols(fd)
        if elf_digest(fd) != kernel_hash:
            raise GateError("kernel changed during gate")
    total = 0
    for tree in trees:
        root = Path(tree)
        files = 0
        for parent, parts in walk_files(root, source=True):
            data = read_file(parent, parts[-1], MAX_SOURCE_BYTES)
            total += len(data)
            files += 1
            if total > MAX_TOTAL_SOURCE_BYTES:
                raise GateError("source trees exceed cumulative byte bound")
            scan_source(data, root.joinpath(*parts))
        if files == 0:
            raise GateError(f"source tree contains no scanned files: {root}")
    return symbol_status


def main(argv=None):
    parser = argparse.ArgumentParser(description="Gate first-boot inputs using kernel build metadata, ELF symbols and source scans.")
    parser.add_argument("--kernel", required=True, type=Path, help="kernel ELF to inspect")
    parser.add_argument("--kernel-build", required=True, type=Path, help="kernel build directory containing Cargo fingerprints and kernel output")
    parser.add_argument("--tree", required=True, action="append", type=Path, help="source tree to scan; repeat for every required component")
    parser.add_argument("--root", required=True, type=Path, help="assembled filesystem staging directory")
    parser.add_argument("--initfs", required=True, type=Path, help="shipped initfs archive; must match ROOT/usr/lib/boot/initfs")
    args = parser.parse_args(argv)
    try:
        status = check(args.kernel, args.kernel_build, args.tree, args.root, args.initfs)
    except (GateError, OSError, ValueError, RecursionError, subprocess.SubprocessError) as error:
        print(f"hw-live-check: rejected: {error}", file=sys.stderr)
        return 1
    print(f"hw-live-check: accepted; {status}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
