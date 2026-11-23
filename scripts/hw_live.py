import argparse
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import tomllib

MAX_IMAGE_BYTES = 4 * 1024 * 1024 * 1024
CHUNK_BYTES = 1024 * 1024
NVME_BYTES = 16 * CHUNK_BYTES
MOUNT_STEPS = 200
MAX_KEYS = 256
MAX_TIMEOUT_SECONDS = 600
FEATURES = ("system76_ec_debug", "lpss_debug")


def digest(path):
    size = path.stat().st_size
    if not 0 < size <= MAX_IMAGE_BYTES:
        raise ValueError("first-boot artifact exceeds size bound")
    result = hashlib.sha256()
    with path.open("rb") as source:
        for _ in range((size + CHUNK_BYTES - 1) // CHUNK_BYTES):
            result.update(source.read(CHUNK_BYTES))
        if source.read(1):
            raise ValueError("first-boot artifact grew during hashing")
    return result.hexdigest()


def run(command, **kwargs):
    return subprocess.run(command, check=True, timeout=120, **kwargs)


@contextmanager
def mounted_copy(root, image, scratch):
    copy = scratch / "inspection.img"
    run(["cp", "--reflink=auto", str(image), str(copy)])
    mount = scratch / "root"
    mount.mkdir()
    log = (scratch / "mount.log").open("wb")
    process = subprocess.Popen([str(root / "build/fstools/bin/redoxfs"), "--flush-on-unmount", str(copy), str(mount)], stdout=log, stderr=subprocess.STDOUT)
    try:
        for _ in range(MOUNT_STEPS):
            if os.path.ismount(mount) and (mount / "usr/lib/boot/kernel").is_file():
                break
            if process.poll() is not None:
                raise RuntimeError("first-boot inspection mount exited")
            time.sleep(0.1)
        else:
            raise RuntimeError("first-boot inspection mount timed out")
        yield mount
    finally:
        if os.path.ismount(mount):
            tool = shutil.which("fusermount3") or shutil.which("fusermount")
            if not tool:
                raise RuntimeError("cannot unmount first-boot inspection")
            run([tool, "-u", str(mount)])
        if process.poll() is None:
            process.terminate()
        process.wait(timeout=20)
        log.close()


def load_gate(root):
    spec = importlib.util.spec_from_file_location("hw_live_gate", root / "scripts/hw-live-check.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def safety(root, image, scratch, out):
    gate = load_gate(root)
    target = "x86_64-unknown-redox"
    build = root / "recipes/core/kernel/target" / target / "build"
    trees = [root / "recipes/core" / name / "source" for name in ("kernel", "bootloader", "base", "installer")]
    trees.append(root / "recipes/core/ferrion-update/crate")
    evidence = {"image_sha256": digest(image), "faults": []}
    with mounted_copy(root, image, scratch) as filesystem:
        kernel = filesystem / "usr/lib/boot/kernel"
        initfs = filesystem / "usr/lib/boot/initfs"
        try:
            gate.check(kernel, build, trees, filesystem, initfs)
        except gate.GateError as error:
            raise RuntimeError(f"first-boot safety gate: {error}") from error
        evidence["kernel_sha256"] = digest(kernel)
        evidence["initfs_sha256"] = digest(initfs)
        saved_kernel = scratch / "safe-kernel"
        shutil.copyfile(kernel, saved_kernel)
        for feature in FEATURES:
            fault_build = root / "build/hw-live-faults" / feature
            fault_kernel = fault_build / "kernel"
            if not fault_kernel.is_file():
                raise RuntimeError(f"build the real {feature} fault kernel before hw-live")
            shutil.copyfile(fault_kernel, kernel)
            try:
                gate.check(kernel, fault_build, trees, filesystem, initfs)
            except gate.GateError as error:
                if "forbidden enabled kernel features" not in str(error) or feature not in str(error):
                    raise RuntimeError(f"fault kernel rejected for the wrong reason: {error}") from error
                evidence["faults"].append({"fault": feature, "rejected": str(error), "kernel_sha256": digest(fault_kernel)})
            else:
                raise RuntimeError(f"enabled kernel feature accepted: {feature}")
            finally:
                shutil.copyfile(saved_kernel, kernel)
        planted = scratch / "fault-source"
        planted.mkdir()
        (planted / "fault.rs").write_text("fn fault() { unsafe { (runtime.SetVariable)(name, guid, 7, 0, data); } }\n")
        try:
            gate.check(kernel, build, [*trees, planted], filesystem, initfs)
        except gate.GateError as error:
            if "UEFI variable" not in str(error):
                raise RuntimeError(f"variable fault rejected for the wrong reason: {error}") from error
            evidence["faults"].append({"fault": "SetVariable", "rejected": "UEFI variable call"})
        else:
            raise RuntimeError("planted SetVariable call accepted")
        binary = filesystem / "usr/lib/drivers/nvmed"
        binary.write_bytes(b"planted-storage-driver\n")
        binary.chmod(0o755)
        try:
            gate.check_image(filesystem)
        except gate.GateError as error:
            if "forbidden image program" not in str(error):
                raise RuntimeError(f"storage fault rejected for the wrong reason: {error}") from error
            evidence["faults"].append({"fault": "nvmed", "rejected": "storage driver binary"})
        else:
            raise RuntimeError("planted storage driver accepted")
        finally:
            binary.unlink()
        config = filesystem / "usr/lib/pcid.d/xhcid.toml"
        previous = config.read_bytes()
        config.write_text('[[drivers]]\nname = "planted storage"\nclass = 1\ncommand = ["nvmed"]\n')
        try:
            gate.check_image(filesystem)
        except gate.GateError as error:
            if "storage class" not in str(error):
                raise RuntimeError(f"storage config rejected for the wrong reason: {error}") from error
            evidence["faults"].append({"fault": "PCI class 1", "rejected": "storage driver configuration"})
        else:
            raise RuntimeError("planted storage configuration accepted")
        finally:
            config.write_bytes(previous)
    (out / "safety.json").write_text(json.dumps(evidence, indent=2) + "\n")


def keyboard(vm, text):
    if len(text) > MAX_KEYS:
        raise ValueError("first-boot keyboard input exceeds bound")
    keys = {" ": "spc", "-": "minus", "/": "slash", ".": "dot", ">": "shift-dot", "\n": "ret"}
    for character in text:
        if not ((character.isascii() and character.isalnum()) or character in keys):
            raise ValueError("unsupported first-boot keyboard character")
        vm.command("sendkey " + keys.get(character, character))
        time.sleep(0.05)


def test(args, out, root, vm_type):
    if args.arch != "x86_64":
        return False, "hw-live requires x86_64"
    if not 0 < args.timeout <= MAX_TIMEOUT_SECONDS:
        return False, f"hw-live timeout must be within {MAX_TIMEOUT_SECONDS} seconds"
    config_name = "my_hw_live" if args.config == "desktop" else args.config
    config_path = root / "config" / f"{config_name}.toml"
    account = tomllib.loads(config_path.read_text()).get("users", {}).get("root", {})
    password = account.get("password", "")
    if not password or password == "password" or len(password) > MAX_KEYS:
        return False, "hw-live requires its own root password in a private config/my_*.toml"
    args = argparse.Namespace(**{**vars(args), "config": config_name})
    build = root / "build/x86_64" / config_name
    image = build / "redox-live.iso"
    bootloader = build / "bootloader-live.efi"
    if not image.is_file() or not bootloader.is_file():
        return False, "build the private first-boot live image before hw-live"
    scratch = Path(tempfile.mkdtemp(prefix="ferrion-hw-live-"))
    results = []
    try:
        safety(root, image, scratch, out)
        stick = scratch / "stick.img"
        run(["python3", str(root / "scripts/hw-stick.py"), "create-image", "--bootloader", str(bootloader), "--image", str(image), "--output", str(stick)])
        disk = scratch / "nvme.img"
        block = bytes(range(256)) * (CHUNK_BYTES // 256)
        with disk.open("wb") as stream:
            for _ in range(NVME_BYTES // CHUNK_BYTES):
                stream.write(block)
        before = digest(disk)
        desktop = config_name.endswith("_desktop")
        cases = ((24, "ps2"),) if desktop else ((1, "ps2"), (24, "ps2"), (24, "usb"))
        for cpus, input_type in cases:
            case = out / f"cpu-{cpus}-{input_type}"
            case.mkdir(exist_ok=True)
            flags = (f"-device qemu-xhci,id=hwusb -global VGA.xres=2560 -global VGA.yres=1600 "
                     f"-drive if=none,id=hw_nvme,format=raw,file={disk} "
                     "-device nvme,drive=hw_nvme,serial=HW_LIVE_PROBE")
            if input_type == "usb":
                flags += " -device usb-kbd,bus=hwusb.0"
            make = ("disk=usb", f"DISK={stick}", "usb=no", "net=no", "audio=no", "gpu=vga", "live=yes",
                    f"QEMU_SMP={cpus}", "QEMU_MEM=4096", "QEMU_MACHINE=q35" + (",i8042=off" if input_type == "usb" else ""))
            vm = vm_type(args, case, qemu_flags=flags, make=make, snapshot=False)
            try:
                if not vm.wait_serial("login:", args.timeout):
                    raise RuntimeError(f"{cpus}/{input_type}: no login prompt")
                login_seconds = vm.elapsed()
                if vm.command("info cpus").count("CPU #") != cpus:
                    raise RuntimeError("QEMU CPU count differs from the requested topology")
                if "first boot: storage disabled, diagnostic console enabled" not in vm.text():
                    raise RuntimeError("first-boot policy was not applied")
                frame = vm.screenshot()
                if (frame.width, frame.height) != (2560, 1600):
                    raise RuntimeError(f"wrong framebuffer: {frame.width} by {frame.height}")
                if desktop:
                    deadline = time.monotonic() + args.timeout
                    while frame.kind() != "login" and time.monotonic() < deadline:
                        time.sleep(0.2)
                        frame = vm.screenshot()
                    if frame.kind() != "login":
                        raise RuntimeError("first-boot desktop did not reach graphical login")
                    login_seconds = vm.elapsed()
                else:
                    time.sleep(2)
                    keyboard(vm, "root\n")
                    time.sleep(1)
                    keyboard(vm, password + "\n")
                    time.sleep(2)
                    keyboard(vm, "echo hw-live-keyboard > /scheme/debug\n")
                    if not vm.wait_serial("hw-live-keyboard", args.timeout):
                        raise RuntimeError(f"{cpus}/{input_type}: keyboard login and command failed")
                frame.save_png(case / "login.png")
                results.append({"cpus": cpus, "keyboard": input_type, "width": frame.width, "height": frame.height,
                                "login_seconds": round(login_seconds, 3)})
            finally:
                vm.close()
                after = digest(disk)
                if after != before:
                    raise RuntimeError("NVMe contents changed during first boot")
            results[-1]["nvme_sha256_before"] = before
            results[-1]["nvme_sha256_after"] = after
        (out / "hw-live.json").write_text(json.dumps(results, indent=2) + "\n")
        run(["cp", "--reflink=auto", str(stick), str(out / "stick.img")])
        return True, f"{len(results)}/{len(cases)} USB live boots; 2560x1600; writable NVMe unchanged; 5/5 planted hazards refused"
    finally:
        mounts = subprocess.run(["findmnt", "-R", str(scratch)], capture_output=True, timeout=10)
        if mounts.returncode != 1:
            raise RuntimeError(f"refusing cleanup with mount state unresolved: {scratch}")
        shutil.rmtree(scratch)
