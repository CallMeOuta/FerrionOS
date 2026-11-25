#!/usr/bin/env python3
import argparse
import base64
import json
import subprocess
import sys
import tempfile
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_TMUX_IMAGE = "localhost/ferrion-wt-rune-term:54b11e8d9902"
DEFAULT_BIN = ROOT / "components" / "ferrion-ui" / "target" / "debug" / "term-demo"

ANSI16 = [
    [0, 0, 0],
    [205, 0, 0],
    [0, 205, 0],
    [205, 205, 0],
    [0, 0, 238],
    [205, 0, 205],
    [0, 205, 205],
    [229, 229, 229],
    [127, 127, 127],
    [255, 0, 0],
    [0, 255, 0],
    [255, 255, 0],
    [92, 92, 255],
    [255, 0, 255],
    [0, 255, 255],
    [255, 255, 255],
]
DEFAULT_FG = [229, 229, 229]
DEFAULT_BG = [18, 20, 26]

DEC_SPECIAL = {
    "`": "◆",
    "a": "▒",
    "f": "°",
    "g": "±",
    "j": "┘",
    "k": "┐",
    "l": "┌",
    "m": "└",
    "n": "┼",
    "o": "⎺",
    "p": "⎻",
    "q": "─",
    "r": "⎼",
    "s": "⎽",
    "t": "├",
    "u": "┤",
    "v": "┴",
    "w": "┬",
    "x": "│",
    "y": "≤",
    "z": "≥",
    "{": "π",
    "|": "≠",
    "}": "£",
    "~": "·",
}

EXCLUDED_FROM_TMUX_DIFF = [
    (
        "CSI L / CSI M (IL/DL) outside DECSTBM scroll margins",
        "tmux 3.8 shifts rows outside scroll_top..=scroll_bottom; xterm ctlseqs defines IL/DL outside scroll margins as a no-op",
    ),
]

CASES = [
    ("basic_text_and_crlf", b"Hello, FerrionOS!\r\nSecond line\r\n\tTabbed\r\nBack\x08\x08ck"),
    (
        "sgr_16_256_truecolor_attrs",
        (
            b"\x1b[1;31mBoldRed\x1b[0m "
            b"\x1b[2;3;4;32mDimItUl\x1b[0m "
            b"\x1b[7;9;33;44mInvStrike\x1b[0m\r\n"
            b"\x1b[38;5;196;48;5;21mIdx256\x1b[0m "
            b"\x1b[38;2;12;180;240;48;2;40;20;60mTrueColor\x1b[0m "
            b"\x1b[91;102mBright16\x1b[0m\r\n"
            b"\x1b[38:2::200:100:50mColonRGB\x1b[0m "
            b"\x1b[38:5:82mColon256\x1b[0m "
            b"\x1b[4:2mDblUl\x1b[24mPlain"
        ),
    ),
    (
        "cursor_motion_and_bounds",
        (
            b"\x1b[5;10HAnchor"
            b"\x1b[2AUp2\x1b[3BDown3\x1b[4CRight4\x1b[6DLeft6"
            b"\x1b[12GCol12\x1b[8dRow8"
            b"\x1b[2ENext2\x1b[1FPrev1"
            b"\x1b[10;20fHvpPos"
        ),
    ),
    (
        "erase_ed_el_ech_bce",
        (
            b"0123456789ABCDEF\r\nLINE_TWO_CONTENT\r\nLINE_THREE_HELLO\r\nLINE_FOUR_WORLD"
            b"\x1b[44m\x1b[2;5H\x1b[K"
            b"\x1b[3;8H\x1b[1K"
            b"\x1b[1;4H\x1b[4X"
            b"\x1b[4;1H\x1b[2K\x1b[0mDONE"
        ),
    ),
    (
        "erase_display_modes",
        (
            b"AAA\r\nBBB\r\nCCC\r\nDDD\x1b[2;2H\x1b[0J"
            b"\x1b[4;1HZZZ\x1b[1;2H\x1b[1J"
        ),
    ),
    (
        "insert_delete_chars_and_lines",
        (
            b"ABCDEFGH\r\n12345678\r\nWXYZ9999"
            b"\x1b[1;3H\x1b[2@XX"
            b"\x1b[2;2H\x1b[3P"
            b"\x1b[2;1H\x1b[1LINSERTED"
            b"\x1b[1;1H\x1b[1M"
        ),
    ),
    (
        "scroll_region_decstbm_su_sd_ri",
        (
            b"HEADER\r\nR2\r\nR3\r\nR4\r\nR5\r\nFOOTER"
            b"\x1b[2;5r"
            b"\x1b[5;1H\r\nR6_NEW"
            b"\x1b[2;1H\x1bMREV_TOP"
            b"\x1b[1S"
            b"\x1b[1T"
            b"\x1b[r\x1b[7;1HAFTER_RESET"
        ),
    ),
    (
        "autowrap_and_pending_wrap",
        b"A" * 80 + b"B\r\n" + b"C" * 80 + b"\rD\r\n" + b"E" * 79 + b"F",
    ),
    (
        "wide_cjk_and_overwrite",
        (
            "日本語テストABC\r\n".encode("utf-8")
            + "漢字1234\x1b[2;2HX\x1b[2;4HY\r\n".encode("utf-8")
            + b"Z" * 79
            + "漢".encode("utf-8")
            + b"!"
        ),
    ),
    (
        "saved_cursor_and_alt_screen",
        (
            b"PRIMARY_LINE_1\r\nPRIMARY_LINE_2"
            b"\x1b[1;8H\x1b7"
            b"\x1b[?1049h"
            b"\x1b[2;4HALT_CONTENT"
            b"\x1b[?1049l"
            b"\x1b8_RESTORED"
        ),
    ),
    (
        "tabs_and_dec_special_graphics",
        b"A\tB\tC\x1b[Z_BACK\r\n\x1b(0lqqqk\r\nx   x\r\nmqqqj\x1b(B",
    ),
    (
        "scrollback_and_full_screen_scroll",
        b"".join(f"SCROLL_ROW_{i:02d}\r\n".encode("ascii") for i in range(30))
        + b"FINAL_PROMPT$ ",
    ),
    (
        "cursor_visibility_and_decom_origin",
        (
            b"TOP_OUTSIDE\r\n"
            b"\x1b[3;8r\x1b[?6h\x1b[1;1HORIGIN_TOP"
            b"\x1b[3;5HINSIDE"
            b"\x1b[?25l"
        ),
    ),
    (
        "insert_mode_irm_and_rep",
        b"HELLO WORLD\x1b[1;7H\x1b[4hBIG \x1b[4l\r\nX\x1b[5bY",
    ),
    (
        "sgr_partial_reset_and_hidden",
        (
            b"\x1b[1;2;3;4;7;8;9;31;42mALL"
            b"\x1b[22mNOBD\x1b[23mNOIT\x1b[24mNOUL"
            b"\x1b[27mNOIV\x1b[28mNOHD\x1b[29mNOST"
            b"\x1b[39mDFG\x1b[49mDBG\x1b[mPLAIN"
        ),
    ),
    (
        "scroll_region_clamping_and_il_dl",
        (
            b"R1\r\nR2\r\nR3\r\nR4\r\nR5\r\nR6"
            b"\x1b[2;5r"
            b"\x1b[3;1H\x1b[10ACLAMP_TOP"
            b"\x1b[10BCLAMP_BOT"
            b"\x1b[2;1H\x1b[2LNEW1\r\nNEW2"
            b"\x1b[3;1H\x1b[1MDELETED"
        ),
    ),
]


def resolve_indexed(idx):
    if idx < 16:
        return ANSI16[idx]
    if idx < 232:
        n = idx - 16
        r, g, b = n // 36, (n % 36) // 6, n % 6
        lvl = lambda v: 0 if v == 0 else 55 + 40 * v
        return [lvl(r), lvl(g), lvl(b)]
    gray = 8 + 10 * (idx - 232)
    return [gray, gray, gray]


def char_width(ch):
    eaw = unicodedata.east_asian_width(ch)
    return 2 if eaw in ("W", "F") else 1


def parse_tmux_capture(raw_bytes, cols, rows):
    text = raw_bytes.decode("utf-8", errors="replace")
    raw_lines = text.split("\n")
    if len(raw_lines) > rows and raw_lines[-1] == "":
        raw_lines = raw_lines[:-1]
    while len(raw_lines) < rows:
        raw_lines.append("")
    raw_lines = raw_lines[:rows]

    grid = []
    fg = DEFAULT_FG
    bg = DEFAULT_BG
    attrs = set()
    dec_special = False
    for line in raw_lines:
        cells = []
        i = 0
        n = len(line)
        while i < n:
            if line[i] == "\x0e":
                dec_special = True
                i += 1
                continue
            if line[i] == "\x0f":
                dec_special = False
                i += 1
                continue
            if line[i] == "\x1b" and i + 1 < n and line[i + 1] == "[":
                j = i + 2
                while j < n and not (0x40 <= ord(line[j]) <= 0x7E):
                    j += 1
                if j < n and line[j] == "m":
                    param_str = line[i + 2 : j]
                    parts = param_str.split(";") if param_str else ["0"]
                    p = 0
                    while p < len(parts):
                        tok = parts[p]
                        if ":" in tok:
                            sub = [int(x) if x else 0 for x in tok.split(":")]
                            code = sub[0]
                            if code == 4:
                                attrs.discard("underline")
                                attrs.discard("double_underline")
                                if len(sub) < 2 or sub[1] == 1:
                                    attrs.add("underline")
                                elif sub[1] == 2:
                                    attrs.add("double_underline")
                            elif code in (38, 48):
                                target_fg = code == 38
                                if len(sub) >= 3 and sub[1] == 5:
                                    c = resolve_indexed(sub[2] & 0xFF)
                                    if target_fg:
                                        fg = c
                                    else:
                                        bg = c
                                elif len(sub) >= 6 and sub[1] == 2:
                                    c = [sub[3] & 0xFF, sub[4] & 0xFF, sub[5] & 0xFF]
                                    if target_fg:
                                        fg = c
                                    else:
                                        bg = c
                                elif len(sub) >= 5 and sub[1] == 2:
                                    c = [sub[2] & 0xFF, sub[3] & 0xFF, sub[4] & 0xFF]
                                    if target_fg:
                                        fg = c
                                    else:
                                        bg = c
                            p += 1
                            continue
                        code = int(tok) if tok else 0
                        if code == 0:
                            fg = DEFAULT_FG
                            bg = DEFAULT_BG
                            attrs.clear()
                        elif code == 1:
                            attrs.add("bold")
                        elif code == 2:
                            attrs.add("dim")
                        elif code == 3:
                            attrs.add("italic")
                        elif code == 4:
                            attrs.discard("double_underline")
                            attrs.add("underline")
                        elif code == 7:
                            attrs.add("inverse")
                        elif code == 8:
                            attrs.add("hidden")
                        elif code == 9:
                            attrs.add("strikethrough")
                        elif code == 21:
                            attrs.discard("underline")
                            attrs.add("double_underline")
                        elif code == 22:
                            attrs.discard("bold")
                            attrs.discard("dim")
                        elif code == 23:
                            attrs.discard("italic")
                        elif code == 24:
                            attrs.discard("underline")
                            attrs.discard("double_underline")
                        elif code == 27:
                            attrs.discard("inverse")
                        elif code == 28:
                            attrs.discard("hidden")
                        elif code == 29:
                            attrs.discard("strikethrough")
                        elif 30 <= code <= 37:
                            fg = ANSI16[code - 30]
                        elif code == 39:
                            fg = DEFAULT_FG
                        elif 40 <= code <= 47:
                            bg = ANSI16[code - 40]
                        elif code == 49:
                            bg = DEFAULT_BG
                        elif 90 <= code <= 97:
                            fg = ANSI16[8 + code - 90]
                        elif 100 <= code <= 107:
                            bg = ANSI16[8 + code - 100]
                        elif code in (38, 48):
                            target_fg = code == 38
                            if p + 2 < len(parts) and parts[p + 1] == "5":
                                c = resolve_indexed(int(parts[p + 2]) & 0xFF)
                                if target_fg:
                                    fg = c
                                else:
                                    bg = c
                                p += 2
                            elif p + 4 < len(parts) and parts[p + 1] == "2":
                                c = [
                                    int(parts[p + 2]) & 0xFF,
                                    int(parts[p + 3]) & 0xFF,
                                    int(parts[p + 4]) & 0xFF,
                                ]
                                if target_fg:
                                    fg = c
                                else:
                                    bg = c
                                p += 4
                        p += 1
                i = j + 1
                continue
            ch = line[i]
            if dec_special:
                ch = DEC_SPECIAL.get(ch, ch)
            if ch == "\t":
                next_stop = ((len(cells) // 8) + 1) * 8
                target = min(next_stop, cols)
                while len(cells) < target:
                    cells.append({
                        "ch": " ",
                        "w": 1,
                        "fg": list(fg),
                        "bg": list(bg),
                        "attrs": sorted(attrs),
                    })
                i += 1
                continue
            w = char_width(ch)
            if len(cells) + w <= cols:
                cells.append({
                    "ch": ch,
                    "w": w,
                    "fg": list(fg),
                    "bg": list(bg),
                    "attrs": sorted(attrs),
                })
                if w == 2:
                    cells.append({
                        "ch": "",
                        "w": 0,
                        "fg": list(fg),
                        "bg": list(bg),
                        "attrs": sorted(attrs),
                    })
            i += 1
        while len(cells) < cols:
            cells.append({
                "ch": " ",
                "w": 1,
                "fg": list(DEFAULT_FG),
                "bg": list(DEFAULT_BG),
                "attrs": [],
            })
        grid.append(cells[:cols])
    return grid


def run_tmux_batch(image, cols, rows, case_dir):
    script = r"""
set -euo pipefail
cols="$1"
rows="$2"
case_dir="$3"
shift 3
idx=0
for name in "$@"; do
    sock="diff_$$_${idx}"
    idx=$((idx + 1))
    path="${case_dir}/${name}.bin"
    tmux -L "$sock" -f /dev/null new-session -d -x "$cols" -y "$rows" \
        env TERM=xterm-256color LC_ALL=C.UTF-8 LANG=C.UTF-8 sh -c "stty raw -echo; cat '$path'; sleep 30"
    sleep 0.15
    cur="$(tmux -L "$sock" display-message -t 0 -p '#{cursor_x},#{cursor_y},#{cursor_flag}')"
    cap_b64="$(tmux -L "$sock" capture-pane -t 0 -p -e -N | base64 -w 0)"
    tmux -L "$sock" kill-server >/dev/null 2>&1 || true
    printf '%s\t%s\t%s\n' "$name" "$cur" "$cap_b64"
done
"""
    cmd = [
        "podman",
        "run",
        "--rm",
        "--network=none",
        "-v",
        f"{case_dir}:/cases:ro",
        image,
        "bash",
        "-c",
        script,
        "rune-term-diff",
        str(cols),
        str(rows),
        "/cases",
        *[name for name, _ in CASES],
    ]
    out = subprocess.run(cmd, check=True, capture_output=True, text=True)
    results = {}
    for line in out.stdout.splitlines():
        if not line.strip():
            continue
        name, cur, cap_b64 = line.split("\t", 2)
        cx, cy, cvis = [int(x) for x in cur.split(",")]
        results[name] = {
            "capture_b64": cap_b64,
            "cursor": [cx, cy],
            "cursor_visible": bool(cvis),
        }
    return results


def main():
    parser = argparse.ArgumentParser(
        description="Differential check comparing rune-term against pinned tmux."
    )
    parser.add_argument("--bin", default=str(DEFAULT_BIN))
    parser.add_argument("--image", default=DEFAULT_TMUX_IMAGE)
    parser.add_argument("--cols", type=int, default=80)
    parser.add_argument("--rows", type=int, default=24)
    parser.add_argument("--json-out", default=None)
    args = parser.parse_args()

    with tempfile.TemporaryDirectory(prefix="rune-term-diff-") as tmp:
        tmp_path = Path(tmp)
        for name, data in CASES:
            (tmp_path / f"{name}.bin").write_bytes(data)

        tmux_results = run_tmux_batch(args.image, args.cols, args.rows, tmp)
        summary = {
            "image": args.image,
            "cols": args.cols,
            "rows": args.rows,
            "cases": len(CASES),
            "passed": 0,
            "failed": 0,
            "total_mismatches": 0,
            "excluded": [
                {"sequence": seq, "reason": reason}
                for seq, reason in EXCLUDED_FROM_TMUX_DIFF
            ],
            "details": [],
        }

        for name, _ in CASES:
            in_file = tmp_path / f"{name}.bin"
            out_file = tmp_path / f"{name}.json"
            subprocess.run(
                [
                    args.bin,
                    f"--feed={in_file}",
                    f"--cols={args.cols}",
                    f"--rows={args.rows}",
                    f"--dump-screen={out_file}",
                ],
                check=True,
            )
            got = json.loads(out_file.read_text())
            t = tmux_results[name]
            ref_grid = parse_tmux_capture(
                base64.b64decode(t["capture_b64"]), args.cols, args.rows
            )
            mismatches = []
            got_cursor = [
                got["cursor"][0] + (1 if got.get("pending_wrap") else 0),
                got["cursor"][1],
            ]
            if got_cursor != t["cursor"]:
                mismatches.append(
                    f"cursor got={got_cursor} (raw={got['cursor']}, pw={got.get('pending_wrap')}) ref={t['cursor']}"
                )
            if got["cursor_visible"] != t["cursor_visible"]:
                mismatches.append(
                    f"cursor_visible got={got['cursor_visible']} ref={t['cursor_visible']}"
                )
            for r in range(args.rows):
                for c in range(args.cols):
                    gc = got["lines"][r][c]
                    rc = ref_grid[r][c]
                    if (gc["ch"], gc["w"], gc["fg"], gc["bg"], sorted(gc["attrs"])) != (
                        rc["ch"],
                        rc["w"],
                        rc["fg"],
                        rc["bg"],
                        rc["attrs"],
                    ):
                        if len(mismatches) < 10:
                            mismatches.append(f"({r},{c}) got={gc} ref={rc}")
                        else:
                            mismatches.append("...")
                            break
                if len(mismatches) >= 11:
                    break
            ok = len(mismatches) == 0
            if ok:
                summary["passed"] += 1
            else:
                summary["failed"] += 1
                summary["total_mismatches"] += len(mismatches)
            summary["details"].append(
                {"name": name, "ok": ok, "mismatches": mismatches[:10]}
            )
            status = "PASS" if ok else f"FAIL ({len(mismatches)} diffs)"
            print(f"{status:20s} {name}")
            if not ok:
                for m in mismatches[:3]:
                    print(f"    {m}")

    for seq, reason in EXCLUDED_FROM_TMUX_DIFF:
        print(f"EXCLUDED: {seq} -- {reason}")
    print(f"SUMMARY: {summary['passed']}/{summary['cases']} passed, {summary['failed']} failed")
    if args.json_out:
        Path(args.json_out).write_text(json.dumps(summary, indent=2) + "\n")
    sys.exit(0 if summary["failed"] == 0 else 1)


if __name__ == "__main__":
    main()
