#!/usr/bin/env python3

import json
import re
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from fractions import Fraction
from pathlib import Path

HERE = Path(__file__).resolve().parent
SVG = "{http://www.w3.org/2000/svg}"
PROGRAMS = ("rsvg-convert", "magick", "optipng")

UNIT = 16
NAME_CAP_HEIGHT = 24
MARK_TO_NAME = 32
NAME_INSET = 4
LOCKUP_WIDTH = 160
LOCKUP_HEIGHT = 176

SCREEN = (1280, 800)
LOGIN_BOX_HEIGHT = 164
NAME_TO_BOX = 64

START_ICON = (48, 6)
FAVICON = (32, 4)
AVATAR = (500, 40)

BOOK_LOGO = (511, 180)

SOCIAL_PREVIEW = (1280, 640)
SOCIAL_PREVIEW_UNIT = 40
README_UNIT = 32

TRACK_SHARE = Fraction(16, 100)

TITLE = '<title id="t">FerrionOS</title>'


def outline(d):
    """Corners of one closed outline written with M, h, v, H, V and z."""
    tokens = re.findall(r"[A-Za-z]|-?\d+", d)
    x = y = 0
    corners = []
    i = 0
    while i < len(tokens):
        command = tokens[i]
        if command == "M":
            x, y = int(tokens[i + 1]), int(tokens[i + 2])
            i += 3
        elif command in "hHvV":
            value = int(tokens[i + 1])
            if command == "h":
                x += value
            elif command == "H":
                x = value
            elif command == "v":
                y += value
            else:
                y = value
            i += 2
        elif command in "zZ":
            break
        else:
            raise SystemExit(f"mark.svg: unsupported path command {command!r} in {d!r}")
        corners.append((x, y))
    return corners


def covers(corners, px, py):
    """Whether the outline covers the point, by counting crossings to its right."""
    inside = False
    for (x0, y0), (x1, y1) in zip(corners, corners[1:] + corners[:1]):
        if (y0 > py) != (y1 > py) and px < x0:
            inside = not inside
    return inside


def path_data(corners, scale=1, dx=0, dy=0):
    """The outline as path data on whole numbers, scaled and moved."""
    points = [(x * scale + dx, y * scale + dy) for x, y in corners]
    d = f"M{points[0][0]} {points[0][1]}"
    for (x0, y0), (x1, y1) in zip(points, points[1:]):
        d += f"H{x1}" if y0 == y1 else f"V{y1}"
    return d + "z"


def mark_paths(outlines, indent, scale=1, dx=0, dy=0):
    return "\n".join(f'{indent}<path d="{path_data(o, scale, dx, dy)}"/>' for o in outlines)


def blend(ink, ground, share):
    """A color between two 0xRRGGBB values: ink at `share` over ground."""
    mixed = 0
    for shift in (16, 8, 0):
        i, g = (ink >> shift) & 0xFF, (ground >> shift) & 0xFF
        mixed |= round(g + (i - g) * share) << shift
    return mixed


def main():
    missing = [program for program in PROGRAMS if not shutil.which(program)]
    if missing:
        raise SystemExit("generate.py needs these programs on PATH: " + ", ".join(missing))

    tokens = json.loads((HERE / "color.tokens.json").read_text())["ferrion"]["color"]
    ground = tokens["ground"]["$value"]["hex"]
    ink = tokens["ink"]["$value"]["hex"]

    mark = ET.parse(HERE / "mark.svg").getroot()
    columns, rows = (int(n) for n in mark.get("viewBox").split()[2:])
    outlines = [outline(path.get("d")) for path in mark.iter(f"{SVG}path")]
    cells = [[any(covers(o, x + 0.5, y + 0.5) for o in outlines) for x in range(columns)] for y in range(rows)]

    wordmark = ET.parse(HERE / "wordmark.svg").getroot()
    cap_line, baseline = int(wordmark.get("data-cap-line")), int(wordmark.get("data-baseline"))
    name_d = " ".join(wordmark.find(f"{SVG}path").get("d").split())

    written = []

    def target(name):
        path = HERE / name
        path.parent.mkdir(parents=True, exist_ok=True)
        written.append(name)
        return path

    def write(name, text):
        target(name).write_text(text)

    def render(name, width, height, body, upload=False):
        """Writes a PNG of `width` by `height` pixels from SVG content; one that is uploaded is
        also made as small as it gets, without metadata."""
        png = target(name)
        with tempfile.TemporaryDirectory() as scratch:
            svg = Path(scratch) / "image.svg"
            svg.write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}">\n{body}\n</svg>\n')
            subprocess.run(["rsvg-convert", str(svg), "-o", str(png)], check=True)
        if upload:
            subprocess.run(["optipng", "-quiet", "-strip", "all", str(png)], check=True)

    def paper(width, height):
        return f'    <rect width="{width}" height="{height}" fill="{ground}"/>'

    def centred_mark(side, unit, color):
        dx, dy = (side - columns * unit) // 2, (side - rows * unit) // 2
        return f'    <g fill="{color}">\n{mark_paths(outlines, "        ", unit, dx, dy)}\n    </g>'

    for side in (16, 32):
        unit = side // 8
        dx, dy = (side - columns * unit) // 2, (side - rows * unit) // 2
        write(f"mark-{side}.svg", f"""<svg xmlns="http://www.w3.org/2000/svg" width="{side}" height="{side}" viewBox="0 0 {side} {side}" role="img" aria-labelledby="t" fill="currentColor">
    {TITLE}
    <!-- Generated by generate.py from mark.svg. Do not edit. The mark at {unit} pixels per unit: {columns * unit} by {rows * unit} at ({dx}, {dy}). -->
{mark_paths(outlines, "    ", unit, dx, dy)}
</svg>
""")

    write("mark.txt", "".join("".join("##" if cell else "  " for cell in row).rstrip() + "\n" for row in cells))

    ratio = Fraction(NAME_CAP_HEIGHT, UNIT) / (baseline - cap_line)
    name_scale, grid = ratio.numerator, ratio.denominator
    px = Fraction(grid, UNIT)
    lockup_box = f"0 0 {int(LOCKUP_WIDTH * px)} {int(LOCKUP_HEIGHT * px)}"
    name_x = int(NAME_INSET * px)
    name_y = rows * grid + int((MARK_TO_NAME + NAME_CAP_HEIGHT) * px) - baseline * name_scale
    mark_x = (int(LOCKUP_WIDTH * px) - columns * grid) // 2

    def lockup_paths(indent):
        return f"""{mark_paths(outlines, indent, grid, mark_x)}
{indent}<path transform="translate({name_x} {name_y}) scale({name_scale})" d="{name_d}"/>"""

    def lockup_size(unit):
        return LOCKUP_WIDTH * unit // UNIT, LOCKUP_HEIGHT * unit // UNIT

    def lockup_at(x, y, color, unit=UNIT):
        """The lockup inside a larger image, its box at (x, y), at `unit` pixels per unit."""
        width, height = lockup_size(unit)
        return f"""    <svg x="{x}" y="{y}" width="{width}" height="{height}" viewBox="{lockup_box}" fill="{color}">
{lockup_paths("        ")}
    </svg>"""

    def lockup_file(name, unit, fill, what):
        width, height = lockup_size(unit)
        write(name, f"""<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="{lockup_box}" role="img" aria-labelledby="t" fill="{fill}">
    {TITLE}
    <!-- Generated by generate.py from mark.svg and wordmark.svg. Do not edit. {what}: one unit is {grid} here, and the name is drawn at {name_scale} per font unit. -->
{lockup_paths("    ")}
</svg>
""")

    lockup_file("lockup.svg", UNIT, "currentColor", "The mark above the name")

    width, height = SCREEN
    name_baseline = LOGIN_BOX_HEIGHT // 2 + NAME_TO_BOX
    mark_top = name_baseline + NAME_CAP_HEIGHT + MARK_TO_NAME + rows * UNIT
    render("orbdata/usr/share/ui/login.png", width, height,
           paper(width, height) + "\n" + lockup_at((width - LOCKUP_WIDTH) // 2, height // 2 - mark_top, ink))

    subprocess.run(["magick", "-size", f"{width}x{height}", f"xc:{ground}", "-sampling-factor", "4:4:4",
                    "-quality", "100", "-strip", str(target("orbdata/usr/share/ui/background.jpg"))], check=True)

    side, unit = START_ICON
    render("orbdata/usr/share/icons/places/start-here.png", side, side, centred_mark(side, unit, ink))

    side, unit = FAVICON
    render("book/theme/favicon.png", side, side, paper(side, side) + "\n" + centred_mark(side, unit, ink))

    width, height = BOOK_LOGO
    for name, color in (("light", ground), ("dark", ink)):
        render(f"book/src/assets/logo-{name}.png", width, height,
               lockup_at(-NAME_INSET, (height - LOCKUP_HEIGHT) // 2, color))

    width, height = SOCIAL_PREVIEW
    unit = SOCIAL_PREVIEW_UNIT
    mark_to_baseline = (rows * UNIT + MARK_TO_NAME + NAME_CAP_HEIGHT) * unit // UNIT
    render("github/social-preview.png", width, height,
           paper(width, height) + "\n"
           + lockup_at((width - lockup_size(unit)[0]) // 2, (height - mark_to_baseline) // 2, ink, unit),
           upload=True)
    side, unit = AVATAR
    render("github/avatar.png", side, side, paper(side, side) + "\n" + centred_mark(side, unit, ink), upload=True)
    lockup_file("github/readme-dark.svg", README_UNIT, ink, "The mark above the name in ink, for dark pages")
    lockup_file("github/readme-light.svg", README_UNIT, ground, "The mark above the name in the ground color, for light pages")

    ground_rgb, ink_rgb = int(ground[1:], 16), int(ink[1:], 16)
    bitmap = ", ".join("0b" + "".join("1" if cell else "0" for cell in row) for row in cells)
    write("mark.rs", f"""// Generated by branding/generate.py from mark.svg and color.tokens.json. Do not edit.

/// The ground, the ink and the empty part of a progress bar, as 0xRRGGBB.
pub const GROUND: u32 = {ground_rgb:#08x};
pub const INK: u32 = {ink_rgb:#08x};
pub const TRACK: u32 = {blend(ink_rgb, ground_rgb, TRACK_SHARE):#08x};

/// The mark on its grid of {columns} by {rows} units: one byte per row from the top, with the left
/// column in bit {columns - 1}.
pub const MARK_COLUMNS: usize = {columns};
pub const MARK: [u8; {rows}] = [{bitmap}];

/// The login background is {SCREEN[0]} by {SCREEN[1]} pixels, and orblogin scales it to cover the screen.
/// The lengths below are in its pixels: on a screen a length is the larger of
/// `length * width / LOGIN_WIDTH` and `length * height / LOGIN_HEIGHT`.
pub const LOGIN_WIDTH: usize = {SCREEN[0]};
pub const LOGIN_HEIGHT: usize = {SCREEN[1]};

/// One unit of the mark's grid, and the width of the box of the mark and the name.
pub const MARK_UNIT: usize = {UNIT};
pub const LOCKUP_WIDTH: usize = {LOCKUP_WIDTH};

/// Heights above the center of the screen: the top of the mark, then the cap line and the
/// baseline of the name that the login screen sets under it.
pub const MARK_TOP: usize = {mark_top};
pub const NAME_CAP_LINE: usize = {name_baseline + NAME_CAP_HEIGHT};
pub const NAME_BASELINE: usize = {name_baseline};
""")

    write("orbital-background.toml", f"""
# Generated by branding/generate.py from color.tokens.json. Do not edit.
# The ground, as 0xAARRGGBB. Orbital reads a color as a table with a `data` key and falls back to
# its defaults for the whole file when an entry has another shape.
[background_color]
data = 0xff{ground[1:]}
""")

    print("wrote " + ", ".join(written))


if __name__ == "__main__":
    main()
