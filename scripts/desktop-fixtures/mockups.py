#!/usr/bin/env python3
# Builds the mockups of plans/desktop-design.md, at the reference screen of
# 1280 by 800 pixels, from what the identity kit and the desktop image already hold:
#   branding/color.tokens.json   ground and ink, the two base colors
#   branding/mark.svg            the mark
#   Fira Sans and Fira Mono      from the fetched source of the orbdata package
#   symbolic icons               from the fetched source of the cosmic-icons package
# Every other neutral is ground or ink at a fixed share over the other (SHARES below);
# the shares, the one accent and the three colors of the window controls are the ones
# of the maintainer's prototype, named in the plan.
# Each screen is written twice. Beside this file is the rich picture: soft shadows and
# translucent panels, what a GPU renderer draws. Under flat/ is the picture a CPU
# renderer draws: the same boxes with opaque fills and no shadow. The flat files carry
# their regions as <rect> elements under <g id="regions">: the rectangle a part of the
# shell occupies, and probe pixels with the color expected there. The slices of the plan
# compare screenshots of the virtual machine against them.
# Text is outlined, so a mockup looks the same in every viewer.
# Run it after changing a token, a size or a scene:
#   python3 scripts/desktop-fixtures/mockups.py            writes the SVG files here
#   python3 scripts/desktop-fixtures/mockups.py --check    renders flat/ and reads every probe back
#   python3 scripts/desktop-fixtures/mockups.py --tokens   prints the token tables of the plan
# It needs fontTools, and `make f.orbdata f.cosmic-icons` to have fetched the two sources;
# --check also needs rsvg-convert (librsvg) and magick (ImageMagick) on PATH.

import json
import sys
import xml.etree.ElementTree as ET
from fractions import Fraction
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
FONTS = ROOT / "recipes/gui/orbdata/source/usr/share/fonts"
ICONS = ROOT / "recipes/icons/cosmic-icons/source"
SVG_NS = "{http://www.w3.org/2000/svg}"

W, H = 1280, 800  # the reference screen: QEMU's default mode, and scripts/vm-test.py's
BAR = 44  # height of the top bar
GAP = 12  # between tiled windows, and between a window and the edge of the working area
TITLE = 32  # height of the title strip the compositor draws on a free window
RADIUS = 14  # corner radius of a window
PANEL_RADIUS = 16  # corner radius of a shell panel: dock, popovers
ISLAND = (128, 32)  # the island at rest
DOCK_ITEM, DOCK_PAD, DOCK_GAP, DOCK_BOTTOM = 44, 6, 4, 12
DOCK_H = DOCK_ITEM + 2 * DOCK_PAD
DOT = 24  # pitch of the dot grid behind the windows

# The one accent. Amber is the prototype's default; the others are the set it offers.
ACCENT = 0xE68E0D
ACCENTS = {"amber": 0xE68E0D, "blood": 0xC8202A, "ash": 0xA8A29C, "sakura": 0xE0608A, "venom": 0x5CB83A,
           "ice": 0x3A9FD8, "dusk": 0x8A5CF0}
# The three window controls take the red, yellow and green of the terminal's sixteen colors.
CONTROLS = {"dark": (0xD35F5F, 0xE6B450, 0x86B37E), "light": (0xC4453C, 0xB07D10, 0x5F8A12)}

# Shares, in percent, of the foreground over the background: ink over ground in the dark
# theme, ground over ink in the light one.
SHARES = {
    "dark": {"line": 8, "line-strong": 14, "raise": Fraction(11, 2), "raise-2": Fraction(19, 2), "fg-2": 72,
             "muted": 57, "ring": 50, "soft": 16},
    "light": {"line": 10, "line-strong": 18, "raise": 4, "raise-2": 8, "fg-2": 72, "muted": 64, "ring": 50,
              "soft": 14},
}
JOBS = {
    "bg": "the desktop, and the content of a window",
    "well": "what sits lower than its surroundings: a title strip, a sidebar, a field",
    "raise": "what the shell floats above the windows: the island, the dock, the launcher, popovers",
    "raise-2": "a control on a raised surface, a key cap, a neutral selection",
    "fg": "text and glyphs",
    "fg-2": "secondary text",
    "muted": "hints, placeholders, resting glyphs",
    "inactive": "a control of a window that is not the key window",
    "ring": "the border of the focused window, and the edge of a field",
    "accent": "the primary button, the selected row, the text cursor, what wants an answer",
    "accent-ink": "text and glyphs on an accent fill",
}


def blend(top, base, share):
    """A color between two 0xRRGGBB values: `top` at `share` percent over `base`, as branding/generate.py blends."""
    mixed = 0
    for shift in (16, 8, 0):
        t, b = (top >> shift) & 0xFF, (base >> shift) & 0xFF
        mixed |= round(b + (t - b) * Fraction(share) / 100) << shift
    return mixed


def hexa(color):
    return f"#{color:06x}"


def luminance(color):
    total = 0.0
    for shift, weight in ((16, 0.2126), (8, 0.7152), (0, 0.0722)):
        v = ((color >> shift) & 0xFF) / 255
        total += weight * (v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4)
    return total


def contrast(a, b):
    la, lb = sorted((luminance(a), luminance(b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


def readable(color, on, floor=4.5):
    """`color` darkened in whole percents until it reaches `floor` on `on`; itself when it already does."""
    for share in range(0, 101):
        candidate = blend(0x000000, color, share)
        if contrast(candidate, on) >= floor:
            return candidate
    return 0x000000


class Theme:
    """The color roles of one theme, as 0xRRGGBB values. `tier` is "rich" or "flat"."""

    def __init__(self, name, tier="rich"):
        kit = json.loads((ROOT / "branding/color.tokens.json").read_text())["ferrion"]["color"]
        ground, ink = (int(kit[k]["$value"]["hex"][1:], 16) for k in ("ground", "ink"))
        self.name, self.tier, self.s = name, tier, SHARES[name]
        dark = name == "dark"
        self.fg, self.bg = (ink, ground) if dark else (ground, ink)
        s = self.s
        c = {"bg": self.bg, "fg": self.fg}
        c["well"] = blend(0x000000, self.bg, 28) if dark else blend(self.fg, self.bg, 6)
        for role in ("raise", "raise-2", "fg-2", "muted", "ring"):
            c[role] = blend(self.fg, self.bg, s[role])
        c["inactive"] = blend(c["muted"], self.bg, 55)
        c["accent"] = ACCENT if dark else readable(ACCENT, self.bg)
        dark_ink = blend(0x000000, ground, 40)
        c["accent-ink"] = max((dark_ink, 0xFFFFFF), key=lambda ink_: contrast(ink_, c["accent"]))
        c["title"] = blend(c["well"], self.bg, 86)
        # On paper each control is darkened until it reaches 3:1 on the title strip it sits on.
        c["red"], c["yellow"], c["green"] = (readable(v, c["title"], 3) for v in CONTROLS[name])
        c["dot"] = blend(self.fg, self.bg, 10)
        self.c = c

    def __getitem__(self, role):
        return hexa(self.c[role])

    def line(self, on="bg", strong=False):
        """A hairline on the surface `on`, as an opaque color."""
        return hexa(blend(self.fg, self.c[on], self.s["line-strong" if strong else "line"]))

    def soft(self, on="raise"):
        """The accent as a quiet fill on the surface `on`: a selected row."""
        return hexa(blend(self.c["accent"], self.c[on], self.s["soft"]))


def number(v):
    text = f"{v:.2f}".rstrip("0").rstrip(".")
    return "0" if text == "-0" else text


class Face:
    """One font file: advances, kerning pairs, tabular figures and outlines."""

    def __init__(self, tag, path):
        from fontTools.ttLib import TTFont

        self.tag = tag
        self.font = TTFont(path)
        self.cmap = self.font.getBestCmap()
        self.hmtx = self.font["hmtx"]
        self.glyphs = self.font.getGlyphSet()
        self.upem = self.font["head"].unitsPerEm
        self.cap = self.font["OS/2"].sCapHeight / self.upem
        self.tabular = self._single_substitutions("tnum")
        self.pair_lookups = self._pair_lookups()

    def _lookups(self, table, feature):
        indices = []
        for record in table.FeatureList.FeatureRecord:
            if record.FeatureTag == feature:
                indices += [i for i in record.Feature.LookupListIndex if i not in indices]
        return [table.LookupList.Lookup[i] for i in sorted(indices)]

    def _single_substitutions(self, feature):
        mapping = {}
        if "GSUB" in self.font:
            for lookup in self._lookups(self.font["GSUB"].table, feature):
                for sub in lookup.SubTable:
                    mapping.update(getattr(getattr(sub, "ExtSubTable", sub), "mapping", {}))
        return mapping

    def _pair_lookups(self):
        lookups = []
        if "GPOS" in self.font:
            for lookup in self._lookups(self.font["GPOS"].table, "kern"):
                subs = [getattr(sub, "ExtSubTable", sub) for sub in lookup.SubTable]
                lookups.append([(sub, set(sub.Coverage.glyphs)) for sub in subs if hasattr(sub, "Coverage")])
        return lookups

    def kern(self, left, right):
        """The font's own pair adjustment, in font units."""
        total = 0
        for subs in self.pair_lookups:
            for sub, covered in subs:
                if left not in covered:
                    continue
                if sub.Format == 1:
                    pairs = sub.PairSet[sub.Coverage.glyphs.index(left)].PairValueRecord
                    hit = next((p for p in pairs if p.SecondGlyph == right), None)
                    if hit is None:
                        continue
                    value = hit.Value1
                else:
                    first = sub.ClassDef1.classDefs.get(left, 0)
                    second = sub.ClassDef2.classDefs.get(right, 0)
                    value = sub.Class1Record[first].Class2Record[second].Value1
                total += (getattr(value, "XAdvance", 0) or 0) if value is not None else 0
                break
        return total

    def shape(self, text, tabular=False):
        """Glyph names with their x offsets in font units, and the advance of the whole run."""
        names = []
        for ch in text:
            if ord(ch) not in self.cmap:
                sys.exit(f"mockups: {self.tag} has no glyph for {ch!r} (U+{ord(ch):04X}) in {text!r}")
            name = self.cmap[ord(ch)]
            names.append(self.tabular.get(name, name) if tabular else name)
        run, x = [], 0
        for i, name in enumerate(names):
            if i:
                x += self.kern(names[i - 1], name)
            run.append((name, x))
            x += self.hmtx[name][0]
        return run, x

    def outline(self, name):
        from fontTools.pens.svgPathPen import SVGPathPen

        pen = SVGPathPen(self.glyphs, ntos=number)
        self.glyphs[name].draw(pen)
        return pen.getCommands()


# Shadows of the rich picture, as the prototype sets them: (dy, blur radius, opacity) per layer.
SHADOWS = {
    "dark": {"win": ((10, 32, 0.32), (2, 6, 0.18)), "panel": ((16, 40, 0.42),)},
    "light": {"win": ((10, 28, 0.16), (1, 4, 0.10)), "panel": ((16, 40, 0.14),)},
}
PANEL_ALPHA = 0.78  # a shell panel over what is behind it, where the renderer can blur


class Canvas:
    """One mockup: the drawing, the glyph outlines it uses, and its regions."""

    def __init__(self, theme, faces, title, description):
        self.t, self.faces, self.title, self.description = theme, faces, title, description
        self.body, self.glyph_defs, self.other_defs, self.regions = [], {}, [], []
        self.clips = 0
        self.rich = theme.tier == "rich"
        self.filters = set()

    # Text

    def measure(self, text, face="r", size=13, tabular=False):
        _, advance = self.faces[face].shape(text, tabular)
        return advance * size / self.faces[face].upem

    def text(self, x, y, text, face="r", size=13, fill="fg", anchor="start", tabular=False):
        """Sets `text` with its baseline at `y`; returns its width in pixels."""
        f = self.faces[face]
        run, advance = f.shape(text, tabular)
        scale = size / f.upem
        width = advance * scale
        x -= {"start": 0, "middle": width / 2, "end": width}[anchor]
        uses = []
        for name, offset in run:
            d = self.glyph_defs.setdefault((face, name), f.outline(name))
            if d:
                uses.append(f'<use href="#{face}-{name.replace(".", "_")}" x="{offset}"/>')
        label = text.replace("&", "&amp;").replace('"', "&quot;").replace("<", "&lt;")
        self.body.append(f'<g transform="translate({number(x)} {number(y)}) scale({scale:.5f} -{scale:.5f})" '
                         f'fill="{self.color(fill)}" aria-label="{label}">{"".join(uses)}</g>')
        return width

    def baseline(self, top, height, face="r", size=13):
        """The baseline that centers the capitals of a line of text in a box."""
        return top + height / 2 + self.faces[face].cap * size / 2

    # Shapes

    def color(self, value):
        return value if value.startswith("#") else self.t[value]

    def rect(self, x, y, w, h, r=0, fill=None, stroke=None, opacity=None):
        """A box; a `stroke` is one pixel wide and drawn inside it."""
        attrs = f' opacity="{opacity}"' if opacity is not None else ""
        if stroke:
            x, y, w, h, r = x + 0.5, y + 0.5, w - 1, h - 1, max(0, r - 0.5)
            attrs += f' stroke="{self.color(stroke)}"'
        attrs += f' fill="{self.color(fill)}"' if fill else ' fill="none"'
        radius = f' rx="{number(r)}"' if r else ""
        self.body.append(f'<rect x="{number(x)}" y="{number(y)}" width="{number(w)}" height="{number(h)}"{radius}{attrs}/>')

    def shadow(self, x, y, w, h, r, kind):
        """The soft shadow under a box, in the rich picture only."""
        if not self.rich:
            return
        if kind not in self.filters:
            self.filters.add(kind)
            layers = SHADOWS[self.t.name][kind]
            parts = []
            for i, (dy, blur, alpha) in enumerate(layers):
                parts.append(f'<feGaussianBlur in="SourceAlpha" stdDeviation="{number(blur / 2)}" result="b{i}"/>'
                             f'<feOffset in="b{i}" dy="{dy}" result="o{i}"/>'
                             f'<feComponentTransfer in="o{i}" result="s{i}"><feFuncA type="linear" slope="{alpha}"/>'
                             f"</feComponentTransfer>")
            merge = "".join(f'<feMergeNode in="s{i}"/>' for i in range(len(layers)))
            self.other_defs.append(f'<filter id="shadow-{kind}" x="-30%" y="-30%" width="160%" height="180%" '
                                   f'color-interpolation-filters="sRGB">{"".join(parts)}<feMerge>{merge}</feMerge></filter>')
        self.body.append(f'<rect x="{number(x)}" y="{number(y)}" width="{number(w)}" height="{number(h)}" '
                         f'rx="{number(r)}" fill="#000000" filter="url(#shadow-{kind})"/>')

    def panel(self, x, y, w, h, r=PANEL_RADIUS, shadow=True, glass=True):
        """A surface the shell floats above the windows: translucent over a shadow where the renderer can
        afford it, the same box opaque where it cannot."""
        if shadow:
            self.shadow(x, y, w, h, r, "panel")
        self.rect(x, y, w, h, r=r, fill="raise", opacity=PANEL_ALPHA if (self.rich and glass) else None)
        self.rect(x, y, w, h, r=r, stroke=self.t.line("raise"))

    def circle(self, cx, cy, r, fill=None, stroke=None, width=1.5):
        attrs = f' fill="{self.color(fill)}"' if fill else ' fill="none"'
        if stroke:
            attrs += f' stroke="{self.color(stroke)}" stroke-width="{number(width)}"'
        self.body.append(f'<circle cx="{number(cx)}" cy="{number(cy)}" r="{number(r)}"{attrs}/>')

    def arc(self, cx, cy, r, share, stroke, width=1.5):
        """A part of a ring, clockwise from the top: progress drawn by how much of it is there."""
        import math

        angle = 2 * math.pi * share
        ex, ey = cx + r * math.sin(angle), cy - r * math.cos(angle)
        self.body.append(f'<path d="M{number(cx)} {number(cy - r)}A{number(r)} {number(r)} 0 {int(share > 0.5)} 1 '
                         f'{number(ex)} {number(ey)}" fill="none" stroke="{self.color(stroke)}" '
                         f'stroke-width="{number(width)}" stroke-linecap="round"/>')

    def icon(self, name, x, y, fill="fg-2", size=16):
        """A symbolic icon of the cosmic-icons package in a box of `size` pixels."""
        hits = sorted(ICONS.rglob(f"{name}-symbolic.svg"))
        if not hits:
            sys.exit(f"mockups: no {name}-symbolic.svg under {ICONS.relative_to(ROOT)}; run `make f.cosmic-icons`")
        root = ET.parse(hits[0]).getroot()
        _, _, vw, vh = (float(v) for v in root.attrib["viewBox"].split())
        scale = size / 16
        parts, color = [], self.color(fill)
        for el in root.iter(SVG_NS + "path"):
            if float(el.attrib.get("fill-opacity", 1)) < 0.05:
                continue  # the transparent square that pads every icon to its box
            attrs = f' d="{el.attrib["d"]}"'
            for key in ("fill-rule", "clip-rule", "opacity", "stroke-width", "stroke-linecap"):
                if key in el.attrib:
                    attrs += f' {key}="{el.attrib[key]}"'
            if "stroke" in el.attrib:
                attrs += f' stroke="{color}"'
                attrs += "" if el.attrib.get("fill", "none") != "none" else ' fill="none"'
            parts.append(f"<path{attrs}/>")
        # A few icons are drawn in a box one pixel wider or taller than 16: center the box.
        dx, dy = x - (vw - 16) / 2 * scale, y - (vh - 16) / 2 * scale
        self.body.append(f'<g transform="translate({number(dx)} {number(dy)}) scale({number(scale)})" '
                         f'fill="{color}">{"".join(parts)}</g>')

    def mark(self, x, y, unit, fill="fg"):
        """The mark with its top left corner at (x, y), at `unit` pixels per unit of its grid."""
        root = ET.parse(ROOT / "branding/mark.svg").getroot()
        paths = "".join(f'<path d="{p.attrib["d"]}"/>' for p in root.iter(SVG_NS + "path"))
        self.body.append(f'<g transform="translate({number(x)} {number(y)}) scale({number(unit)})" '
                         f'fill="{self.color(fill)}">{paths}</g>')

    def keycap(self, right, top, label, height=20, fill="raise-2"):
        """A key cap whose right edge is at `right`; returns its left edge."""
        width = max(height, self.measure(label, "m", 11) + 12)
        self.rect(right - width, top, width, height, r=6, fill=fill)
        self.text(right - width / 2, self.baseline(top, height, "m", 11), label, "m", 11, "fg-2", "middle")
        return right - width

    def mode(self, x, y, mode, fill, behind):
        """The glyph of a workspace's layout in a box of 12 pixels: two overlapping windows, or three columns."""
        if mode == "free":
            self.rect(x, y + 4, 8, 8, r=2, stroke=fill)
            self.rect(x + 4, y, 8, 8, r=2, fill=behind, stroke=fill)
        else:
            for i in range(3):
                self.rect(x + i * 4.5, y, 3, 12, r=1, fill=fill)

    # Structure

    def clip(self, x, y, w, h, r=0):
        """Opens a group clipped to a box; close it with `end`."""
        self.clips += 1
        self.other_defs.append(f'<clipPath id="clip{self.clips}"><rect x="{number(x)}" y="{number(y)}" '
                               f'width="{number(w)}" height="{number(h)}" rx="{number(r)}"/></clipPath>')
        self.body.append(f'<g clip-path="url(#clip{self.clips})">')

    def group(self, transform):
        self.body.append(f'<g transform="{transform}">')

    def end(self):
        self.body.append("</g>")

    def region(self, name, x, y, w, h, probes=()):
        """Names the box a part occupies; each probe is (x, y, role or color) of a pixel inside a flat fill."""
        marks = " ".join(f"{int(px)},{int(py)}:{self.color(role)}" for px, py, role in probes)
        self.regions.append(f'<rect id="region-{name}" x="{number(x)}" y="{number(y)}" width="{number(w)}" '
                            f'height="{number(h)}" data-probes="{marks}"/>')

    def write(self, name):
        glyphs = "".join(f'<path id="{face}-{glyph.replace(".", "_")}" d="{d}"/>'
                         for (face, glyph), d in sorted(self.glyph_defs.items()) if d)
        tier = ("the rich picture: what a GPU renderer draws" if self.rich
                else "the flat picture: what a CPU renderer draws, with the regions a check compares")
        out = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" '
            f'aria-labelledby="t d" data-theme="{self.t.name}" data-tier="{self.t.tier}">',
            f'<title id="t">{self.title}</title>',
            f'<desc id="d">{self.description}</desc>',
            "<!--",
            "    Generated by mockups.py from branding/color.tokens.json and branding/mark.svg. Do not edit.",
            "    A mockup of the plan in plans/desktop-design.md, at the reference screen of 1280 by 800 pixels;",
            f"    {tier}.",
            "    Text is outlined from Fira Sans and Fira Mono, the faces the orbdata package ships:",
            "    Digitized data copyright (c) 2012-2015, The Mozilla Foundation and Telefonica S.A.,",
            "    licensed under the SIL Open Font License, Version 1.1.",
            "    Icons are the symbolic icons of the cosmic-icons package, (c) System76,",
            "    licensed under the Creative Commons Attribution-ShareAlike 4.0 International license,",
            "    https://github.com/pop-os/cosmic-icons; they are recolored here, and this file's copies of them",
            "    stay under that license.",
            "-->",
            f"<defs>{glyphs}{''.join(self.other_defs)}</defs>",
            *self.body,
        ]
        if not self.rich:
            out.append(f'<g id="regions" display="none">{"".join(self.regions)}</g>')
        out.append("</svg>")
        folder = HERE if self.rich else HERE / "flat"
        folder.mkdir(exist_ok=True)
        (folder / name).write_text("\n".join(out) + "\n")


# The desktop behind the windows


def wallpaper(c, x=0, y=0, w=W, h=H):
    """The plain ground with a quiet grid of dots: something for a shadow and a translucent panel to sit on."""
    c.rect(x, y, w, h, fill="bg")
    if "dots" not in c.filters:
        c.filters.add("dots")
        c.other_defs.append(f'<pattern id="dots" width="{DOT}" height="{DOT}" patternUnits="userSpaceOnUse">'
                            f'<rect x="11" y="11" width="2" height="2" fill="{c.t["dot"]}"/></pattern>')
    c.body.append(f'<rect x="{number(x)}" y="{number(y)}" width="{number(w)}" height="{number(h)}" fill="url(#dots)"/>')


def clear_of_dots(x, y):
    """Whether a probe at (x, y) misses the dot grid."""
    return x % DOT not in (11, 12) and y % DOT not in (11, 12)


# The parts of the shell


def island(c, cx, top, size=ISLAND):
    """The island: a capsule on the bar's axis. Returns its left edge."""
    w, h = size
    x = cx - w / 2
    c.rect(x, top, w, h, r=h / 2, fill="raise")
    c.rect(x, top, w, h, r=h / 2, stroke=c.t.line("raise", strong=True))
    return x


def bar(c, app, workspaces):
    """The top bar: the mark, the name of the app in use, the workspaces, the island and the status.
    `workspaces` is a list of (mode, icons, focused index or None); the first one is the active one."""
    t = c.t
    c.mark(16, 15, 2)  # 8 by 14 pixels, as branding/mark-16.svg draws it
    x = 36
    if app:
        x += c.text(x, c.baseline(0, BAR, "s"), app, "s") + 16
    first = x
    for index, (mode, icons, focused) in enumerate(workspaces):
        active = index == 0
        width = 28 + (20 * len(icons) + 4 if icons else 0)
        if active:
            c.rect(x, 9, width, 26, r=8, fill="raise")
            c.rect(x, 9, width, 26, r=8, stroke=t.line("raise", strong=True))
        c.mode(x + 8, 16, mode, "fg" if active else "muted", "raise" if active else "bg")
        for i, icon in enumerate(icons):
            c.icon(icon, x + 28 + i * 20, 14, "fg" if (active and i == focused) else "muted")
        x += width + 4
    c.region("bar", 0, 0, W, BAR, [(4, 4, "bg"), (304, 6, "bg"), (1000, 6, "bg"), (17, 16, "fg")])
    c.region("workspaces", first, 9, x - 4 - first, 26, [(first + 3, 22, "raise")])
    left = island(c, W / 2, 6)
    c.region("island", left, 6, ISLAND[0], ISLAND[1], [(W // 2, 22, "raise")])
    clock = "14:07"
    right = W - 16
    right -= c.text(right, c.baseline(0, BAR), clock, anchor="end", tabular=True) + 8
    right -= c.text(right, c.baseline(0, BAR), "Sat 21 Nov", fill="fg-2", anchor="end") + 16
    c.icon("audio-volume-high", right - 16, 14)
    c.icon("network-wired", right - 44, 14)
    c.region("status", right - 52, 9, W - 8 - (right - 52), 26)


def dock(c, pinned, running=(), here=(), minimized=()):
    """The dock, centered above the bottom edge: a symbolic glyph stands for each app's own icon."""
    t = c.t
    count = len(pinned) + len(minimized)
    width = 2 * DOCK_PAD + count * DOCK_ITEM + (count - 1) * DOCK_GAP + (13 if minimized else 0)
    x, y = round(W / 2 - width / 2), H - DOCK_BOTTOM - DOCK_H
    c.panel(x, y, width, DOCK_H)
    cx = x + DOCK_PAD
    for icon in pinned:
        c.icon(icon, cx + 8, y + DOCK_PAD + 6, "fg", 28)
        if icon in here:
            c.rect(cx + 16, y + DOCK_H - 6, 12, 4, r=2, fill="accent")
        elif icon in running:
            c.circle(cx + 22, y + DOCK_H - 4, 2, fill="muted")
        cx += DOCK_ITEM + DOCK_GAP
    if minimized:
        c.rect(cx + 2, y + 14, 1, 28, fill=t.line("raise", strong=True))
        cx += 13
        for icon in minimized:
            c.rect(cx + 4, y + DOCK_PAD + 4, 36, 28, r=6, fill="bg", stroke=t.line("bg", strong=True))
            c.icon(icon, cx + 14, y + DOCK_PAD + 10, "muted")
            cx += DOCK_ITEM + DOCK_GAP
    c.region("dock", x, y, width, DOCK_H, [(x + 3, y + 28, "raise")])
    return x, y, width


PINNED = ["system-file-manager", "utilities-terminal", "web-browser", "format-indent-more",
          "accessories-text-editor", "preferences-system"]


# Windows


def title_strip(c, x, y, w, title, focused):
    """The strip the compositor draws on a free window: three round controls at the left, the title centered."""
    t = c.t
    c.rect(x, y, w, TITLE, fill="title")
    c.rect(x, y + TITLE, w, 1, fill=t.line("bg"))
    for i, role in enumerate(("red", "yellow", "green")):
        c.circle(x + 18 + i * 20, y + 16, 6, fill=role if focused else "raise-2")
    c.text(x + w / 2, c.baseline(y, TITLE, "r", 12), title, "r", 12, "fg-2" if focused else "muted", "middle")


def window(c, x, y, w, h, title, focused, body, strip=True):
    """A window: its shadow, its strip when the compositor draws one, its content and its border."""
    c.shadow(x, y, w, h, RADIUS, "win")
    c.clip(x, y, w, h, RADIUS)
    c.rect(x, y, w, h, fill="bg")
    top = y
    if strip:
        title_strip(c, x, y, w, title, focused)
        top = y + TITLE + 1
    body(c, x, top, w, y + h - top, focused)
    c.end()
    c.rect(x, y, w, h, r=RADIUS, stroke="ring" if focused else c.t.line("bg", strong=True))


def files_body(c, x, top, w, h, focused):
    t = c.t
    side = 132
    c.rect(x, top, side, h, fill="well")
    c.rect(x + side, top, 1, h, fill=t.line("bg"))
    c.text(x + 16, top + 22, "Places", "s", 11, "muted")
    places = [("user-home", "Home"), ("folder-documents", "Documents"), ("folder-download", "Downloads"),
              ("folder-pictures", "Pictures")]
    for i, (icon, name) in enumerate(places):
        row = top + 32 + i * 28
        if i == 0:
            c.rect(x + 8, row, side - 16, 28, r=8, fill="raise-2")
        c.icon(icon, x + 16, row + 6, "accent" if i == 0 else "muted")
        c.text(x + 40, c.baseline(row, 28), name, fill="fg" if i == 0 else "fg-2")
    left, right = x + side + 1 + 16, x + w - 16
    c.text(left, top + 26, "Home", "s", 13)
    items = [("folder", "Documents", "6 items"), ("folder", "Downloads", "2 items"), ("folder", "Pictures", "14 items"),
             ("folder", "Projects", "3 items"), ("text-x-generic", "hello.rs", "78 B"),
             ("text-x-generic", "notes.md", "2.1 kB")]
    for i, (icon, name, meta) in enumerate(items):
        row = top + 40 + i * 32
        c.icon(icon, left, row + 8, "muted")
        c.text(left + 24, c.baseline(row, 32), name)
        c.text(right, c.baseline(row, 32), meta, "m", 12, "muted", "end")


CODE = [
    [("use", "fg"), (" std::fs;", "fg-2")],
    [],
    [("fn", "fg"), (" main() {", "fg-2")],
    [("    let", "fg"), (" path = ", "fg-2"), ('"/usr/lib/os-release"', "fg"), (";", "fg-2")],
    [("    let", "fg"), (" release = fs::read_to_string(path)", "fg-2")],
    [("        .unwrap_or_default();", "fg-2")],
    [("    let", "fg"), (" name = release", "fg-2")],
    [("        .lines()", "fg-2")],
    [("        .find_map(|line| line.strip_prefix(", "fg-2"), ('"NAME="', "fg"), ("))", "fg-2")],
    [("        .unwrap_or(", "fg-2"), ('"FerrionOS"', "fg"), (");", "fg-2")],
    [("    println!(", "fg-2"), ('"Hello from {name}!"', "fg"), (");", "fg-2")],
    [("}", "fg-2")],
]
CURSOR_LINE = 11  # the line the text cursor is on, counted from 1


def editor_body(c, x, top, w, h, focused):
    t = c.t
    side = 148
    c.rect(x, top, side, h, fill="well")
    c.rect(x + side, top, 1, h, fill=t.line("bg"))
    c.text(x + 16, top + 22, "ferrion-hello", "s", 11, "muted")
    tree = [(0, "folder", "src"), (1, "text-x-generic", "main.rs"), (0, "text-x-generic", "Cargo.toml"),
            (0, "text-x-generic", "Cargo.lock")]
    for i, (depth, icon, name) in enumerate(tree):
        row = top + 32 + i * 28
        current = name == "main.rs"
        if current:
            c.rect(x + 8, row, side - 16, 28, r=8, fill="raise-2")
        c.icon(icon, x + 16 + depth * 16, row + 6, "accent" if current else "muted")
        c.text(x + 40 + depth * 16, c.baseline(row, 28), name, fill="fg" if current else "fg-2")
    left = x + side + 1
    c.rect(left, top, w, 32, fill="well")
    c.rect(left, top + 32, w, 1, fill=t.line("bg"))
    tab = c.measure("main.rs") + 32
    c.rect(left, top, tab, 33, fill="bg")
    c.rect(left + tab, top, 1, 32, fill=t.line("bg"))
    c.text(left + 16, c.baseline(top, 32), "main.rs")
    code_top = top + 33 + 12
    for i, line in enumerate(CODE):
        base = code_top + i * 18 + 13
        c.text(left + 36, base, str(i + 1), "m", 12, "inactive", "end")
        cx = left + 52
        for text, role in line:
            cx += c.text(cx, base, text, "m", 12, role)
        if i + 1 == CURSOR_LINE and focused:
            c.rect(cx, base - 12, 1.5, 16, fill="accent")
    status = top + h - 24
    c.rect(left, status, w, 24, fill="well")
    c.rect(left, status, w, 1, fill=t.line("bg"))
    c.text(left + 16, c.baseline(status, 24, "m", 11), f"Ln {CURSOR_LINE}, Col 36", "m", 11, "muted")
    c.text(x + w - 16, c.baseline(status, 24, "r", 11), "Rust", "r", 11, "muted", "end")


SHELL = [
    [("~>", "muted"), (" ls /usr/share/ui | sort-by size", "fg")],
    [(" # \u2502 name           \u2502 type \u2502    size", "muted")],
    [(" 0 \u2502 login.png      \u2502 file \u2502  8.9 kB", "fg")],
    [(" 1 \u2502 background.jpg \u2502 file \u2502 12.3 kB", "fg")],
    [("~>", "muted"), (" ", "fg")],
]


def terminal_body(c, x, top, w, h, focused):
    for i, line in enumerate(SHELL):
        cx = x + 12
        for text, role in line:
            cx += c.text(cx, top + 12 + i * 18 + 13, text, "m", 12, role)


def browser_body(c, x, top, w, h, focused):
    t = c.t
    c.rect(x, top, w, 40, fill="well")
    c.rect(x, top + 40, w, 1, fill=t.line("bg"))
    c.rect(x + 12, top + 6, w - 24, 28, r=8, fill="bg", stroke=t.line("bg", strong=True))
    c.text(x + 24, c.baseline(top + 6, 28), "The FerrionOS Book", fill="fg-2")
    c.text(x + 48, top + 96, "The FerrionOS Book", "s", 17)
    for i, line in enumerate(("Introduction", "Getting started", "Running in a virtual machine")):
        c.text(x + 48, top + 132 + i * 24, line, fill="fg-2")


# Where the three windows of the first workspace sit when it is free: (x, y, w, h, title, focused, body).
FREE = [
    (96, 84, 520, 400, "Files", False, files_body),
    (760, 72, 440, 300, "Terminal", False, terminal_body),
    (330, 170, 720, 500, "ferrion-hello \u2014 main.rs", True, editor_body),
]


def free_windows(c):
    for x, y, w, h, title, focused, body in FREE:
        window(c, x, y, w, h, title, focused, body)


def column_width(share):
    """A column's width from its share of the screen: gaps are taken out so that shares add up, as niri does it."""
    return round((W - GAP) * share - GAP)


def columns(c):
    """The same three windows as columns of one strip; the third runs past the right edge of the screen."""
    top, height = BAR + GAP, H - BAR - 2 * GAP
    third, half = column_width(Fraction(1, 3)), column_width(Fraction(1, 2))
    x1, x2, x3 = GAP, GAP + third + GAP, GAP + third + GAP + half + GAP
    window(c, x1, top, third, height, "Files", False, files_body, strip=False)
    window(c, x2, top, half, height, "ferrion-hello \u2014 main.rs", True, editor_body, strip=False)
    window(c, x3, top, half, height, "Terminal", False, terminal_body, strip=False)
    return (x1, top, third, height), (x2, top, half, height), (x3, top, half, height)


APPS = ["system-file-manager", "utilities-terminal", "format-indent-more"]  # of the first workspace, in order
RUNNING = ("system-file-manager", "utilities-terminal", "format-indent-more", "web-browser")


# The screens


def desktop_traditional(theme, faces):
    c = Canvas(theme, faces, "FerrionOS desktop with free windows",
               "A workspace whose windows are placed freely and overlap: Files and a terminal behind the code "
               "editor, which has focus. Each window has a title strip with three round controls at its left. "
               "The top bar holds the mark, the name of the app in use, the workspaces, the island and the "
               "status; the dock is on the bottom edge.")
    wallpaper(c)
    free_windows(c)
    bar(c, "Zed", [("free", APPS, 2), ("columns", ["web-browser"], None), ("free", [], None)])
    dock(c, PINNED, running=RUNNING, here=APPS, minimized=["accessories-text-editor"])
    line = c.t.line("bg", strong=True)
    c.region("window-files", 96, 84, 520, 400, [(96, 284, line), (296, 92, "title"), (596, 144, "bg")])
    c.region("window-terminal", 760, 72, 440, 300, [(760, 120, line), (820, 80, "title"), (1180, 192, "bg")])
    c.region("window-editor", 330, 170, 720, 500, [(330, 420, "ring"), (430, 178, "title"), (1030, 470, "bg"),
                                                    (348, 186, "red")])
    return c


def desktop_tiling(theme, faces):
    c = Canvas(theme, faces, "FerrionOS desktop with windows in columns",
               "The same workspace switched to columns: Files, the code editor with focus, and a terminal that "
               "runs past the right edge of the screen, side by side on a strip that scrolls. No window "
               "overlaps another, and the dock is out of the way.")
    wallpaper(c)
    files, editor, terminal = columns(c)
    bar(c, "Zed", [("columns", ["system-file-manager", "format-indent-more", "utilities-terminal"], 1),
                   ("columns", ["web-browser"], None), ("free", [], None)])
    line = c.t.line("bg", strong=True)
    for name, (x, y, w, h), edge, probes in (
            ("window-files", files, line, [(60, 700, "well"), (405, 10, "bg")]),
            ("window-editor", editor, "ring", [(60, 600, "well"), (600, 400, "bg")]),
            ("window-terminal", terminal, line, [(100, 400, "bg")])):
        visible = min(w, W - x)
        c.region(name, x, y, visible, h, [(x, y + h // 2, edge)] + [(x + dx, y + dy, role) for dx, dy, role in probes])
    c.region("gap", 0, BAR, W, GAP, [(200, BAR + 6, "bg"), (800, BAR + 6, "bg")])
    return c


def overview(theme, faces):
    c = Canvas(theme, faces, "FerrionOS overview",
               "The overview of a workspace with free windows: its three windows spread into one layer so that "
               "none covers another, each with its name; the workspaces as small pictures in a column at the "
               "left; the dock.")
    t = c.t
    c.rect(0, 0, W, H, fill="well")
    # The column of workspaces: the vertical order of the desktop, at a glance.
    rail_x, thumb_w, thumb_h = 24, 168, 105
    zoom = thumb_w / W
    cards = []
    for index, (label, draw) in enumerate((("1", free_windows), ("2", None), ("3", None))):
        y = 76 + index * (thumb_h + 36)
        c.clip(rail_x, y, thumb_w, thumb_h, 10)
        c.rect(rail_x, y, thumb_w, thumb_h, fill="bg")
        c.group(f"translate({rail_x} {number(y)}) scale({zoom:.5f})")
        if index == 0:
            free_windows(c)
        elif index == 1:
            width = column_width(Fraction(2, 3))
            window(c, (W - width) // 2, BAR + GAP, width, H - BAR - 2 * GAP, "The FerrionOS Book", True,
                   browser_body, strip=False)
        c.end()
        c.end()
        c.rect(rail_x, y, thumb_w, thumb_h, r=10, stroke="ring" if index == 0 else t.line("well", strong=True))
        c.mode(rail_x + 2, y + thumb_h + 9, "columns" if index == 1 else "free", "fg" if index == 0 else "muted", "well")
        c.text(rail_x + 22, y + thumb_h + 20, f"Workspace {label}", "s" if index == 0 else "r", 11,
               "fg" if index == 0 else "muted")
        cards.append((rail_x, y))
    # The windows of this workspace in one layer, in the order they sit from left to right.
    scale, gap = 0.54, 40
    order = sorted(FREE, key=lambda win: win[0])
    total = sum(win[2] for win in order) * scale + gap * (len(order) - 1)
    x = round(224 + (1032 - total) / 2)
    spread = []
    for wx, wy, ww, wh, title, focused, body in order:
        w, h = ww * scale, wh * scale
        y = 388 - h / 2
        c.group(f"translate({number(x)} {number(y)}) scale({scale})")
        window(c, 0, 0, ww, wh, title, focused, body)
        c.end()
        c.text(x + w / 2, y + h + 24, title, "s" if focused else "r", 13, "fg" if focused else "fg-2", "middle")
        spread.append((title, x, y, w, h, focused))
        x += w + gap
    bar(c, "Zed", [("free", APPS, 2), ("columns", ["web-browser"], None), ("free", [], None)])
    c.regions.clear()
    dock(c, PINNED, running=RUNNING, here=APPS, minimized=["accessories-text-editor"])
    c.region("backdrop", 0, BAR, W, H - BAR, [(700, 60, "well"), (1260, 400, "well"), (208, 700, "well")])
    c.region("workspace-1", cards[0][0], cards[0][1], thumb_w, thumb_h, [(rail_x + 4, cards[0][1] + 50, "bg")])
    c.region("workspace-2", cards[1][0], cards[1][1], thumb_w, thumb_h, [(rail_x + 4, cards[1][1] + 50, "bg")])
    for name, (title, x, y, w, h, focused) in zip(("spread-files", "spread-editor", "spread-terminal"), spread):
        c.region(name, x, y, w, h, [(x + w / 2, y + h - 4, "bg" if name != "spread-editor" else "well")])
    return c


def launcher(theme, faces):
    c = desktop_traditional(theme, faces)
    c.title = "FerrionOS launcher"
    c.description = ("The launcher, opened with the Super key over the desktop: a search field, the apps used "
                     "most, three commands with their shortcuts, and the keys that act on the selected row. It "
                     "hangs under the island it came out of.")
    t = c.t
    c.regions.clear()
    c.rect(0, 0, W, H, fill="bg", opacity=0.5)
    x, y, w, h = 260, 56, 760, 480
    c.panel(x, y, w, h, r=30)
    c.icon("system-search", x + 24, y + 22, "muted", 20)
    c.rect(x + 56, y + 21, 1.5, 22, fill="accent")
    c.text(x + 64, c.baseline(y, 64, "r", 17), "Search apps, settings and commands", "r", 17, "muted")
    c.rect(x + 1, y + 64, w - 2, 1, fill=t.line("raise"))
    apps = [("system-file-manager", "Files"), ("utilities-terminal", "Terminal"), ("format-indent-more", "Zed"),
            ("preferences-system", "Settings")]
    commands = [(None, "Arrange this workspace in columns", ["Super", "Shift", "Space"]),
                ("system-lock-screen", "Lock the screen", ["Super", "Shift", "Esc"]),
                ("input-keyboard", "Show all shortcuts", ["Super", "/"])]
    row = y + 64 + 12
    c.text(x + 28, row + 19, "Used most", "s", 11, "muted")
    row += 28
    selected = row
    for i, (icon, name) in enumerate(apps):
        if i == 0:
            c.rect(x + 12, row, w - 24, 40, r=12, fill=t.soft("raise"))
        c.icon(icon, x + 26, row + 8, "accent" if i == 0 else "fg", 24)
        c.text(x + 64, c.baseline(row, 40), name, "s" if i == 0 else "r")
        c.text(x + w - 28, c.baseline(row, 40), "App", fill="muted", anchor="end")
        row += 40
    c.text(x + 28, row + 19, "Commands", "s", 11, "muted")
    row += 28
    for icon, name, keys in commands:
        if icon:
            c.icon(icon, x + 26, row + 8, "fg", 24)
        else:
            c.mode(x + 32, row + 14, "columns", "fg", "raise")
        c.text(x + 64, c.baseline(row, 40), name)
        right = x + w - 28
        for key in reversed(keys):
            right = c.keycap(right, row + 10, key) - 4
        row += 40
    foot = y + h - 44
    c.rect(x + 1, foot, w - 2, 1, fill=t.line("raise"))
    base = c.baseline(foot, 44, "r", 11)
    right = c.keycap(x + w - 24, foot + 12, "K") - 4
    right = c.keycap(right, foot + 12, "Ctrl") - 8
    right -= c.text(right, base, "Actions", "r", 11, "muted", "end") + 20
    right = c.keycap(right, foot + 12, "Enter") - 8
    c.text(right, base, "Open", "r", 11, "muted", "end")
    c.region("launcher", x, y, w, h, [(x + 200, y + 6, "raise"), (x + 100, y + h - 20, "raise")])
    c.region("launcher-field", x, y, w, 64, [(x + 700, y + 32, "raise")])
    c.region("launcher-selected", x + 12, selected, w - 24, 40, [(x + 400, selected + 6, t.soft("raise"))])
    return c


def lock(theme, faces):
    """The lock screen, which keeps the dark roles in both themes: the boot path is one color."""
    c = Canvas(theme, faces, "FerrionOS lock screen",
               "The lock screen: the mark where the boot splash and the login screen draw it, a large quiet "
               "clock, the date, the name of the person whose session is locked, and one password field.")
    t = c.t
    c.rect(0, 0, W, H, fill="bg")
    c.mark(608, 86, 16)  # where branding/mark.rs puts it: 314 pixels above the center of the screen
    base = 230 + faces["r"].cap * 96  # the cap line of the name on the login screen is the cap line of the time
    c.text(W / 2, base, "14:07", "r", 96, anchor="middle", tabular=True)
    c.text(W / 2, base + 36, "Saturday, 21 November", fill="fg-2", anchor="middle")
    field = 382
    c.text(W / 2, field - 16, "Ada Moreau", "s", anchor="middle")
    c.rect(512, field, 256, 36, r=18, fill="well", stroke="ring")
    c.rect(530, field + 9, 1.5, 18, fill="accent")
    c.text(538, c.baseline(field, 36), "Password", fill="muted")
    c.circle(750, field + 18, 13, fill="accent")
    c.icon("go-next", 742, field + 10, "accent-ink")
    c.text(32, H - 28, "3 notifications", "r", 11, "muted")
    c.region("screen", 0, 0, W, H, [(8, 8, "bg"), (1272, 792, "bg"), (640, 700, "bg")])
    c.region("mark", 608, 86, 64, 112, [(616, 94, "fg"), (664, 94, "fg"), (616, 158, "fg"), (616, 142, "bg"),
                                         (656, 126, "bg")])
    c.region("field", 512, field, 256, 36, [(640, field + 5, "well"), (750, field + 8, "accent")])
    return c


def first_run(theme, faces):
    c = Canvas(theme, faces, "FerrionOS desktop after the first login",
               "What a new person sees first: the top bar, an empty workspace whose windows will be free to "
               "place, the dock, and one card that says so and offers columns instead, with its shortcut.")
    t = c.t
    wallpaper(c)
    bar(c, None, [("free", [], None)])
    dock(c, PINNED)
    w, h = 432, 132
    x, y = (W - w) // 2, 300
    c.panel(x, y, w, h, glass=False)
    c.mode(x + 24, y + 25, "free", "fg", "raise")
    c.text(x + 46, y + 36, "Windows open where you put them.", "s")
    c.text(x + 24, y + 60, "Columns keeps them side by side on a strip that scrolls.", fill="fg-2")
    label = "Try columns"
    bw = round(12 + 12 + 8 + c.measure(label, "s") + 14)
    bx, by = x + 24, y + 80
    c.rect(bx, by, bw, 32, r=8, fill="accent")
    c.mode(bx + 12, by + 10, "columns", "accent-ink", "accent")
    c.text(bx + 32, c.baseline(by, 32, "s"), label, "s", 13, "accent-ink")
    right = x + w - 24
    for key in ("Space", "Shift", "Super"):
        right = c.keycap(right, by + 6, key) - 4
    c.region("mode-card", x, y, w, h, [(x + 8, y + 8, "raise"), (x + w - 8, y + h - 8, "raise")])
    c.region("mode-button", bx, by, bw, 32, [(bx + 6, by + 16, "accent")])
    return c


def island_states(theme, faces):
    c = Canvas(theme, faces, "States of the island in the FerrionOS top bar",
               "The island in seven states: at rest it is an empty capsule; a change of a workspace's layout, "
               "a notification, a volume change, a task with progress, a recording and a waiting agent each "
               "reshape it.")
    t = c.t
    c.rect(0, 0, W, H, fill="bg")
    c.text(64, 64, "The island", "s", 17)
    c.text(64, 88, "One capsule in the middle of the top bar for what the system is doing now. It grows from its "
           "own middle and returns.", fill="fg-2")
    rows = ["At rest", "A workspace changed its layout, for 2 seconds", "A notification, for 4 seconds",
            "The volume changed, for 1.5 seconds", "A task that runs: quiet",
            "The screen is being recorded: says who, and a press stops it", "An agent waits for an answer: asks"]
    cx = 900
    for i, caption in enumerate(rows):
        y = 140 + i * 92
        c.rect(64, y - 22, 1152, 1, fill=t.line("bg"))
        c.text(64, y + 20, caption, fill="fg-2")
        if i == 0:
            island(c, cx, y)
        elif i == 1:
            left = island(c, cx, y - 2, (300, 36))
            c.mode(left + 16, y + 10, "columns", "fg", "raise")
            c.text(left + 38, c.baseline(y - 2, 36, "s"), "Workspace 1", "s")
            c.text(left + 300 - 16, c.baseline(y - 2, 36), "Columns", fill="fg-2", anchor="end")
        elif i == 2:
            left = island(c, cx, y - 18, (400, 68))
            c.circle(left + 34, y + 16, 18, fill="raise-2")
            c.icon("system-file-manager", left + 24, y + 6, "fg", 20)
            c.text(left + 64, y + 11, "Files", "s")
            c.text(left + 64, y + 31, "Copied 3 items to Documents", fill="fg-2")
        elif i == 3:
            left = island(c, cx, y - 4, (300, 40))
            c.icon("audio-volume-medium", left + 16, y + 8, "fg")
            c.rect(left + 44, y + 14, 200, 4, r=2, fill="raise-2")
            c.rect(left + 44, y + 14, 128, 4, r=2, fill="fg")
            c.text(left + 300 - 16, c.baseline(y - 4, 40, "m", 12), "64%", "m", 12, "fg-2", "end")
        elif i == 4:
            left = island(c, cx, y, (292, 32))
            c.circle(left + 20, y + 16, 7, stroke="raise-2", width=2)
            c.arc(left + 20, y + 16, 7, 0.62, "fg-2", 2)
            c.text(left + 38, c.baseline(y, 32), "Installing rust-analyzer", fill="fg-2")
            c.text(left + 292 - 16, c.baseline(y, 32, "m", 12), "62%", "m", 12, "muted", "end")
        elif i == 5:
            left = island(c, cx, y, (236, 32))
            c.circle(left + 20, y + 16, 5, fill="red")
            c.text(left + 36, c.baseline(y, 32, "s"), "Screen recorder", "s")
            c.text(left + 236 - 16, c.baseline(y, 32, "m", 12), "0:42", "m", 12, "fg-2", "end")
        else:
            left = island(c, cx, y, (236, 32))
            c.icon("dialog-question", left + 12, y + 8, "accent")
            c.text(left + 36, c.baseline(y, 32, "s"), "Agent in ferrion-hello", "s")
            c.circle(left + 236 - 20, y + 16, 4, fill="accent")
    return c


SCENES = {"desktop-traditional": desktop_traditional, "desktop-tiling": desktop_tiling, "overview": overview,
          "launcher": launcher, "lock": lock, "first-run": first_run, "island": island_states}


def tokens_table():
    lines = []
    dark, light = Theme("dark"), Theme("light")
    lines.append("|Role|Dark|Light|Job|")
    lines.append("|---|---|---|---|")
    for role, job in JOBS.items():
        lines.append(f"|`{role}`|`{dark[role]}`|`{light[role]}`|{job}|")
    lines.append(f"|`line` on `bg`, on `raise`|`{dark.line('bg')}`, `{dark.line('raise')}`|"
                 f"`{light.line('bg')}`, `{light.line('raise')}`|a hairline: a divider, the edge of a panel|")
    lines.append(f"|`line-strong` on `bg`|`{dark.line('bg', True)}`|`{light.line('bg', True)}`|"
                 f"the border of a window without focus|")
    lines.append(f"|accent, soft, on `raise`|`{dark.soft()}`|`{light.soft()}`|the selected row of a list|")
    lines.append(f"|`red`, `yellow`, `green`|{', '.join(f'`{dark[r]}`' for r in ('red', 'yellow', 'green'))}|"
                 f"{', '.join(f'`{light[r]}`' for r in ('red', 'yellow', 'green'))}|"
                 f"close, minimize and zoom on the key window|")
    lines.append("")
    lines.append("|Pair|Dark|Light|Floor|")
    lines.append("|---|---|---|---|")
    pairs = [("fg", "bg", 4.5), ("fg", "raise", 4.5), ("fg", "well", 4.5), ("fg-2", "bg", 4.5), ("fg-2", "raise", 4.5),
             ("muted", "bg", 4.5), ("muted", "raise", 4.5), ("muted", "well", 4.5), ("ring", "bg", 3),
             ("ring", "well", 3), ("accent", "bg", 3), ("accent", "raise", 3), ("accent-ink", "accent", 4.5),
             ("red", "title", 3), ("yellow", "title", 3), ("green", "title", 3)]
    for a, b, floor in pairs:
        cells = [f"{contrast(theme.c[a], theme.c[b]):.1f}:1" for theme in (dark, light)]
        lines.append(f"|`{a}` on `{b}`|{cells[0]}|{cells[1]}|{floor}:1|")
    lines.append("")
    lines.append("|Accent|Value|On dark `bg`|In the light theme|On light `bg`|")
    lines.append("|---|---|---|---|---|")
    for name, value in ACCENTS.items():
        shifted = readable(value, light.bg)
        lines.append(f"|{name}|`{hexa(value)}`|{contrast(value, dark.bg):.1f}:1|`{hexa(shifted)}`|"
                     f"{contrast(shifted, light.bg):.1f}:1|")
    return "\n".join(lines)


def check():
    """Renders every flat mockup and reads each probe pixel back; returns the number that differ."""
    import re
    import shutil
    import subprocess

    absent = [program for program in ("rsvg-convert", "magick") if not shutil.which(program)]
    if absent:
        sys.exit(f"mockups: --check needs {', '.join(absent)} on PATH")
    differing = 0
    for path in sorted((HERE / "flat").glob("*.svg")):
        png = subprocess.run(["rsvg-convert", path], capture_output=True, check=True).stdout
        ppm = subprocess.run(["magick", "png:-", "-alpha", "off", "ppm:-"], input=png, capture_output=True,
                             check=True).stdout
        header = re.match(rb"P6\s+(\d+)\s+(\d+)\s+255\s", ppm)
        width, pixels = int(header[1]), ppm[header.end():]
        regions = [r for g in ET.parse(path).getroot().iter(SVG_NS + "g") if g.get("id") == "regions" for r in g]
        probes = 0
        for region in regions:
            for probe in region.get("data-probes").split():
                place, expected = probe.split(":")
                x, y = (int(v) for v in place.split(","))
                at = (y * width + x) * 3
                seen = "#" + pixels[at:at + 3].hex()
                probes += 1
                if seen != expected:
                    differing += 1
                    print(f"flat/{path.name}: {region.get('id')} at ({x}, {y}) is {seen}, expected {expected}")
        print(f"flat/{path.name}: {len(regions)} regions, {probes} probes")
    return differing


def main():
    if "--tokens" in sys.argv:
        print(tokens_table())
        return
    if "--check" in sys.argv:
        sys.exit(1 if check() else 0)
    try:
        import fontTools  # noqa: F401
    except ImportError:
        sys.exit("mockups: fontTools is missing; install python-fonttools")
    files = {"r": "Sans/Fira/Regular.ttf", "s": "Sans/Fira/SemiBold.ttf", "m": "Mono/Fira/Regular.ttf"}
    missing = [str((FONTS / f).relative_to(ROOT)) for f in files.values() if not (FONTS / f).exists()]
    if missing:
        sys.exit(f"mockups: missing {', '.join(missing)}; run `make f.orbdata`")
    faces = {tag: Face(tag, FONTS / f) for tag, f in files.items()}
    written = []
    for tier in ("rich", "flat"):
        for name, scene in SCENES.items():
            scene(Theme("dark", tier), faces).write(f"{name}.svg")
        desktop_traditional(Theme("light", tier), faces).write("desktop-traditional-light.svg")
    written = sorted(path.name for path in HERE.glob("*.svg"))
    print(f"wrote {len(written)} screens, each also under flat/: " + ", ".join(written))


if __name__ == "__main__":
    main()
