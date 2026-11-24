#!/usr/bin/env python3
import argparse
import hashlib
import json
import re
import shlex
import struct
import subprocess
import sys
from pathlib import Path

EDID_BLOCK_SIZE = 128
MAX_EDID_EXTENSION_BLOCKS = 8
MAX_CORPUS_FIXTURES = 32
MAX_FIXTURE_BYTES = EDID_BLOCK_SIZE * 16
SKIP_DMT_BLOCKS = (
    "Established Timings I & II:",
    "Standard Timings:",
    "Established timings III:",
)

CAPTURED_QEMU_FIXTURES = (
    (
        "qemu-7680x4320",
        "captured (QEMU 11.1.3 virtio-vga)",
        "QEMU 11.1.3 virtio-vga xres=7680 yres=4320 (DisplayID 1.3 Type I 75Hz + CTA VICs)",
    ),
    (
        "qemu-3440x1440",
        "captured (QEMU 11.1.3 virtio-vga)",
        "QEMU 11.1.3 virtio-vga xres=3440 yres=1440 (base DTD 43:18 75Hz + CTA VICs)",
    ),
    (
        "qemu-5120x1440",
        "captured (QEMU 11.1.3 virtio-vga)",
        "QEMU 11.1.3 virtio-vga xres=5120 yres=1440 (DisplayID 1.3 Type I 32:9 75Hz + CTA VICs)",
    ),
    (
        "qemu-3000x2000",
        "captured (QEMU 11.1.3 virtio-vga)",
        "QEMU 11.1.3 virtio-vga xres=3000 yres=2000 (base DTD 3:2 75Hz + CTA VICs)",
    ),
    (
        "qemu-2160x3840",
        "captured (QEMU 11.1.3 virtio-vga)",
        "QEMU 11.1.3 virtio-vga xres=2160 yres=3840 (DisplayID 1.3 Type I portrait 75Hz + CTA VICs)",
    ),
    (
        "qemu-2560x1440-120hz",
        "captured (QEMU 11.1.3 bochs-display)",
        "QEMU 11.1.3 bochs-display xres=2560 yres=1440 refresh_rate=120000 (real 120Hz base DTD + CTA VICs)",
    ),
)


def with_edid_checksum(block: bytearray) -> bytes:
    assert len(block) == EDID_BLOCK_SIZE
    block[127] = (-sum(block[:127])) & 0xFF
    assert sum(block) % 256 == 0
    return bytes(block)


def encode_dtd(
    pixel_clock_10khz: int,
    h_active: int,
    h_blank: int,
    h_sync_off: int,
    h_sync_width: int,
    v_active: int,
    v_blank: int,
    v_sync_off: int,
    v_sync_width: int,
) -> bytes:
    assert 1 <= pixel_clock_10khz <= 65535
    assert 1 <= h_active <= 4095 and 1 <= h_blank <= 4095
    assert 1 <= v_active <= 4095 and 1 <= v_blank <= 4095
    d = bytearray(18)
    d[0:2] = struct.pack("<H", pixel_clock_10khz)
    d[2] = h_active & 0xFF
    d[3] = h_blank & 0xFF
    d[4] = (((h_active >> 8) & 0x0F) << 4) | ((h_blank >> 8) & 0x0F)
    d[5] = v_active & 0xFF
    d[6] = v_blank & 0xFF
    d[7] = (((v_active >> 8) & 0x0F) << 4) | ((v_blank >> 8) & 0x0F)
    d[8] = h_sync_off & 0xFF
    d[9] = h_sync_width & 0xFF
    d[10] = ((v_sync_off & 0x0F) << 4) | (v_sync_width & 0x0F)
    d[11] = (
        (((h_sync_off >> 8) & 0x03) << 6)
        | (((h_sync_width >> 8) & 0x03) << 4)
        | (((v_sync_off >> 4) & 0x03) << 2)
        | ((v_sync_width >> 4) & 0x03)
    )
    d[12] = 0x58
    d[13] = 0x2C
    d[14] = 0x25
    d[17] = 0x1E
    return bytes(d)


def make_base_block(dtds: list[bytes], ext_count: int) -> bytes:
    assert len(dtds) <= 4
    block = bytearray(EDID_BLOCK_SIZE)
    block[0:8] = b"\x00\xFF\xFF\xFF\xFF\xFF\xFF\x00"
    block[8:10] = b"\x18\xA5"
    block[18] = 1
    block[19] = 4
    block[20] = 0xA5
    block[21] = 60
    block[22] = 34
    block[24] = 0x02 if dtds else 0x00
    for idx, dtd in enumerate(dtds):
        assert len(dtd) == 18
        off = 54 + idx * 18
        block[off : off + 18] = dtd
    block[126] = ext_count
    return with_edid_checksum(block)


def make_cta_block(vics: list[int], dtds: list[bytes] | None = None) -> bytes:
    assert len(vics) <= 31
    block = bytearray(EDID_BLOCK_SIZE)
    block[0] = 0x02
    block[1] = 0x03
    pos = 4
    if vics:
        block[pos] = (2 << 5) | len(vics)
        pos += 1
        for vic in vics:
            assert 1 <= vic <= 255
            block[pos] = vic
            pos += 1
    block[2] = pos
    block[3] = 0
    for dtd in dtds or []:
        assert len(dtd) == 18
        assert pos + 18 <= 127
        block[pos : pos + 18] = dtd
        pos += 18
    return with_edid_checksum(block)


def encode_displayid_timing(
    clock_raw: int,
    h_active: int,
    h_blank: int,
    h_sync_off: int,
    h_sync_width: int,
    v_active: int,
    v_blank: int,
    v_sync_off: int,
    v_sync_width: int,
    preferred: bool,
    aspect_code: int = 0x07,
) -> bytes:
    assert 0 <= clock_raw <= 0xFFFFFF
    assert 1 <= h_active <= 65536 and 1 <= h_blank <= 65536
    assert 1 <= v_active <= 65536 and 1 <= v_blank <= 65536
    t = bytearray(20)
    t[0] = clock_raw & 0xFF
    t[1] = (clock_raw >> 8) & 0xFF
    t[2] = (clock_raw >> 16) & 0xFF
    t[3] = (0x80 if preferred else 0x00) | (aspect_code & 0x0F)
    t[4:6] = struct.pack("<H", h_active - 1)
    t[6:8] = struct.pack("<H", h_blank - 1)
    t[8:10] = struct.pack("<H", h_sync_off - 1)
    t[10:12] = struct.pack("<H", h_sync_width - 1)
    t[12:14] = struct.pack("<H", v_active - 1)
    t[14:16] = struct.pack("<H", v_blank - 1)
    t[16:18] = struct.pack("<H", v_sync_off - 1)
    t[18:20] = struct.pack("<H", v_sync_width - 1)
    return bytes(t)


def make_displayid_block(version: int, tag: int, timings: list[bytes]) -> bytes:
    payload = bytes([tag, 0x00, 20 * len(timings)]) + b"".join(timings)
    assert 5 + len(payload) <= 126
    block = bytearray(EDID_BLOCK_SIZE)
    block[0] = 0x70
    block[1] = version
    block[2] = len(payload)
    block[3] = 0x03
    block[4] = 0x00
    end = 5 + len(payload)
    block[5:end] = payload
    block[end] = (-sum(block[1:end])) & 0xFF
    assert sum(block[1 : end + 1]) % 256 == 0
    return with_edid_checksum(block)


def build_generated_fixtures() -> list[dict]:
    dtd_1080p_60 = encode_dtd(14850, 1920, 280, 88, 44, 1080, 45, 4, 5)
    dtd_1080p_120 = encode_dtd(29700, 1920, 280, 88, 44, 1080, 45, 4, 5)
    dtd_1080p_240 = encode_dtd(59400, 1920, 280, 88, 44, 1080, 45, 4, 5)
    dtd_4k_60 = encode_dtd(59400, 3840, 560, 176, 88, 2160, 90, 8, 10)
    dtd_2560x1600_60 = encode_dtd(26850, 2560, 160, 48, 32, 1600, 46, 3, 6)
    dtd_3000x2000_60 = encode_dtd(39270, 3000, 160, 48, 32, 2000, 71, 3, 6)
    dtd_2848x1600_60 = encode_dtd(29754, 2848, 160, 48, 32, 1600, 48, 3, 6)

    did7_1080p_5994 = encode_displayid_timing(148351, 1920, 280, 88, 44, 1080, 45, 4, 5, True, 0x03)
    did7_1080p_11988 = encode_displayid_timing(296702, 1920, 280, 88, 44, 1080, 45, 4, 5, False, 0x03)

    did1_1440p_144 = encode_displayid_timing(58636, 2560, 160, 48, 32, 1440, 57, 3, 5, True, 0x03)
    did1_1440p_165 = encode_displayid_timing(67715, 2560, 160, 48, 32, 1440, 69, 3, 5, False, 0x03)

    did7_4k_120 = encode_displayid_timing(1187999, 3840, 560, 176, 88, 2160, 90, 8, 10, True, 0x03)
    did7_4k_11988 = encode_displayid_timing(1186812, 3840, 560, 176, 88, 2160, 90, 8, 10, True, 0x03)
    did7_8k_120 = encode_displayid_timing(4364697, 7680, 512, 128, 64, 4320, 120, 8, 10, True, 0x03)

    did1_4k_60 = encode_displayid_timing(53327, 3840, 160, 48, 32, 2160, 62, 3, 5, True, 0x03)
    did1_3440x1440_144 = encode_displayid_timing(78479, 3440, 160, 48, 32, 1440, 74, 3, 5, True, 0x07)
    did1_7680x2160_60 = encode_displayid_timing(106247, 7680, 160, 48, 32, 2160, 98, 3, 5, True, 0x07)
    did1_5120x1440_120 = encode_displayid_timing(95385, 5120, 160, 48, 32, 1440, 65, 3, 5, False, 0x07)
    did1_2160x3840_60 = encode_displayid_timing(54683, 2160, 160, 48, 32, 3840, 88, 3, 5, True, 0x07)

    edid_panel_4k = make_base_block([dtd_1080p_60], 1) + make_displayid_block(0x13, 0x03, [did1_4k_60])
    bad_chk = bytearray(make_base_block([dtd_1080p_60], 0))
    bad_chk[127] ^= 0xFF

    edid_9ext = (
        make_base_block([dtd_1080p_60], 9)
        + make_cta_block([]) * 8
        + make_displayid_block(0x13, 0x03, [did1_4k_60])
    )

    overflow_did = bytearray(EDID_BLOCK_SIZE)
    overflow_did[0] = 0x70
    overflow_did[1] = 0x13
    overflow_did[2] = 125
    overflow_did[3] = 0x03
    overflow_did[4] = 0x00
    overflow_did_b = with_edid_checksum(overflow_did)

    return [
        {
            "name": "refresh-1080p-60-120-240",
            "edid": make_base_block([dtd_1080p_60, dtd_1080p_120, dtd_1080p_240], 0),
            "description": "1920x1080 base DTDs at 60, 120, and 240 Hz",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "refresh-1080p-5994-11988",
            "edid": make_base_block([], 1)
            + make_displayid_block(0x20, 0x22, [did7_1080p_5994, did7_1080p_11988]),
            "description": "1920x1080 DisplayID 2.0 Type VII at 59.940 and 119.880 Hz",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "refresh-1440p-144-165",
            "edid": make_base_block([], 1)
            + make_displayid_block(0x13, 0x03, [did1_1440p_144, did1_1440p_165]),
            "description": "2560x1440 DisplayID 1.3 Type I at 144 and 165 Hz",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "refresh-4k-60-120-multi",
            "edid": make_base_block([dtd_4k_60], 2)
            + make_cta_block([97, 118])
            + make_displayid_block(0x20, 0x22, [did7_4k_120]),
            "description": "3840x2160 multi-rate: 60Hz base DTD + VIC 97/118 + 120Hz DisplayID Type VII",
            "malformed": None,
            "preferred_divergence": {
                "reason": "FerrionOS prioritizes a DisplayID preferred timing over Block 0 DTD 1 so high-bandwidth native timings beat compatibility base DTDs",
            },
        },
        {
            "name": "refresh-4k-11988-displayid7",
            "edid": make_base_block([], 1) + make_displayid_block(0x20, 0x22, [did7_4k_11988]),
            "description": "3840x2160 at 119.880 Hz via DisplayID 2.0 Type VII",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "vic-only-4k120",
            "edid": make_base_block([], 1) + make_cta_block([118]),
            "description": "4K120 advertised only via CTA-861 VIC 118 (no DTD or DisplayID)",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "displayid7-4k120",
            "edid": make_base_block([], 1) + make_displayid_block(0x20, 0x22, [did7_4k_120]),
            "description": "4K120 advertised via DisplayID 2.0 Type VII",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "vic-only-8k-50-60-120",
            "edid": make_base_block([], 1) + make_cta_block([199, 198, 201]),
            "description": "8K advertised only via CTA-861 VICs 199 (60Hz), 198 (50Hz), and 201 (120Hz)",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "displayid7-8k120",
            "edid": make_base_block([], 1) + make_displayid_block(0x20, 0x22, [did7_8k_120]),
            "description": "8K120 via DisplayID 2.0 Type VII (4.365 GHz pixel clock; exceeds u32 kHz*1000)",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "aspect-16x10-2560x1600",
            "edid": make_base_block([dtd_2560x1600_60], 0),
            "description": "16:10 2560x1600@60Hz base DTD",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "aspect-3x2-3000x2000",
            "edid": make_base_block([dtd_3000x2000_60], 0),
            "description": "3:2 3000x2000@60Hz base DTD",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "aspect-21x9-3440x1440-144",
            "edid": make_base_block([], 2)
            + make_cta_block([90])
            + make_displayid_block(0x13, 0x03, [did1_3440x1440_144]),
            "description": "21:9 3440x1440@144Hz DisplayID Type I + 2560x1080@60 VIC 90",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "aspect-21x9-5120x2160-vic126",
            "edid": make_base_block([], 1) + make_cta_block([126, 193]),
            "description": "21:9 5120x2160 via CTA VIC 126 (60Hz) and VIC 193 (120Hz)",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "aspect-32x9-7680x2160",
            "edid": make_base_block([], 1)
            + make_displayid_block(0x13, 0x03, [did1_7680x2160_60, did1_5120x1440_120]),
            "description": "32:9 7680x2160@60Hz and 5120x1440@120Hz DisplayID Type I",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "aspect-portrait-2160x3840",
            "edid": make_base_block([], 1)
            + make_displayid_block(0x13, 0x03, [did1_2160x3840_60]),
            "description": "Native portrait 2160x3840@60Hz DisplayID Type I",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "panel-4k-base-1080p-dtd",
            "edid": edid_panel_4k,
            "description": "4K panel whose base DTD is 1920x1080 and whose DisplayID preferred timing is 3840x2160",
            "malformed": None,
            "preferred_divergence": {
                "reason": "4K panel puts 1920x1080 in Block 0 DTD 1 for legacy compatibility and its native 3840x2160 preferred timing in DisplayID; FerrionOS chooses DisplayID preferred",
            },
        },
        {
            "name": "aspect-tol-near16x9",
            "edid": make_base_block([dtd_2848x1600_60], 0),
            "description": "Near-16:9 2848x1600@60Hz (0.125% off 16:9, not exact 16:9) for OVMF aspect-approx vs fits check",
            "malformed": None,
            "preferred_divergence": None,
        },
        {
            "name": "malformed-bad-checksum",
            "edid": bytes(bad_chk),
            "description": "Base block with inverted checksum byte",
            "malformed": {
                "kind": "bad-base-checksum",
                "expected_error": "BadBaseChecksum",
                "expected_extensions_truncated": False,
                "expected_bad_extension_blocks": 0,
            },
            "preferred_divergence": None,
        },
        {
            "name": "malformed-wrong-length",
            "edid": make_base_block([dtd_1080p_60], 1)[:200],
            "description": "Truncated 200-byte EDID (not a multiple of 128 bytes)",
            "malformed": {
                "kind": "invalid-length",
                "expected_error": "InvalidLength",
                "expected_extensions_truncated": False,
                "expected_bad_extension_blocks": 0,
            },
            "preferred_divergence": None,
        },
        {
            "name": "malformed-oversized-9ext",
            "edid": edid_9ext,
            "description": "9 extension blocks (exceeds MAX_EDID_EXTENSION_BLOCKS=8); 9th block holds DisplayID 4K",
            "malformed": {
                "kind": "oversized-extensions",
                "expected_error": None,
                "expected_extensions_truncated": True,
                "expected_bad_extension_blocks": 0,
            },
            "preferred_divergence": None,
        },
        {
            "name": "malformed-displayid-len-overflow",
            "edid": make_base_block([dtd_1080p_60], 1) + overflow_did_b,
            "description": "DisplayID extension whose section payload length overflows past byte 126",
            "malformed": {
                "kind": "displayid-length-overflow",
                "expected_error": None,
                "expected_extensions_truncated": False,
                "expected_bad_extension_blocks": 0,
            },
            "preferred_divergence": None,
        },
    ]


def parse_edid_decode_output(raw_text: str) -> dict:
    modes = []
    block_kind = None
    in_skip_dmt = False
    did_tag = None
    base_first_dtd = None
    did_preferred = None
    cta_native_vic = None
    cta_first_vic = None

    dtd_re = re.compile(r"^\s+DTD(?:\s+\d+)?:\s+(\d+)x(\d+)(i?)\s+([0-9.]+)\s+Hz\b(.*)$")
    vic_re = re.compile(r"^\s+VIC\s+(\d+):\s+(\d+)x(\d+)(i?)\s+([0-9.]+)\s+Hz\b(.*)$")

    for line in raw_text.splitlines():
        if line.startswith("Block 0, Base EDID:"):
            block_kind = "base"
            in_skip_dmt = False
            did_tag = None
            continue
        if re.match(r"^Block \d+, CTA-861 Extension Block:", line):
            block_kind = "cta"
            in_skip_dmt = False
            did_tag = None
            continue
        if re.match(r"^Block \d+, DisplayID Extension Block:", line):
            block_kind = "displayid"
            in_skip_dmt = False
            did_tag = None
            continue
        if line.startswith("Block "):
            block_kind = "other"
            in_skip_dmt = False
            did_tag = None
            continue

        stripped = line.strip()
        if any(stripped.startswith(prefix) for prefix in SKIP_DMT_BLOCKS):
            in_skip_dmt = True
            continue
        if stripped.startswith("Detailed Timing Descriptors:") or stripped.startswith("Display Range Limits:"):
            in_skip_dmt = False
        if block_kind == "displayid":
            if "Video Timing Modes Type 1" in line:
                did_tag = "displayid-type1"
                continue
            if "Video Timing Modes Type 7" in line:
                did_tag = "displayid-type7"
                continue

        if in_skip_dmt:
            continue

        m_dtd = dtd_re.match(line)
        if m_dtd:
            w, h, interlaced, hz, tail = (
                int(m_dtd.group(1)),
                int(m_dtd.group(2)),
                bool(m_dtd.group(3)),
                float(m_dtd.group(4)),
                m_dtd.group(5),
            )
            if interlaced:
                continue
            refresh_mhz = int(round(hz * 1000.0))
            if block_kind == "base":
                src = "base-dtd"
                pref = base_first_dtd is None
                entry = {
                    "width": w,
                    "height": h,
                    "refresh_mhz": refresh_mhz,
                    "preferred": pref,
                    "source_block": src,
                }
                if base_first_dtd is None:
                    base_first_dtd = entry
                modes.append(entry)
            elif block_kind == "cta":
                modes.append(
                    {
                        "width": w,
                        "height": h,
                        "refresh_mhz": refresh_mhz,
                        "preferred": False,
                        "source_block": "cta-dtd",
                    }
                )
            elif block_kind == "displayid" and did_tag in ("displayid-type1", "displayid-type7"):
                pref = "preferred" in tail
                entry = {
                    "width": w,
                    "height": h,
                    "refresh_mhz": refresh_mhz,
                    "preferred": pref,
                    "source_block": did_tag,
                }
                if pref and did_preferred is None:
                    did_preferred = entry
                modes.append(entry)
            continue

        m_vic = vic_re.match(line)
        if m_vic and block_kind == "cta":
            _vic, w, h, interlaced, hz, tail = (
                int(m_vic.group(1)),
                int(m_vic.group(2)),
                int(m_vic.group(3)),
                bool(m_vic.group(4)),
                float(m_vic.group(5)),
                m_vic.group(6),
            )
            if interlaced:
                continue
            refresh_mhz = int(round(hz * 1000.0))
            native = "(native)" in tail
            entry = {
                "width": w,
                "height": h,
                "refresh_mhz": refresh_mhz,
                "preferred": native,
                "source_block": "cta-vic",
            }
            if native and cta_native_vic is None:
                cta_native_vic = entry
            if cta_first_vic is None:
                cta_first_vic = entry
            modes.append(entry)

    edid_decode_preferred = base_first_dtd or did_preferred or cta_native_vic or cta_first_vic
    return {
        "modes": modes,
        "edid_decode_preferred": edid_decode_preferred,
    }


def run_edid_decode(edid_cmd: list[str], data: bytes) -> tuple[str, int]:
    proc = subprocess.run(edid_cmd, input=data, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return proc.stdout.decode("utf-8", errors="replace"), proc.returncode


def get_edid_decode_version(edid_cmd: list[str]) -> str:
    out = subprocess.check_output(edid_cmd + ["--version"], stderr=subprocess.STDOUT).decode("utf-8", errors="replace")
    return out.strip().splitlines()[0]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description="Generate or verify FerrionOS EDID fixture corpus.")
    parser.add_argument("--corpus-dir", type=Path, default=Path("scripts/edid-corpus"))
    parser.add_argument(
        "--edid-decode-cmd",
        default="edid-decode",
        help="Command to invoke edid-decode (e.g. 'podman run --rm -i localhost/ferrion-wt-displays:latest edid-decode')",
    )
    parser.add_argument("--check", action="store_true", help="Verify committed corpus files match generator output")
    args = parser.parse_args(argv)

    edid_cmd = shlex.split(args.edid_decode_cmd)
    version = get_edid_decode_version(edid_cmd)
    corpus_dir = args.corpus_dir
    corpus_dir.mkdir(parents=True, exist_ok=True)

    generated = build_generated_fixtures()
    total_count = len(CAPTURED_QEMU_FIXTURES) + len(generated)
    assert total_count <= MAX_CORPUS_FIXTURES, f"{total_count} > {MAX_CORPUS_FIXTURES}"

    manifest_fixtures = []

    for name, origin, desc in CAPTURED_QEMU_FIXTURES:
        edid_path = corpus_dir / f"{name}.edid"
        if not edid_path.exists():
            raise FileNotFoundError(f"missing captured fixture {edid_path}")
        raw = edid_path.read_bytes()
        assert 1 <= len(raw) <= MAX_FIXTURE_BYTES
        decoded_text, _rc = run_edid_decode(edid_cmd, raw)
        parsed = parse_edid_decode_output(decoded_text)
        expected = {
            "name": name,
            "origin": origin,
            "description": desc,
            "size_bytes": len(raw),
            "sha256": hashlib.sha256(raw).hexdigest(),
            "oracle_version": version,
            "malformed": None,
            "preferred_divergence": None,
            "edid_decode_preferred": parsed["edid_decode_preferred"],
            "modes": parsed["modes"],
        }
        json_path = corpus_dir / f"{name}.json"
        json_text = json.dumps(expected, indent=2) + "\n"
        if args.check:
            if not json_path.exists() or json_path.read_text(encoding="utf-8") != json_text:
                print(f"MISMATCH {json_path}", file=sys.stderr)
                return 1
        else:
            json_path.write_text(json_text, encoding="utf-8")
        manifest_fixtures.append(
            {
                "name": name,
                "file": f"{name}.edid",
                "expected_file": f"{name}.json",
                "origin": origin,
                "description": desc,
                "size_bytes": len(raw),
                "sha256": expected["sha256"],
                "malformed": None,
                "preferred_divergence": None,
            }
        )

    for spec in generated:
        name = spec["name"]
        raw = spec["edid"]
        assert 1 <= len(raw) <= MAX_FIXTURE_BYTES
        edid_path = corpus_dir / f"{name}.edid"
        if args.check:
            if not edid_path.exists() or edid_path.read_bytes() != raw:
                print(f"MISMATCH {edid_path}", file=sys.stderr)
                return 1
        else:
            edid_path.write_bytes(raw)

        malformed = spec["malformed"]
        if malformed and malformed["expected_error"] is not None:
            parsed = {"modes": [], "edid_decode_preferred": None}
        else:
            decoded_text, _rc = run_edid_decode(edid_cmd, raw)
            parsed = parse_edid_decode_output(decoded_text)

        pref_div = spec["preferred_divergence"]
        if malformed and malformed.get("expected_extensions_truncated"):
            ferrion_modes = [m for m in parsed["modes"] if m["source_block"] == "base-dtd"]
        else:
            ferrion_modes = parsed["modes"]

        if pref_div is not None:
            did_prefs = [m for m in parsed["modes"] if m["source_block"].startswith("displayid-") and m["preferred"]]
            ferrion_pref = did_prefs[0]
            pref_div_full = {
                "reason": pref_div["reason"],
                "edid_decode_preferred": parsed["edid_decode_preferred"],
                "ferrion_preferred": ferrion_pref,
            }
        else:
            pref_div_full = None

        expected = {
            "name": name,
            "origin": "generated (scripts/edid_fixtures.py)",
            "description": spec["description"],
            "size_bytes": len(raw),
            "sha256": hashlib.sha256(raw).hexdigest(),
            "oracle_version": version,
            "malformed": malformed,
            "preferred_divergence": pref_div_full,
            "edid_decode_preferred": parsed["edid_decode_preferred"],
            "modes": ferrion_modes,
        }
        json_path = corpus_dir / f"{name}.json"
        json_text = json.dumps(expected, indent=2) + "\n"
        if args.check:
            if not json_path.exists() or json_path.read_text(encoding="utf-8") != json_text:
                print(f"MISMATCH {json_path}", file=sys.stderr)
                return 1
        else:
            json_path.write_text(json_text, encoding="utf-8")
        manifest_fixtures.append(
            {
                "name": name,
                "file": f"{name}.edid",
                "expected_file": f"{name}.json",
                "origin": expected["origin"],
                "description": spec["description"],
                "size_bytes": len(raw),
                "sha256": expected["sha256"],
                "malformed": malformed,
                "preferred_divergence": pref_div_full,
            }
        )

    manifest = {
        "schema_version": 1,
        "license": "MIT",
        "max_fixtures": MAX_CORPUS_FIXTURES,
        "max_fixture_bytes": MAX_FIXTURE_BYTES,
        "max_edid_extension_blocks": MAX_EDID_EXTENSION_BLOCKS,
        "generator": "scripts/edid_fixtures.py",
        "oracle_version": version,
        "skip_dmt_blocks": list(SKIP_DMT_BLOCKS),
        "fixture_count": len(manifest_fixtures),
        "fixtures": manifest_fixtures,
    }
    manifest_path = corpus_dir / "manifest.json"
    manifest_text = json.dumps(manifest, indent=2) + "\n"
    if args.check:
        if not manifest_path.exists() or manifest_path.read_text(encoding="utf-8") != manifest_text:
            print(f"MISMATCH {manifest_path}", file=sys.stderr)
            return 1
        print(f"OK: {len(manifest_fixtures)}/{MAX_CORPUS_FIXTURES} fixtures verified ({version})")
    else:
        manifest_path.write_text(manifest_text, encoding="utf-8")
        print(f"Wrote {len(manifest_fixtures)}/{MAX_CORPUS_FIXTURES} fixtures ({version})")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
