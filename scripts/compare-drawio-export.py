"""Measure how closely a converted diagram matches draw.io's own SVG export.

Usage: python3 scripts/compare-drawio-export.py --docsvg PATH EXPORT.svg...

Each input must be an SVG draw.io exported with "Include a copy of my diagram",
so the diagram it was made from can be read back out of the file and converted
here. Both renderings are compared without their text: draw.io draws labels in
`foreignObject`, which no standalone renderer supports, so a like-for-like
comparison is of the geometry. Both are rendered at one image pixel per model
unit and aligned on the content each of them drew, so the measurement does not
depend on where either puts the page edge.

Requires `rsvg-convert`, Pillow and NumPy. No diagram is downloaded; the files
the caller names are the only input.
"""
import argparse
import json
import html
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.parse

import numpy as np
from PIL import Image

TEXT = [
    re.compile(r"<switch>.*?</switch>", re.S),
    re.compile(r"<foreignObject.*?</foreignObject>", re.S),
    re.compile(r"<text\b.*?</text>", re.S),
    re.compile(r"<text\b[^>]*/>"),
]


def without_text(source, target):
    text = source.read_text(errors="replace")
    for pattern in TEXT:
        text = pattern.sub("", text)
    target.write_text(text, encoding="utf-8")
    return target


def render(svg, png):
    subprocess.run(
        ["rsvg-convert", "-b", "white", "-o", str(png), str(svg)],
        check=True,
        capture_output=True,
    )
    return np.asarray(Image.open(png).convert("RGB")).astype(np.int16)


def content_box(image):
    rows, columns = np.where(image.mean(axis=2) < 250)
    return None if rows.size == 0 else (rows.min(), rows.max(), columns.min(), columns.max())


def embedded_diagram(export):
    text = export.read_text(errors="replace")
    match = re.search(r'\scontent="([^"]*)"', text)
    if not match:
        return None
    source = html.unescape(match.group(1))
    return source if source.lstrip().startswith("<") else urllib.parse.unquote(source)


def compare(export, docsvg, stencils, work):
    source = embedded_diagram(export)
    if source is None:
        return export.name, "no embedded diagram", ""
    room = work / export.stem
    room.mkdir(parents=True, exist_ok=True)
    diagram = room / "source.drawio"
    diagram.write_text(source, encoding="utf-8")
    arguments = [str(docsvg), str(diagram), "--output", str(room / "ours")]
    for path in stencils:
        arguments += ["--stencils", str(path)]
    result = subprocess.run(arguments, capture_output=True, text=True)
    if result.returncode:
        return export.name, "conversion failed", result.stderr.strip().splitlines()[-1][:70]
    pages = sorted((room / "ours").glob("page-*.svg"))
    if not pages:
        return export.name, "no pages", ""
    theirs = render(without_text(export, room / "theirs.svg"), room / "theirs.png")
    ours = render(without_text(pages[0], room / "ours.svg"), room / "ours.png")
    a, b = content_box(theirs), content_box(ours)
    if a is None and b is None:
        # A diagram whose only content is a label leaves both sides blank once
        # the text is stripped, so there is no geometry to compare.
        return export.name, "no geometry either side", ""
    if b is None:
        # draw.io stands a "click here to edit" placeholder on a diagram that
        # has nothing in it. That placeholder is not in the model, so drawing
        # nothing is right and the difference is not a defect.
        report = json.loads((room / "ours" / "conversion.json").read_text())
        if any("no cells to draw" in warning for warning in report["warnings"]):
            return export.name, "empty diagram", "draw.io drew its own placeholder"
        return export.name, "WE DREW NOTHING", "draw.io drew geometry here"
    if a is None:
        return export.name, "they drew nothing", "we drew geometry they did not"
    height = min(a[1] - a[0], b[1] - b[0]) + 1
    width = min(a[3] - a[2], b[3] - b[2]) + 1
    theirs = theirs[a[0]:a[0] + height, a[2]:a[2] + width]
    ours = ours[b[0]:b[0] + height, b[2]:b[2] + width]
    difference = np.abs(theirs - ours).max(axis=2)
    drift = (abs((a[1] - a[0]) - (b[1] - b[0])), abs((a[3] - a[2]) - (b[3] - b[2])))
    return (
        export.name,
        f"{width}x{height}",
        f"mean={difference.mean():5.2f}  >32={(difference > 32).mean() * 100:5.2f}%"
        f"  content drift={drift[1]}x{drift[0]}px",
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docsvg", type=pathlib.Path, default=pathlib.Path("target/release/docsvg"))
    parser.add_argument("--stencils", type=pathlib.Path, action="append", default=[])
    parser.add_argument("exports", type=pathlib.Path, nargs="+")
    arguments = parser.parse_args()
    if shutil.which("rsvg-convert") is None:
        parser.error("rsvg-convert is required")
    with tempfile.TemporaryDirectory(prefix="docsvg-fidelity-") as temporary:
        work = pathlib.Path(temporary)
        for export in arguments.exports:
            name, size, detail = compare(export, arguments.docsvg, arguments.stencils, work)
            print(f"{name[:44]:46s} {size:14s} {detail}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
