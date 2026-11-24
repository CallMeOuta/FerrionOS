#!/usr/bin/env python3
from pathlib import Path
import json
import os
import re
import select
import shutil
import struct
import subprocess
import tempfile
import time

EDID_BLOCK_SIZE = 128
MAX_GPT_ENTRIES = 128
OVMF_CANDIDATES = (
    "/usr/share/edk2/x64/OVMF.4m.fd",
    "/usr/share/ovmf/OVMF.fd",
    "/usr/share/OVMF/OVMF_CODE.fd",
)
VESAD_RE = re.compile(r"vesad:\s+(\d+)x(\d+)\s+stride\s+(\d+)")
BOOT_LINE_RE = re.compile(r"Output 0,\s+preferred:.*chosen:.*")
ESP_TYPE_GUID = bytes([
    0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11,
    0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B,
])


def with_edid_checksum(block: bytearray) -> bytes:
    assert len(block) == EDID_BLOCK_SIZE
    block[127] = (-sum(block[:127])) & 0xFF
    assert sum(block) % 256 == 0
    return bytes(block)


def encode_dtd(pixel_clock_10khz: int, width: int, height: int) -> bytes:
    assert 1 <= width <= 4095
    assert 1 <= height <= 4095
    assert 1 <= pixel_clock_10khz <= 65535
    h_blank = 160
    v_blank = 60
    d = bytearray(18)
    d[0:2] = struct.pack("<H", pixel_clock_10khz)
    d[2] = width & 0xFF
    d[3] = h_blank & 0xFF
    d[4] = (((width >> 8) & 0x0F) << 4) | ((h_blank >> 8) & 0x0F)
    d[5] = height & 0xFF
    d[6] = v_blank & 0xFF
    d[7] = (((height >> 8) & 0x0F) << 4) | ((v_blank >> 8) & 0x0F)
    d[8] = 48
    d[9] = 32
    d[10] = (3 << 4) | 5
    d[11] = 0
    d[12] = 0x58
    d[13] = 0x2C
    d[14] = 0x25
    d[17] = 0x18
    return bytes(d)


def make_base_block(dtd_w: int, dtd_h: int, dtd_clock_10khz: int, ext_count: int) -> bytes:
    block = bytearray(EDID_BLOCK_SIZE)
    block[0:8] = b"\x00\xFF\xFF\xFF\xFF\xFF\xFF\x00"
    block[8:10] = b"\x18\xA5"
    block[18] = 1
    block[19] = 4
    block[20] = 0xA5
    block[24] = 0x02
    block[54:72] = encode_dtd(dtd_clock_10khz, dtd_w, dtd_h)
    block[126] = ext_count
    return with_edid_checksum(block)


def make_displayid_block(width: int, height: int, preferred: bool) -> bytes:
    assert 1 <= width <= 65536
    assert 1 <= height <= 65536
    timing = bytearray(20)
    clock_10khz = 53325
    timing[0] = clock_10khz & 0xFF
    timing[1] = (clock_10khz >> 8) & 0xFF
    timing[2] = (clock_10khz >> 16) & 0xFF
    timing[3] = 0x80 if preferred else 0x00
    timing[4:6] = struct.pack("<H", width - 1)
    timing[6:8] = struct.pack("<H", 160 - 1)
    timing[8:10] = struct.pack("<H", 48 - 1)
    timing[10:12] = struct.pack("<H", 32 - 1)
    timing[12:14] = struct.pack("<H", height - 1)
    timing[14:16] = struct.pack("<H", 62 - 1)
    timing[16:18] = struct.pack("<H", 3 - 1)
    timing[18:20] = struct.pack("<H", 5 - 1)

    data_block = bytes([0x03, 0x00, 20]) + bytes(timing)
    block = bytearray(EDID_BLOCK_SIZE)
    block[0] = 0x70
    block[1] = 0x13
    block[2] = len(data_block)
    block[3] = 0x03
    block[4] = 0x00
    end = 5 + len(data_block)
    block[5:end] = data_block
    block[end] = (-sum(block[1:end])) & 0xFF
    assert sum(block[1 : end + 1]) % 256 == 0
    return with_edid_checksum(block)


def make_empty_cta_block() -> bytes:
    block = bytearray(EDID_BLOCK_SIZE)
    block[0] = 0x02
    block[1] = 0x03
    block[2] = 4
    block[3] = 0
    return with_edid_checksum(block)


def find_esp_offset(image: Path) -> int:
    with image.open("rb") as f:
        f.seek(512)
        hdr = f.read(92)
        if hdr[0:8] != b"EFI PART":
            raise RuntimeError(f"{image} has no GPT header")
        part_lba = struct.unpack_from("<Q", hdr, 72)[0]
        num_entries = struct.unpack_from("<I", hdr, 80)[0]
        entry_size = struct.unpack_from("<I", hdr, 84)[0]
        assert entry_size >= 128
        f.seek(part_lba * 512)
        for _ in range(min(num_entries, MAX_GPT_ENTRIES)):
            entry = f.read(entry_size)
            if entry[:16] == ESP_TYPE_GUID:
                first_lba = struct.unpack_from("<Q", entry, 32)[0]
                return first_lba * 512
    raise RuntimeError(f"no EFI System Partition found in {image}")


def find_ovmf() -> str:
    for path in OVMF_CANDIDATES:
        if Path(path).exists():
            return path
    raise RuntimeError("OVMF firmware not found")


def prepare_image(
    src_image: Path,
    shim_efi: Path,
    dst_image: Path,
    edid_bytes: bytes,
    protocol_tag: bytes,
) -> None:
    subprocess.run(["cp", "--reflink=auto", str(src_image), str(dst_image)], check=True)
    esp_offset = find_esp_offset(dst_image)
    target = f"{dst_image}@@{esp_offset}"
    with tempfile.TemporaryDirectory(prefix="ferrion-edid-esp-") as tmpdir:
        tmp = Path(tmpdir)
        orig = tmp / "FERRION.EFI"
        edid_file = tmp / "TEST.EDID"
        cfg_file = tmp / "TEST.CFG"
        edid_file.write_bytes(edid_bytes)
        cfg_file.write_bytes(protocol_tag)
        subprocess.run(["mcopy", "-o", "-i", target, "::EFI/BOOT/BOOTX64.EFI", str(orig)], check=True)
        subprocess.run(["mcopy", "-o", "-i", target, str(orig), "::EFI/BOOT/FERRION.EFI"], check=True)
        subprocess.run(["mcopy", "-o", "-i", target, str(shim_efi), "::EFI/BOOT/BOOTX64.EFI"], check=True)
        subprocess.run(["mcopy", "-o", "-i", target, str(edid_file), "::EFI/BOOT/TEST.EDID"], check=True)
        subprocess.run(["mcopy", "-o", "-i", target, str(cfg_file), "::EFI/BOOT/TEST.CFG"], check=True)


def boot_case(
    image: Path,
    vgamem_mb: int,
    timeout_s: float,
    serial_out: Path,
    press_key: bool = False,
) -> dict:
    ovmf = find_ovmf()
    cmd = [
        "qemu-system-x86_64",
        "-machine", "q35",
        "-enable-kvm",
        "-cpu", "host",
        "-smp", "4",
        "-m", "2048",
        "-bios", ovmf,
        "-display", "none",
        "-nic", "none",
        "-vga", "std",
        "-global", f"VGA.vgamem_mb={vgamem_mb}",
        "-drive", f"id=nvme0,file={image},format=raw,if=none,snapshot=on",
        "-device", "nvme,drive=nvme0,serial=NVME_SERIAL",
        "-serial", "stdio",
        "-monitor", "none",
    ]
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    deadline = time.monotonic() + timeout_s
    buf = bytearray()
    sent_key = False
    sent_ret = False
    try:
        while time.monotonic() < deadline:
            remaining = max(0.05, min(0.5, deadline - time.monotonic()))
            ready, _, _ = select.select([proc.stdout], [], [], remaining)
            if not ready:
                continue
            chunk = os.read(proc.stdout.fileno(), 4096)
            if not chunk:
                break
            buf.extend(chunk)
            if press_key and not sent_key and b"starting Boot" in buf:
                for _ in range(4):
                    time.sleep(0.15)
                    try:
                        proc.stdin.write(b" ")
                        proc.stdin.flush()
                    except OSError:
                        pass
                sent_key = True
            if press_key and not sent_ret and b"Arrow keys and enter" in buf:
                time.sleep(0.3)
                try:
                    proc.stdin.write(b"\r")
                    proc.stdin.flush()
                except OSError:
                    pass
                sent_ret = True
            if VESAD_RE.search(buf.decode("utf-8", errors="replace")):
                break
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()

    raw_text = buf.decode("utf-8", errors="replace")
    clean_text = re.sub(r"\x1b\[[0-9;]*[A-Za-z]", " ", raw_text).replace("\r", "\n")
    serial_out.write_text(clean_text, encoding="utf-8")

    vesad_match = VESAD_RE.search(clean_text)
    boot_match = BOOT_LINE_RE.search(clean_text)
    return {
        "vesad": f"{vesad_match.group(1)}x{vesad_match.group(2)}" if vesad_match else None,
        "vesad_line": vesad_match.group(0) if vesad_match else None,
        "boot_line": boot_match.group(0).strip() if boot_match else None,
        "quiet_boot_line": "Quiet boot: graphical debug stays off" in clean_text,
    }


def build_cases() -> list[dict]:
    edid_displayid_4k = make_base_block(1920, 1080, 14850, 1) + make_displayid_block(3840, 2160, True)
    edid_base_4k = make_base_block(3840, 2160, 53325, 0)
    edid_9ext = (
        make_base_block(1920, 1080, 14850, 9)
        + make_empty_cta_block() * 8
        + make_displayid_block(3840, 2160, True)
    )
    bad = bytearray(edid_displayid_4k)
    bad[127] ^= 0xFF
    edid_bad_checksum = bytes(bad)

    return [
        {
            "name": "displayid-4k-active",
            "edid": edid_displayid_4k,
            "cfg": b"A",
            "vgamem_mb": 64,
            "expected": "3840x2160",
        },
        {
            "name": "aspect-4k-16m",
            "edid": edid_base_4k,
            "cfg": b"A",
            "vgamem_mb": 16,
            "expected": "2560x1440",
        },
        {
            "name": "displayid-4k-discovered",
            "edid": edid_displayid_4k,
            "cfg": b"D",
            "vgamem_mb": 64,
            "expected": "3840x2160",
        },
        {
            "name": "oversized-9ext",
            "edid": edid_9ext,
            "cfg": b"A",
            "vgamem_mb": 64,
            "expected": "1920x1080",
        },
        {
            "name": "corrupt-edid",
            "edid": edid_bad_checksum,
            "cfg": b"A",
            "vgamem_mb": 64,
            "expected": "1280x800",
        },
    ]


def run_bootloader_edid(args, out: Path, root: Path):
    if args.arch != "x86_64":
        return False, "bootloader-edid requires x86_64"
    shim = root / "build/uefi-edid-shim/uefi-edid-shim.efi"
    if not shim.exists():
        return False, f"missing {shim}; run `make uefi-edid-shim` first"
    image = root / f"build/{args.arch}/{args.config}/harddrive.img"
    if not image.exists():
        return False, f"missing {image}"

    cases = build_cases()
    results = []
    passed = 0
    with tempfile.TemporaryDirectory(prefix="ferrion-boot-edid-") as tmpdir:
        tmp = Path(tmpdir)
        for case in cases:
            case_dir = out / case["name"]
            case_dir.mkdir(parents=True, exist_ok=True)
            img_copy = tmp / f"{case['name']}.img"
            prepare_image(image, shim, img_copy, case["edid"], case["cfg"])
            res = boot_case(
                img_copy,
                case["vgamem_mb"],
                min(float(args.timeout), 45.0),
                case_dir / "serial.log",
            )
            img_copy.unlink(missing_ok=True)
            ok = res["vesad"] == case["expected"] and res["quiet_boot_line"]
            if ok:
                passed += 1
            entry = {
                "name": case["name"],
                "expected": case["expected"],
                "got": res["vesad"],
                "quiet": res["quiet_boot_line"],
                "ok": ok,
                "vesad_line": res["vesad_line"],
            }
            results.append(entry)

    (out / "results.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    summary = "; ".join(f"{r['name']}: {r['got']} (want {r['expected']})" for r in results)
    return passed == len(cases), f"{passed}/{len(cases)} cases; {summary}"
