#!/usr/bin/env python3
import argparse
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import shutil
import stat
import struct
import subprocess
import sys
import tempfile

FAT_MAX_FILE = (1 << 32) - 1
MIB = 1 << 20
MIN_IMAGE_BYTES = 128 * MIB
MAX_IMAGE_BYTES = 16 << 30
MAX_DEVICE_BYTES = 2 << 40
MAX_BLOCK_NODES = 4096
MAX_TOOL_OUTPUT = 4 * MIB
TOOL_TIMEOUT_SECONDS = 300
COPY_CHUNK = MIB
BLKGETSIZE64 = 0x80081272
BLKROGET = 0x125E
SYSTEM_PATHS = ("/", "/boot", "/boot/efi", "/usr", "/var", "/home")


def run_tool(arguments, pass_fds=()):
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith("MTOOLS")}
    environment.update(LC_ALL="C", MTOOLSRC="/dev/null")
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        result = subprocess.run(arguments, stdin=subprocess.DEVNULL, stdout=output,
                                stderr=errors, env=environment,
                                timeout=TOOL_TIMEOUT_SECONDS, pass_fds=pass_fds,
                                check=False)
        if output.tell() > MAX_TOOL_OUTPUT or errors.tell() > MAX_TOOL_OUTPUT:
            raise ValueError("Host tool output exceeds the size limit")
        if result.returncode:
            errors.seek(0)
            detail = errors.read(4096).decode(errors="replace").strip()
            raise ValueError(f"{arguments[0]} failed: {detail}")
        output.seek(0)
        return output.read(MAX_TOOL_OUTPUT).decode("utf-8")


def require_tools(names):
    for name in names:
        if shutil.which(name) is None:
            raise ValueError(f"Required host tool not found: {name}")


def input_file(stack, path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
    stack.callback(os.close, descriptor)
    metadata = os.fstat(descriptor)
    if not stat.S_ISREG(metadata.st_mode) or not 0 < metadata.st_size <= FAT_MAX_FILE:
        raise ValueError(f"Input must be a nonempty regular file of at most {FAT_MAX_FILE} bytes: {path}")
    return descriptor, metadata


def stage_image(bootloader, image, destination):
    require_tools(("mkfs.fat", "mmd", "mcopy"))
    with ExitStack() as stack:
        boot_fd, boot_stat = input_file(stack, bootloader)
        image_fd, image_stat = input_file(stack, image)
        payload = boot_stat.st_size + image_stat.st_size
        size = max(MIN_IMAGE_BYTES, ((payload + payload // 16 + 64 * MIB + MIB - 1) // MIB) * MIB)
        if size > MAX_IMAGE_BYTES:
            raise ValueError("FAT image exceeds the staging size limit")
        with open(destination, "xb") as output:
            output.truncate(size)
        run_tool(["mkfs.fat", "-F", "32", "-n", "FERRION", str(destination)])
        run_tool(["mmd", "-i", str(destination), "::/EFI", "::/EFI/BOOT"])
        for descriptor, expected, target in (
                (boot_fd, boot_stat, "::/EFI/BOOT/BOOTX64.EFI"),
                (image_fd, image_stat, "::/redox-live.iso")):
            run_tool(["mcopy", "-i", str(destination), f"/proc/self/fd/{descriptor}", target],
                     pass_fds=(descriptor,))
            actual = os.fstat(descriptor)
            if (actual.st_size, actual.st_mtime_ns, actual.st_ctime_ns) != (
                    expected.st_size, expected.st_mtime_ns, expected.st_ctime_ns):
                raise ValueError("Input changed while staging the FAT image")
        with open(destination, "rb") as output:
            os.fsync(output.fileno())
        return size


def block_inventory():
    raw = run_tool(["lsblk", "--json", "--bytes", "--paths", "--tree", "--output",
                    "NAME,TYPE,RM,TRAN,MOUNTPOINTS,MAJ:MIN,SIZE,RO"])
    document = json.loads(raw)
    pending = document.get("blockdevices")
    if not isinstance(pending, list) or not pending:
        raise ValueError("lsblk did not report a block-device tree")
    pending = [(node, ()) for node in pending]
    nodes = []
    for _ in range(MAX_BLOCK_NODES):
        if not pending:
            return nodes
        node, ancestors = pending.pop()
        if not isinstance(node, dict):
            raise ValueError("Invalid lsblk device record")
        required = ("name", "type", "rm", "tran", "mountpoints", "maj:min", "size", "ro")
        if any(key not in node for key in required):
            raise ValueError("Incomplete lsblk device record")
        if not isinstance(node["mountpoints"], list):
            raise ValueError("lsblk mount information is unavailable")
        identity = node["maj:min"]
        if not isinstance(identity, str) or len(identity.split(":")) != 2:
            raise ValueError("Invalid lsblk device identity")
        major, minor = identity.split(":")
        if not major.isdecimal() or not minor.isdecimal():
            raise ValueError("Invalid lsblk device identity")
        nodes.append((node, ancestors))
        children = node.get("children", [])
        if not isinstance(children, list) or len(children) + len(pending) > MAX_BLOCK_NODES:
            raise ValueError("lsblk device tree exceeds the node limit")
        pending.extend((child, ancestors + (identity,)) for child in children)
    raise ValueError("lsblk device tree exceeds the node limit")


def validate_device(device):
    metadata = os.stat(device, follow_symlinks=False)
    if not stat.S_ISBLK(metadata.st_mode):
        raise ValueError("Target is not a block device")
    identity = f"{os.major(metadata.st_rdev)}:{os.minor(metadata.st_rdev)}"
    nodes = block_inventory()
    matches = [node for node, _ in nodes if node["name"] == str(device)]
    if len(matches) != 1:
        raise ValueError("Target must appear exactly once in lsblk")
    target = matches[0]
    if (target["maj:min"] != identity or target["type"] != "disk"
            or target["rm"] not in (True, 1) or target["tran"] != "usb"
            or target["ro"] not in (False, 0)):
        raise ValueError("Target must be a writable, removable USB whole disk")
    size = target["size"]
    if not isinstance(size, int) or not MIN_IMAGE_BYTES <= size <= MAX_DEVICE_BYTES:
        raise ValueError("Device capacity is outside the supported range")
    system_ids = {
        node["maj:min"] for node, _ in nodes
        if any(mount in SYSTEM_PATHS for mount in node["mountpoints"])
    }
    root_ids = {node["maj:min"] for node, _ in nodes if "/" in node["mountpoints"]}
    if len(root_ids) != 1:
        raise ValueError("Cannot resolve the host root device; refusing preparation")
    for node, ancestors in nodes:
        if node["maj:min"] == identity or identity in ancestors:
            if any(mount is not None and mount != "" for mount in node["mountpoints"]):
                raise ValueError("Target or a descendant is mounted or used as swap")
            if node["maj:min"] in system_ids:
                raise ValueError("Target is an ancestor of a system filesystem")
            if node["type"] not in ("disk", "part"):
                raise ValueError("Target has an active block-device holder")
    return metadata.st_rdev, size


def create_image(args):
    output = Path(args.output).absolute()
    if os.path.lexists(output):
        raise ValueError("Output already exists; choose a new path")
    with tempfile.TemporaryDirectory(prefix="agent-hw-stick-", dir=output.parent) as scratch:
        staged = Path(scratch) / "stick.img"
        size = stage_image(args.bootloader, args.image, staged)
        os.link(staged, output)
    print(f"Created {output} ({size} bytes)")


def prepare(args):
    require_tools(("lsblk", "mkfs.fat", "mmd", "mcopy"))
    device = Path(args.device).resolve(strict=True)
    if not str(device).startswith("/dev/"):
        raise ValueError("Device must resolve to a path under /dev")
    if args.confirm_erase != str(device):
        raise ValueError(f"Erasing this disk requires --confirm-erase {device}")
    expected_id, expected_size = validate_device(device)
    with tempfile.TemporaryDirectory(prefix="agent-hw-stick-") as scratch:
        staged = Path(scratch) / "stick.img"
        size = stage_image(args.bootloader, args.image, staged)
        if size > expected_size - MIB:
            raise ValueError("FAT image does not fit the device with signature-clearing space")
        descriptor = os.open(device, os.O_RDWR | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC)
        try:
            opened = os.fstat(descriptor)
            if not stat.S_ISBLK(opened.st_mode) or opened.st_rdev != expected_id:
                raise ValueError("Device identity changed before opening")
            actual_size = struct.unpack("=Q", fcntl.ioctl(descriptor, BLKGETSIZE64, bytes(8)))[0]
            readonly = struct.unpack("=I", fcntl.ioctl(descriptor, BLKROGET, bytes(4)))[0]
            if actual_size != expected_size or readonly:
                raise ValueError("Device capacity or write access changed")
            if validate_device(device) != (expected_id, expected_size):
                raise ValueError("Device identity changed before writing")
            print(f"Erasing {device} and copying the FAT boot filesystem", flush=True)
            zeros = bytes(MIB)
            if os.pwrite(descriptor, zeros, expected_size - MIB) != MIB:
                raise OSError("Short write while clearing old disk signatures")
            with open(staged, "rb") as source:
                for offset in range(0, size, COPY_CHUNK):
                    expected = min(COPY_CHUNK, size - offset)
                    data = source.read(expected)
                    if len(data) != expected or os.pwrite(descriptor, data, offset) != expected:
                        raise OSError("Short read or write while copying the FAT filesystem")
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    print(f"Prepared {device}; safely eject it before unplugging")


def main():
    parser = argparse.ArgumentParser(
        description="Stage FerrionOS first-boot files in a FAT32 superfloppy filesystem.",
        epilog="Requires Linux, dosfstools (mkfs.fat), and mtools (mmd/mcopy). "
               "The boot files are EFI/BOOT/BOOTX64.EFI and /redox-live.iso. "
               "Each input must fit FAT's 4 GiB minus 1 byte file limit.")
    commands = parser.add_subparsers(dest="command")
    create = commands.add_parser("create-image", help="create a regular-file FAT image without root")
    disk = commands.add_parser("prepare", help="erase a removable USB whole disk; requires device write access")
    for command in (create, disk):
        command.add_argument("--bootloader", required=True, help="EFI bootloader file")
        command.add_argument("--image", required=True, help="live image file")
    create.add_argument("--output", required=True, help="new regular-file output path (never overwritten)")
    disk.add_argument("--device", required=True, help="removable USB whole disk under /dev")
    disk.add_argument("--confirm-erase", required=True, metavar="CANONICAL_DEVICE",
                      help="repeat the canonical device path to authorize destruction of all disk contents")
    args = parser.parse_args()
    if args.command is None:
        parser.print_help()
        return 0
    try:
        if args.command == "create-image":
            create_image(args)
        else:
            prepare(args)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"hw-stick: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
