"""Read the geometry and RGB probes shared by the desktop VM checks."""

from pathlib import Path
import xml.etree.ElementTree as ET

FIXTURES = Path(__file__).resolve().parent / "desktop-fixtures" / "flat"


def load_regions(name):
    svg = ET.parse(FIXTURES / name).getroot()
    ns = "{http://www.w3.org/2000/svg}"
    regions = {}
    for group in svg.iter(ns + "g"):
        if group.get("id") != "regions":
            continue
        for rect in group.iter(ns + "rect"):
            points = []
            for probe in rect.attrib["data-probes"].split():
                place, color = probe.split(":")
                x, y = map(int, place.split(","))
                expected = bytes.fromhex(color.removeprefix("#"))
                if len(expected) != 3:
                    raise ValueError(f"invalid RGB probe in {rect.attrib['id']}")
                points.append((x, y, expected))
            regions[rect.attrib["id"]] = {
                "rect": tuple(float(rect.attrib[key]) for key in ("x", "y", "width", "height")),
                "probes": points,
            }
    if not regions:
        raise ValueError(f"no regions in {name}")
    return regions


def compare_probes(frame, points):
    differences = []
    for x, y, expected in points:
        if not (0 <= x < frame.width and 0 <= y < frame.height):
            differences.append(f"({x},{y}) outside {frame.width}x{frame.height}")
            continue
        at = (y * frame.width + x) * 3
        seen = frame.rgb[at:at + 3]
        if any(abs(a - b) > 2 for a, b in zip(seen, expected)):
            differences.append(f"({x},{y}) #{seen.hex()}, expected #{expected.hex()}")
    return differences
