#!/usr/bin/env python3
import argparse
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import termios
import tomllib

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
ROOT = Path(__file__).resolve().parents[1]
ACCOUNTS = ROOT / "config/my_hw-live-accounts.toml"
MAX_ACCOUNT_FILE_BYTES = 4096
MAX_ACCOUNT_NAME_BYTES = 32
MAX_PASSWORD_BYTES = 255
MAX_HASH_BYTES = 512
ACCOUNT_UID = 1000


def prompt_value(prompt, hidden=False, limit=MAX_PASSWORD_BYTES):
    descriptor = os.open("/dev/tty", os.O_RDWR | os.O_CLOEXEC)
    settings = termios.tcgetattr(descriptor)
    try:
        if hidden:
            private = termios.tcgetattr(descriptor)
            private[3] &= ~termios.ECHO
            termios.tcsetattr(descriptor, termios.TCSAFLUSH, private)
        os.write(descriptor, prompt.encode())
        with os.fdopen(os.dup(descriptor), "r", encoding="utf-8") as terminal:
            value = terminal.readline(limit + 2)
        if not value.endswith("\n") or len(value[:-1].encode("utf-8")) > limit:
            raise ValueError(f"Input exceeds {limit} UTF-8 bytes")
        return value[:-1]
    finally:
        try:
            termios.tcsetattr(descriptor, termios.TCSAFLUSH, settings)
            if hidden:
                os.write(descriptor, b"\n")
        finally:
            os.close(descriptor)


def valid_account_name(name):
    return isinstance(name, str) and re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", name) is not None


def read_accounts():
    try:
        descriptor = os.open(ACCOUNTS, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    except FileNotFoundError:
        raise ValueError("Create an account with hw-stick.py account, or explicitly use --no-account") from None
    with os.fdopen(descriptor, "rb") as source:
        metadata = os.fstat(source.fileno())
        if not stat.S_ISREG(metadata.st_mode) or stat.S_IMODE(metadata.st_mode) != 0o600 or metadata.st_nlink != 1:
            raise ValueError("Private account configuration must be a regular file with mode 0600")
        raw = source.read(MAX_ACCOUNT_FILE_BYTES + 1)
    if len(raw) > MAX_ACCOUNT_FILE_BYTES:
        raise ValueError("Private account configuration exceeds the size limit")
    try:
        data = tomllib.loads(raw.decode("utf-8"))
    except (UnicodeError, tomllib.TOMLDecodeError):
        raise ValueError("Invalid private account configuration") from None
    users = data.get("users", {})
    if not isinstance(users, dict) or len(users) != 1:
        raise ValueError("Private account configuration must contain exactly one account")
    name, user = next(iter(users.items()))
    if not isinstance(user, dict):
        raise ValueError("Invalid private account configuration")
    encoded = user.get("password_hash", "")
    if (not valid_account_name(name) or name == "root" or user.get("uid") != ACCOUNT_UID
            or user.get("gid") != ACCOUNT_UID or "password" in user
            or not isinstance(encoded, str) or not encoded.startswith("$argon2")
            or len(encoded) > MAX_HASH_BYTES or "\n" in encoded):
        raise ValueError("Private account configuration requires a hashed password and uid/gid 1000")
    return name, encoded


def create_account():
    if os.path.lexists(ACCOUNTS):
        raise ValueError("Private account configuration already exists; it was not changed")
    name = prompt_value("Account name: ", limit=MAX_ACCOUNT_NAME_BYTES)
    if not valid_account_name(name) or name == "root":
        raise ValueError("Use a non-root account name of up to 32 lowercase letters, digits, underscores or hyphens")
    password = prompt_value("Password: ", hidden=True)
    if not password:
        raise ValueError("Password must not be empty")
    confirmation = prompt_value("Confirm password: ", hidden=True)
    if password != confirmation:
        raise ValueError("Passwords do not match")
    result = subprocess.run([str(ROOT / "build/fstools/bin/redox_installer"), "--hash-password"],
                            input=password.encode("utf-8"), capture_output=True, timeout=30)
    if result.returncode:
        raise ValueError("Password hashing failed; rebuild the host installer")
    encoded = result.stdout.decode("ascii").rstrip("\n")
    if not encoded.startswith("$argon2") or len(encoded) > MAX_HASH_BYTES or "\n" in encoded:
        raise ValueError("Host installer returned an invalid password hash")
    data = (f"[users.{name}]\npassword_hash = {json.dumps(encoded)}\n"
            f"uid = {ACCOUNT_UID}\ngid = {ACCOUNT_UID}\n"
            f'home = "/home/{name}"\nshell = "/usr/bin/ion"\n\n'
            f"[groups.users]\ngid = {ACCOUNT_UID}\nmembers = [{json.dumps(name)}]\n").encode()
    assert len(data) <= MAX_ACCOUNT_FILE_BYTES
    descriptor = os.open(ACCOUNTS, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
    try:
        os.fchmod(descriptor, 0o600)
        if os.write(descriptor, data) != len(data):
            raise OSError("Short write while saving private account configuration")
        os.fsync(descriptor)
    except BaseException:
        ACCOUNTS.unlink()
        raise
    finally:
        os.close(descriptor)
    print("Created config/my_hw-live-accounts.toml; rebuild both live images before preparing a stick")


def require_image_account(args):
    if args.no_account:
        return
    name, encoded = read_accounts()
    with ExitStack() as inputs:
        input_file(inputs, args.bootloader)
        input_file(inputs, args.image)
    import hw_live
    scratch = Path(tempfile.mkdtemp(prefix="agent-hw-account-"))
    try:
        with hw_live.mounted_copy(ROOT, Path(args.image), scratch) as filesystem:
            descriptor = os.open(filesystem / "etc/shadow", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(descriptor, "rb") as source:
                if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                    raise ValueError("Image account database is not a regular file")
                shadow = source.read(MAX_ACCOUNT_FILE_BYTES + 1)
            if len(shadow) > MAX_ACCOUNT_FILE_BYTES or f"{name};{encoded}\n".encode() not in shadow.splitlines(keepends=True):
                raise ValueError("Image does not contain the private account; rebuild it before preparing a stick")
    finally:
        mounts = subprocess.run(["findmnt", "-R", str(scratch)], capture_output=True, timeout=10)
        if mounts.returncode != 1:
            raise RuntimeError(f"Refusing cleanup with unresolved mount state: {scratch}")
        shutil.rmtree(scratch)


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
    require_image_account(args)
    output = Path(args.output).absolute()
    if os.path.lexists(output):
        raise ValueError("Output already exists; choose a new path")
    with tempfile.TemporaryDirectory(prefix="agent-hw-stick-", dir=output.parent) as scratch:
        staged = Path(scratch) / "stick.img"
        size = stage_image(args.bootloader, args.image, staged)
        os.link(staged, output)
    print(f"Created {output} ({size} bytes)")


def prepare(args):
    require_image_account(args)
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
    commands.add_parser("account", help="prompt for a private login account and save its password hash")
    create = commands.add_parser("create-image", help="create a regular-file FAT image without root")
    disk = commands.add_parser("prepare", help="erase a removable USB whole disk; requires device write access")
    for command in (create, disk):
        command.add_argument("--bootloader", required=True, help="EFI bootloader file")
        command.add_argument("--image", required=True, help="live image file")
        command.add_argument("--no-account", action="store_true", help="allow a stick without a login account")
    create.add_argument("--output", required=True, help="new regular-file output path (never overwritten)")
    disk.add_argument("--device", required=True, help="removable USB whole disk under /dev")
    disk.add_argument("--confirm-erase", required=True, metavar="CANONICAL_DEVICE",
                      help="repeat the canonical device path to authorize destruction of all disk contents")
    args = parser.parse_args()
    if args.command is None:
        parser.print_help()
        return 0
    try:
        if args.command == "account":
            create_account()
        elif args.command == "create-image":
            create_image(args)
        else:
            prepare(args)
    except (OSError, ValueError, RuntimeError, UnicodeError, subprocess.SubprocessError) as error:
        print(f"hw-stick: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
