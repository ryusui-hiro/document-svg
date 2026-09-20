#!/usr/bin/env python3
"""Write the sample documents in `samples/` and convert them with docsvg.

Every produced document is synthetic and authored in this repository (some
are reused from test fixtures), so the samples carry the repository's own
license instead of a third party's. Run it after changing a renderer to
refresh what `samples/svg/` shows.

    python3 scripts/make_samples.py

Python 3.10+, standard library only.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import shutil
import struct
import subprocess
import sys
import urllib.parse
import zlib
from pathlib import Path

REPOSITORY = Path(__file__).resolve().parent.parent
SAMPLES = REPOSITORY / "samples"
SOURCE = SAMPLES / "source"
RENDERED = SAMPLES / "svg"

EMU_PER_POINT = 12700


# --------------------------------------------------------------------------
# PNG
# --------------------------------------------------------------------------


def png_bytes(width: int, height: int, pixel: "callable[[int, int], tuple[int, int, int]]") -> bytes:
    """Encode an RGB image without depending on an imaging library."""

    raw = bytearray()
    for y in range(height):
        raw.append(0)  # no per-scanline filter
        for x in range(width):
            raw.extend(pixel(x, y))

    def chunk(kind: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + kind
            + payload
            + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
        )

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
        + chunk(b"IEND", b"")
    )


def gradient_badge(width: int = 240, height: int = 160) -> bytes:
    """A small image with a visible gradient and a darker border."""

    def pixel(x: int, y: int) -> tuple[int, int, int]:
        if x < 4 or y < 4 or x >= width - 4 or y >= height - 4:
            return (48, 79, 254)
        across = x / max(width - 1, 1)
        down = y / max(height - 1, 1)
        return (
            int(48 + 180 * across),
            int(79 + 120 * down),
            int(254 - 60 * across),
        )

    return png_bytes(width, height, pixel)


# --------------------------------------------------------------------------
# PDF
# --------------------------------------------------------------------------


def build_pdf(path: Path) -> None:
    """Write a two-page PDF with text, vector art and an embedded image."""

    image = gradient_badge()
    image_stream = zlib.compress(_png_rgb_samples(image), 9)

    def text(x: float, y: float, size: float, font: str, value: str) -> str:
        escaped = value.replace("\\", r"\\").replace("(", r"\(").replace(")", r"\)")
        return f"BT /{font} {size} Tf {x} {y} Td ({escaped}) Tj ET\n"

    page_one = (
        "0.19 0.31 1.00 rg 0 742 612 50 re f\n"
        "1 1 1 rg " + text(48, 758, 22, "F2", "document-svg sample")
        + "0 0 0 rg "
        + text(48, 700, 14, "F2", "A PDF that exercises the converter")
        + text(48, 672, 11, "F1", "Text, vector paths and an embedded raster image on one page.")
        + text(48, 654, 11, "F1", "Page 2 shows a table drawn with stroked paths.")
        # vector art: filled circle-ish shape via bezier, plus a stroked path
        + "0.13 0.59 0.95 rg\n"
        "120 480 m 120 535 165 580 220 580 c 275 580 320 535 320 480 c "
        "320 425 275 380 220 380 c 165 380 120 425 120 480 c f\n"
        "0.98 0.55 0.09 RG 4 w 1 J 1 j\n"
        "360 400 m 420 560 l 480 420 l 540 540 l S\n"
        + "0 0 0 rg "
        + text(48, 330, 10, "F1", "Above: a filled bezier path and a stroked polyline.")
        + text(48, 300, 10, "F1", "Below: a 240x160 RGB image embedded as a Flate stream.")
        + "q 240 0 0 160 48 120 cm /Im1 Do Q\n"
    )

    rows = [
        ("Format", "Input", "Output"),
        ("PDF", "page tree", "page-NNNN.svg"),
        ("PPTX", "slide", "page-NNNN.svg"),
        ("XLSX", "print page", "page-NNNN.svg"),
        ("DOCX", "laid-out page", "page-NNNN.svg"),
    ]
    table = ["0 0 0 rg " + text(48, 720, 18, "F2", "Page 2 - a stroked table")]
    top = 660.0
    row_height = 28.0
    column_x = [48.0, 220.0, 380.0, 560.0]
    for index, row in enumerate(rows):
        y = top - index * row_height
        font = "F2" if index == 0 else "F1"
        for column, value in enumerate(row):
            table.append(text(column_x[column] + 8, y - 19, 11, font, value))
    table.append("0.4 0.4 0.4 RG 1 w\n")
    for index in range(len(rows) + 1):
        y = top - index * row_height
        table.append(f"{column_x[0]} {y} m {column_x[-1]} {y} l S\n")
    for x in column_x:
        table.append(f"{x} {top} m {x} {top - len(rows) * row_height} l S\n")
    page_two = "".join(table)

    objects: dict[int, bytes] = {}
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R >>"
    objects[2] = b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>"
    objects[3] = (
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Resources << /Font << /F1 7 0 R /F2 8 0 R >> /XObject << /Im1 9 0 R >> >> "
        b"/Contents 5 0 R >>"
    )
    objects[4] = (
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
        b"/Resources << /Font << /F1 7 0 R /F2 8 0 R >> >> /Contents 6 0 R >>"
    )
    objects[5] = _stream(page_one.encode("latin-1"))
    objects[6] = _stream(page_two.encode("latin-1"))
    objects[7] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
    objects[8] = (
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
    )
    objects[9] = (
        b"<< /Type /XObject /Subtype /Image /Width 240 /Height 160 /ColorSpace /DeviceRGB "
        b"/BitsPerComponent 8 /Filter /FlateDecode /Length "
        + str(len(image_stream)).encode()
        + b" >>\nstream\n"
        + image_stream
        + b"\nendstream"
    )

    out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets: dict[int, int] = {}
    for number in sorted(objects):
        offsets[number] = len(out)
        out += f"{number} 0 obj\n".encode()
        out += objects[number]
        out += b"\nendobj\n"
    xref_start = len(out)
    highest = max(objects)
    out += f"xref\n0 {highest + 1}\n".encode()
    out += b"0000000000 65535 f \n"
    for number in range(1, highest + 1):
        out += f"{offsets[number]:010} 00000 n \n".encode()
    out += (
        f"trailer\n<< /Size {highest + 1} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n".encode()
    )
    path.write_bytes(bytes(out))


def _stream(payload: bytes) -> bytes:
    compressed = zlib.compress(payload, 9)
    return (
        b"<< /Length "
        + str(len(compressed)).encode()
        + b" /Filter /FlateDecode >>\nstream\n"
        + compressed
        + b"\nendstream"
    )


def _png_rgb_samples(png: bytes) -> bytes:
    """Recover raw RGB samples from the PNG this module wrote."""

    position = 8
    idat = bytearray()
    width = height = 0
    while position < len(png):
        (length,) = struct.unpack(">I", png[position : position + 4])
        kind = png[position + 4 : position + 8]
        payload = png[position + 8 : position + 8 + length]
        if kind == b"IHDR":
            width, height = struct.unpack(">II", payload[:8])
        elif kind == b"IDAT":
            idat.extend(payload)
        position += 12 + length
    raw = zlib.decompress(bytes(idat))
    stride = width * 3
    samples = bytearray()
    for y in range(height):
        start = y * (stride + 1) + 1  # skip the filter byte
        samples.extend(raw[start : start + stride])
    return bytes(samples)


# --------------------------------------------------------------------------
# Open XML packaging
# --------------------------------------------------------------------------


def write_package(path: Path, entries: dict[str, bytes]) -> None:
    import zipfile

    path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, payload in entries.items():
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o600 << 16
            archive.writestr(info, payload)


def relationships(*items: tuple[str, str, str]) -> bytes:
    body = "".join(
        f'<Relationship Id="{identifier}" Type="{kind}" Target="{target}"/>'
        for identifier, kind, target in items
    )
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f"{body}</Relationships>"
    ).encode()


def content_types(defaults: dict[str, str], overrides: dict[str, str]) -> bytes:
    body = "".join(
        f'<Default Extension="{extension}" ContentType="{kind}"/>'
        for extension, kind in defaults.items()
    )
    body += "".join(
        f'<Override PartName="{part}" ContentType="{kind}"/>' for part, kind in overrides.items()
    )
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        f"{body}</Types>"
    ).encode()


R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
PKG = "http://schemas.openxmlformats.org/package/2006/relationships"


# --------------------------------------------------------------------------
# DOCX
# --------------------------------------------------------------------------


def build_docx(path: Path) -> None:
    def paragraph(text: str, style: str | None = None, bold: bool = False) -> str:
        properties = f'<w:pStyle w:val="{style}"/>' if style else ""
        run_properties = "<w:rPr><w:b/></w:rPr>" if bold else ""
        return (
            f"<w:p><w:pPr>{properties}</w:pPr>"
            f"<w:r>{run_properties}<w:t xml:space='preserve'>{text}</w:t></w:r></w:p>"
        )

    def cell(text: str, bold: bool = False, width: int = 2600) -> str:
        run_properties = "<w:rPr><w:b/></w:rPr>" if bold else ""
        return (
            f'<w:tc><w:tcPr><w:tcW w:w="{width}" w:type="dxa"/></w:tcPr>'
            f"<w:p><w:r>{run_properties}<w:t xml:space='preserve'>{text}</w:t></w:r></w:p></w:tc>"
        )

    table_rows = [("Format", "Entry point", "Output"), ("PDF", "convert_path", "page-NNNN.svg")]
    table_rows += [
        ("PPTX", "convert_path", "one page per slide"),
        ("XLSX", "convert_path", "one page per print page"),
        ("DOCX", "convert_path", "one page per laid-out page"),
    ]
    rows = "".join(
        "<w:tr>" + "".join(cell(value, bold=index == 0) for value in row) + "</w:tr>"
        for index, row in enumerate(table_rows)
    )
    table = (
        "<w:tbl><w:tblPr>"
        '<w:tblBorders><w:top w:val="single" w:sz="6" w:color="9E9E9E"/>'
        '<w:left w:val="single" w:sz="6" w:color="9E9E9E"/>'
        '<w:bottom w:val="single" w:sz="6" w:color="9E9E9E"/>'
        '<w:right w:val="single" w:sz="6" w:color="9E9E9E"/>'
        '<w:insideH w:val="single" w:sz="6" w:color="9E9E9E"/>'
        '<w:insideV w:val="single" w:sz="6" w:color="9E9E9E"/></w:tblBorders>'
        "</w:tblPr>" + rows + "</w:tbl>"
    )

    body = (
        paragraph("document-svg sample", style="Title")
        + paragraph(
            "This document is written by scripts/make_samples.py so that the "
            "repository can ship a sample without borrowing someone else's file."
        )
        + paragraph("What the converter reads here", style="Heading1")
        + paragraph(
            "Paper size and margins, style inheritance, word-aware paragraph "
            "wrapping, tables with borders, and the header and footer below."
        )
        + paragraph("Supported inputs", style="Heading1")
        + table
        + paragraph("")
        + paragraph("A page break follows, so the sample spans two pages.")
        + '<w:p><w:r><w:br w:type="page"/></w:r></w:p>'
        + paragraph("Second page", style="Heading1")
        + paragraph(
            "Each page of a DOCX becomes one SVG file. The footer carries a PAGE "
            "field, which the converter resolves to the page it is drawn on."
        )
        + '<w:sectPr><w:headerReference w:type="default" r:id="rId4"/>'
        '<w:footerReference w:type="default" r:id="rId5"/>'
        '<w:pgSz w:w="11906" w:h="16838"/>'
        '<w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" '
        'w:header="709" w:footer="709"/></w:sectPr>'
    )

    namespaces = (
        'xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"'
    )
    document = f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {namespaces}><w:body>{body}</w:body></w:document>'

    styles = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f"<w:styles {namespaces}>"
        '<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri"/>'
        '<w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>'
        '<w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/>'
        '<w:rPr><w:b/><w:sz w:val="56"/><w:color w:val="304FFE"/></w:rPr></w:style>'
        '<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/>'
        '<w:rPr><w:b/><w:sz w:val="32"/><w:color w:val="1A237E"/></w:rPr></w:style>'
        "</w:styles>"
    )
    header = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f"<w:hdr {namespaces}><w:p><w:r><w:t>document-svg / sample document</w:t></w:r></w:p></w:hdr>"
    )
    footer = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f"<w:ftr {namespaces}><w:p><w:r><w:t xml:space='preserve'>Page </w:t></w:r>"
        '<w:r><w:fldChar w:fldCharType="begin"/></w:r>'
        "<w:r><w:instrText xml:space='preserve'> PAGE </w:instrText></w:r>"
        '<w:r><w:fldChar w:fldCharType="separate"/></w:r>'
        "<w:r><w:t>1</w:t></w:r>"
        '<w:r><w:fldChar w:fldCharType="end"/></w:r></w:p></w:ftr>'
    )

    write_package(
        path,
        {
            "[Content_Types].xml": content_types(
                {"rels": "application/vnd.openxmlformats-package.relationships+xml"},
                {
                    "/word/document.xml": "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
                    "/word/styles.xml": "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
                    "/word/header1.xml": "application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml",
                    "/word/footer1.xml": "application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml",
                },
            ),
            "_rels/.rels": relationships(
                ("rId1", f"{R}/officeDocument", "word/document.xml")
            ),
            "word/document.xml": document.encode(),
            "word/_rels/document.xml.rels": relationships(
                ("rId2", f"{R}/styles", "styles.xml"),
                ("rId4", f"{R}/header", "header1.xml"),
                ("rId5", f"{R}/footer", "footer1.xml"),
            ),
            "word/styles.xml": styles.encode(),
            "word/header1.xml": header.encode(),
            "word/footer1.xml": footer.encode(),
        },
    )


# --------------------------------------------------------------------------
# PPTX
# --------------------------------------------------------------------------


def build_pptx(path: Path) -> None:
    a = "http://schemas.openxmlformats.org/drawingml/2006/main"
    p = "http://schemas.openxmlformats.org/presentationml/2006/main"

    def shape(
        identifier: int,
        name: str,
        x: int,
        y: int,
        width: int,
        height: int,
        fill: str | None,
        body: str,
    ) -> str:
        fill_xml = f'<a:solidFill><a:srgbClr val="{fill}"/></a:solidFill>' if fill else "<a:noFill/>"
        return (
            f'<p:sp><p:nvSpPr><p:cNvPr id="{identifier}" name="{name}"/>'
            "<p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr>"
            f'<a:xfrm><a:off x="{x}" y="{y}"/><a:ext cx="{width}" cy="{height}"/></a:xfrm>'
            '<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>'
            f"{fill_xml}<a:ln><a:noFill/></a:ln></p:spPr>"
            f"<p:txBody><a:bodyPr anchor='ctr'/><a:lstStyle/>{body}</p:txBody></p:sp>"
        )

    def para(text: str, size: int, color: str, bold: bool = False, align: str = "l") -> str:
        return (
            f'<a:p><a:pPr algn="{align}"><a:buNone/><a:buClr><a:srgbClr val="000000"/></a:buClr></a:pPr>'
            f'<a:r><a:rPr lang="en" sz="{size}" b="{1 if bold else 0}">'
            f'<a:solidFill><a:srgbClr val="{color}"/></a:solidFill></a:rPr>'
            f"<a:t>{text}</a:t></a:r>"
            '<a:endParaRPr><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:endParaRPr></a:p>'
        )

    def slide(shapes: str) -> bytes:
        return (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
            f'<p:sld xmlns:a="{a}" xmlns:p="{p}" xmlns:r="{R}"><p:cSld><p:spTree>'
            "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>"
            "<p:grpSpPr/>"
            f"{shapes}</p:spTree></p:cSld></p:sld>"
        ).encode()

    slide_width, slide_height = 9144000, 5143500  # 16:9

    slide1 = slide(
        shape(2, "Banner", 0, 0, slide_width, slide_height, "304FFE", "")
        + shape(
            3,
            "Title",
            600000,
            1500000,
            7900000,
            1400000,
            None,
            para("document-svg", 5400, "FFFFFF", bold=True)
            + para("PDF and Office documents as self-contained SVG pages", 1800, "E8EAF6"),
        )
    )

    def bullet(text: str) -> str:
        return para("•  " + text, 1600, "263238")

    slide2 = slide(
        shape(2, "Header", 0, 0, slide_width, 900000, "304FFE", "")
        + shape(
            3,
            "Header text",
            500000,
            0,
            8000000,
            900000,
            None,
            para("One page in, one SVG out", 2800, "FFFFFF", bold=True),
        )
        + shape(
            4,
            "Body",
            500000,
            1200000,
            5000000,
            3200000,
            None,
            bullet("Every page becomes page-NNNN.svg")
            + bullet("Fonts, shapes and images stay in the file")
            + bullet("A conversion.json records warnings and timing")
            + bullet("Nothing is fetched at render time"),
        )
        + shape(5, "Accent A", 5900000, 1400000, 1000000, 1000000, "00BFA5", "")
        + shape(6, "Accent B", 7100000, 1400000, 1000000, 1000000, "FF6D00", "")
        + shape(7, "Accent C", 5900000, 2600000, 1000000, 1000000, "AA00FF", "")
        + shape(8, "Accent D", 7100000, 2600000, 1000000, 1000000, "FFD600", ""),
    )

    master = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<p:sldMaster xmlns:a="{a}" xmlns:p="{p}" xmlns:r="{R}"><p:cSld><p:spTree>'
        '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        "</p:spTree></p:cSld><p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" "
        'accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" '
        'accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/>'
        '<p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst>'
        "</p:sldMaster>"
    ).encode()

    layout = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<p:sldLayout xmlns:a="{a}" xmlns:p="{p}" xmlns:r="{R}" type="blank"><p:cSld><p:spTree>'
        '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>'
        "</p:spTree></p:cSld></p:sldLayout>"
    ).encode()

    def theme_color(tag: str, value: str) -> str:
        return f'<a:{tag}><a:srgbClr val="{value}"/></a:{tag}>'

    theme = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<a:theme xmlns:a="{a}" name="sample"><a:themeElements><a:clrScheme name="sample">'
        + theme_color("dk1", "000000")
        + theme_color("lt1", "FFFFFF")
        + theme_color("dk2", "1A237E")
        + theme_color("lt2", "E8EAF6")
        + theme_color("accent1", "304FFE")
        + theme_color("accent2", "00BFA5")
        + theme_color("accent3", "FF6D00")
        + theme_color("accent4", "AA00FF")
        + theme_color("accent5", "FFD600")
        + theme_color("accent6", "455A64")
        + theme_color("hlink", "304FFE")
        + theme_color("folHlink", "AA00FF")
        + "</a:clrScheme>"
        '<a:fontScheme name="sample"><a:majorFont><a:latin typeface="Calibri"/>'
        "<a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont>"
        '<a:minorFont><a:latin typeface="Calibri"/><a:ea typeface=""/><a:cs typeface=""/>'
        "</a:minorFont></a:fontScheme>"
        "<a:fmtScheme name=\"sample\"><a:fillStyleLst>"
        '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
        '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
        '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst>'
        '<a:lnStyleLst><a:ln><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>'
        '<a:ln><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln>'
        '<a:ln><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst>'
        "<a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle>"
        "<a:effectStyle><a:effectLst/></a:effectStyle>"
        "<a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>"
        '<a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
        '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill>'
        '<a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst>'
        "</a:fmtScheme></a:themeElements></a:theme>"
    ).encode()

    presentation = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<p:presentation xmlns:a="{a}" xmlns:p="{p}" xmlns:r="{R}">'
        '<p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>'
        '<p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst>'
        f'<p:sldSz cx="{slide_width}" cy="{slide_height}"/>'
        f'<p:notesSz cx="{slide_height}" cy="{slide_width}"/>'
        "</p:presentation>"
    ).encode()

    presentation_kinds = "application/vnd.openxmlformats-officedocument.presentationml"
    write_package(
        path,
        {
            "[Content_Types].xml": content_types(
                {"rels": "application/vnd.openxmlformats-package.relationships+xml"},
                {
                    "/ppt/presentation.xml": f"{presentation_kinds}.presentation.main+xml",
                    "/ppt/slides/slide1.xml": f"{presentation_kinds}.slide+xml",
                    "/ppt/slides/slide2.xml": f"{presentation_kinds}.slide+xml",
                    "/ppt/slideMasters/slideMaster1.xml": f"{presentation_kinds}.slideMaster+xml",
                    "/ppt/slideLayouts/slideLayout1.xml": f"{presentation_kinds}.slideLayout+xml",
                    "/ppt/theme/theme1.xml": "application/vnd.openxmlformats-officedocument.theme+xml",
                },
            ),
            "_rels/.rels": relationships(("rId1", f"{R}/officeDocument", "ppt/presentation.xml")),
            "ppt/presentation.xml": presentation,
            "ppt/_rels/presentation.xml.rels": relationships(
                ("rId1", f"{R}/slideMaster", "slideMasters/slideMaster1.xml"),
                ("rId2", f"{R}/slide", "slides/slide1.xml"),
                ("rId3", f"{R}/slide", "slides/slide2.xml"),
            ),
            "ppt/slides/slide1.xml": slide1,
            "ppt/slides/slide2.xml": slide2,
            "ppt/slides/_rels/slide1.xml.rels": relationships(
                ("rId1", f"{R}/slideLayout", "../slideLayouts/slideLayout1.xml")
            ),
            "ppt/slides/_rels/slide2.xml.rels": relationships(
                ("rId1", f"{R}/slideLayout", "../slideLayouts/slideLayout1.xml")
            ),
            "ppt/slideMasters/slideMaster1.xml": master,
            "ppt/slideMasters/_rels/slideMaster1.xml.rels": relationships(
                ("rId1", f"{R}/slideLayout", "../slideLayouts/slideLayout1.xml"),
                ("rId2", f"{R}/theme", "../theme/theme1.xml"),
            ),
            "ppt/slideLayouts/slideLayout1.xml": layout,
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels": relationships(
                ("rId1", f"{R}/slideMaster", "../slideMasters/slideMaster1.xml")
            ),
            "ppt/theme/theme1.xml": theme,
        },
    )


# --------------------------------------------------------------------------
# XLSX
# --------------------------------------------------------------------------


def build_xlsx(path: Path) -> None:
    regions = ["North", "South", "East", "West"]
    products = ["Carretera", "Montana", "Paseo", "Velo", "VTT", "Amarilla"]

    strings: list[str] = []
    index_of: dict[str, int] = {}

    def shared(value: str) -> int:
        if value not in index_of:
            index_of[value] = len(strings)
            strings.append(value)
        return index_of[value]

    header = ["Region", "Product", "Month", "Units", "Unit price", "Revenue"]
    rows: list[str] = []
    merged = '<mergeCells count="1"><mergeCell ref="A1:F1"/></mergeCells>'
    title_cell = f'<c r="A1" s="1" t="s"><v>{shared("Quarterly sales by region")}</v></c>'
    rows.append(f'<row r="1" ht="26" customHeight="1">{title_cell}</row>')
    header_cells = "".join(
        f'<c r="{chr(ord("A") + column)}2" s="2" t="s"><v>{shared(name)}</v></c>'
        for column, name in enumerate(header)
    )
    rows.append(f'<row r="2">{header_cells}</row>')

    # 180 data rows: more than fits on one sheet of paper, so the sample shows
    # how a worksheet is split across pages.
    for offset in range(180):
        row = offset + 3
        region = regions[offset % len(regions)]
        product = products[(offset // 4) % len(products)]
        month = f"2026-{(offset % 12) + 1:02}-01"
        units = 120 + (offset * 37) % 900
        price = 12.5 + (offset % 17) * 3.25
        cells = (
            f'<c r="A{row}" t="s"><v>{shared(region)}</v></c>'
            f'<c r="B{row}" t="s"><v>{shared(product)}</v></c>'
            f'<c r="C{row}" t="s"><v>{shared(month)}</v></c>'
            f'<c r="D{row}"><v>{units}</v></c>'
            f'<c r="E{row}" s="3"><v>{price:.2f}</v></c>'
            f'<c r="F{row}" s="3"><f>D{row}*E{row}</f><v>{units * price:.2f}</v></c>'
        )
        rows.append(f'<row r="{row}">{cells}</row>')

    sheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f'<dimension ref="A1:F{len(rows) + 1}"/>'
        "<cols>"
        '<col min="1" max="1" width="14" customWidth="1"/>'
        '<col min="2" max="2" width="14" customWidth="1"/>'
        '<col min="3" max="3" width="12" customWidth="1"/>'
        '<col min="4" max="4" width="10" customWidth="1"/>'
        '<col min="5" max="6" width="14" customWidth="1"/>'
        "</cols>"
        f"<sheetData>{''.join(rows)}</sheetData>"
        f"{merged}"
        '<pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/>'
        '<pageSetup orientation="portrait" paperSize="9"/>'
        "</worksheet>"
    ).encode()

    shared_strings = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        f'count="{len(strings)}" uniqueCount="{len(strings)}">'
        + "".join(f"<si><t>{value}</t></si>" for value in strings)
        + "</sst>"
    ).encode()

    styles = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        '<numFmts count="1"><numFmt numFmtId="164" formatCode="#,##0.00"/></numFmts>'
        '<fonts count="3"><font><sz val="11"/><name val="Calibri"/></font>'
        '<font><b/><sz val="18"/><color rgb="FF304FFE"/><name val="Calibri"/></font>'
        '<font><b/><sz val="11"/><color rgb="FFFFFFFF"/><name val="Calibri"/></font></fonts>'
        '<fills count="3"><fill><patternFill patternType="none"/></fill>'
        '<fill><patternFill patternType="gray125"/></fill>'
        '<fill><patternFill patternType="solid"><fgColor rgb="FF304FFE"/>'
        "<bgColor indexed=\"64\"/></patternFill></fill></fills>"
        '<borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>'
        '<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>'
        '<cellXfs count="4">'
        '<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>'
        '<xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/>'
        '<xf numFmtId="0" fontId="2" fillId="2" borderId="0" xfId="0" applyFont="1" applyFill="1"/>'
        '<xf numFmtId="164" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>'
        "</cellXfs></styleSheet>"
    ).encode()

    workbook = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        f'xmlns:r="{R}"><sheets><sheet name="Sales" sheetId="1" r:id="rId1"/></sheets></workbook>'
    ).encode()

    sheet_kinds = "application/vnd.openxmlformats-officedocument.spreadsheetml"
    write_package(
        path,
        {
            "[Content_Types].xml": content_types(
                {"rels": "application/vnd.openxmlformats-package.relationships+xml"},
                {
                    "/xl/workbook.xml": f"{sheet_kinds}.sheet.main+xml",
                    "/xl/worksheets/sheet1.xml": f"{sheet_kinds}.worksheet+xml",
                    "/xl/sharedStrings.xml": f"{sheet_kinds}.sharedStrings+xml",
                    "/xl/styles.xml": f"{sheet_kinds}.styles+xml",
                },
            ),
            "_rels/.rels": relationships(("rId1", f"{R}/officeDocument", "xl/workbook.xml")),
            "xl/workbook.xml": workbook,
            "xl/_rels/workbook.xml.rels": relationships(
                ("rId1", f"{R}/worksheet", "worksheets/sheet1.xml"),
                ("rId2", f"{R}/sharedStrings", "sharedStrings.xml"),
                ("rId3", f"{R}/styles", "styles.xml"),
            ),
            "xl/worksheets/sheet1.xml": sheet,
            "xl/sharedStrings.xml": shared_strings,
            "xl/styles.xml": styles,
        },
    )


# --------------------------------------------------------------------------
# drawio
# --------------------------------------------------------------------------


def build_drawio(path: Path) -> None:
    """Write a two-page draw.io file with its diagram bodies left readable.

    The editor compresses each `<diagram>` by default and docsvg reads that
    form too, but a sample is more useful as XML anyone can open and diff.
    """

    def cell(identity: str, label: str, style: str, x: int, y: int,
             width: int, height: int, parent: str = "1") -> str:
        return (
            f'        <mxCell id="{identity}" value="{label}" style="{style}" '
            f'vertex="1" parent="{parent}">\n'
            f'          <mxGeometry x="{x}" y="{y}" width="{width}" '
            f'height="{height}" as="geometry"/>\n'
            "        </mxCell>"
        )

    def edge(identity: str, label: str, style: str, source: str, target: str) -> str:
        return (
            f'        <mxCell id="{identity}" value="{label}" style="{style}" '
            f'edge="1" parent="1" source="{source}" target="{target}">\n'
            '          <mxGeometry relative="1" as="geometry"/>\n'
            "        </mxCell>"
        )

    rounded = "rounded=1;whiteSpace=wrap;html=1;"
    orthogonal = "edgeStyle=orthogonalEdgeStyle;rounded=1;html=1;"
    flow = "\n".join([
        cell("intake", "Upload document", "ellipse;whiteSpace=wrap;html=1;"
             "fillColor=#D5E8D4;strokeColor=#82B366;", 40, 40, 160, 60),
        cell("check", "Supported<br>format?", "rhombus;whiteSpace=wrap;html=1;"
             "fillColor=#FFF2CC;strokeColor=#D6B656;", 60, 150, 120, 90),
        cell("convert", "Convert to SVG pages", rounded + "shadow=1;", 260, 165, 180, 60),
        cell("reject", "Report unsupported input", rounded
             + "fillColor=#F8CECC;strokeColor=#B85450;", 20, 300, 200, 60),
        cell("store", "conversion.json", "shape=cylinder3;whiteSpace=wrap;html=1;"
             "size=12;fillColor=#DAE8FC;strokeColor=#6C8EBF;", 290, 300, 120, 70),
        edge("yes", "yes", orthogonal, "check", "convert"),
        edge("no", "no", orthogonal + "dashed=1;", "check", "reject"),
        edge("start", "", orthogonal, "intake", "check"),
        edge("record", "warnings", "html=1;curved=1;endArrow=open;", "convert", "store"),
    ])
    lanes = "\n".join([
        cell("lane", "Reader", "swimlane;html=1;startSize=26;"
             "fillColor=#E1D5E7;strokeColor=#9673A6;", 40, 40, 380, 130),
        cell("parse", "Parse", rounded, 30, 50, 120, 50, parent="lane"),
        cell("layout", "Build page IR", rounded, 210, 50, 130, 50, parent="lane"),
        cell("writer", "SVG writer", "shape=process;whiteSpace=wrap;html=1;", 130, 220, 180, 60),
        edge("step", "", orthogonal, "parse", "layout"),
        edge("emit", "one page", orthogonal, "layout", "writer"),
    ])
    body = "\n".join([
        '<mxfile host="document-svg" version="1.0">',
        '  <diagram id="flow" name="Conversion flow">',
        '    <mxGraphModel dx="900" dy="700" grid="1" gridSize="10" '
        'pageWidth="850" pageHeight="1100" background="#FFFFFF">',
        "      <root>",
        '        <mxCell id="0"/>',
        '        <mxCell id="1" parent="0"/>',
        flow,
        "      </root>",
        "    </mxGraphModel>",
        "  </diagram>",
        '  <diagram id="lanes" name="Reader stages">',
        '    <mxGraphModel dx="900" dy="700" grid="1" gridSize="10" '
        'pageWidth="850" pageHeight="1100">',
        "      <root>",
        '        <mxCell id="0"/>',
        '        <mxCell id="1" parent="0"/>',
        lanes,
        "      </root>",
        "    </mxGraphModel>",
        "  </diagram>",
        '  <diagram id="shapes" name="Shape libraries">',
        '    <mxGraphModel dx="900" dy="700" grid="1" gridSize="10" '
        'pageWidth="850" pageHeight="1100">',
        "      <root>",
        '        <mxCell id="0"/>',
        '        <mxCell id="1" parent="0"/>',
        library_page(),
        "      </root>",
        "    </mxGraphModel>",
        "  </diagram>",
        "</mxfile>",
        "",
    ])
    path.write_text(body, encoding="utf-8")


def build_stencils(path: Path) -> None:
    """Write a small shape library, in the mxStencil form draw.io uses.

    draw.io's own libraries are not redistributed here, so the sample carries
    one written for it. `docsvg --stencils` reads this file the same way it
    reads the editor's.
    """

    path.write_text("""<shapes name="mxgraph.docsvg">
  <shape name="Gauge" w="100" h="100" aspect="fixed" strokewidth="inherit">
    <connections/>
    <background><ellipse x="0" y="0" w="100" h="100"/></background>
    <foreground>
      <fillstroke/>
      <path>
        <move x="50" y="50"/><line x="82" y="30"/>
        <move x="12" y="50"/><line x="20" y="50"/>
        <move x="88" y="50"/><line x="80" y="50"/>
        <move x="50" y="12"/><line x="50" y="20"/>
      </path>
      <stroke/>
      <fillcolor color="#2D6195"/>
      <ellipse x="44" y="44" w="12" h="12"/>
      <fill/>
    </foreground>
  </shape>
  <shape name="Tray" w="120" h="60" aspect="variable" strokewidth="inherit">
    <connections/>
    <background>
      <path>
        <move x="0" y="12"/><line x="12" y="0"/><line x="120" y="0"/>
        <line x="120" y="48"/><line x="108" y="60"/><line x="0" y="60"/>
        <close/>
      </path>
    </background>
    <foreground>
      <fillstroke/>
      <path><move x="0" y="12"/><line x="108" y="12"/><line x="120" y="0"/></path>
      <path><move x="108" y="12"/><line x="108" y="60"/></path>
      <stroke/>
    </foreground>
  </shape>
</shapes>
""", encoding="utf-8")


def library_page() -> str:
    """Cells for the page that draws shapes from a library and from itself."""

    inline = (
        '<shape h="10" w="10" aspect="variable" strokewidth="inherit"><connections/>'
        '<background><path><move x="0" y="10"/><line x="5" y="0"/><line x="10" y="10"/>'
        '<close/></path></background><foreground><fillstroke/></foreground></shape>'
    )
    quoted = urllib.parse.quote(inline, safe="!~*'()")
    compressor = zlib.compressobj(9, zlib.DEFLATED, -15)
    packed = base64.b64encode(compressor.compress(quoted.encode()) + compressor.flush()).decode()
    cells = [
        '        <mxCell id="gauge" value="From a library" '
        'style="shape=mxgraph.docsvg.gauge;html=1;verticalLabelPosition=bottom;verticalAlign=top;'
        'fillColor=#DAE8FC;strokeColor=#6C8EBF;" vertex="1" parent="1">\n'
        '          <mxGeometry x="40" y="40" width="100" height="100" as="geometry"/>\n'
        "        </mxCell>",
        '        <mxCell id="tray" value="Also from a library" '
        'style="shape=mxgraph.docsvg.tray;html=1;verticalLabelPosition=bottom;verticalAlign=top;'
        'fillColor=#D5E8D4;strokeColor=#82B366;" vertex="1" parent="1">\n'
        '          <mxGeometry x="200" y="60" width="160" height="80" as="geometry"/>\n'
        "        </mxCell>",
        f'        <mxCell id="inline" value="Carried by this file" '
        f'style="shape=stencil({packed});html=1;verticalLabelPosition=bottom;verticalAlign=top;'
        f'fillColor=#FFF2CC;strokeColor=#D6B656;" vertex="1" parent="1">\n'
        '          <mxGeometry x="420" y="60" width="100" height="80" as="geometry"/>\n'
        "        </mxCell>",
        '        <mxCell id="note" value="The first two need --stencils; the third needs nothing." '
        'style="text;html=1;align=left;verticalAlign=middle;whiteSpace=wrap;" vertex="1" parent="1">\n'
        '          <mxGeometry x="40" y="180" width="480" height="30" as="geometry"/>\n'
        "        </mxCell>",
    ]
    return "\n".join(cells)


# --------------------------------------------------------------------------
# CAD (DXF, Gerber, HP-GL)
# --------------------------------------------------------------------------


def build_dxf(path: Path) -> None:
    """Write an architectural floor plan sample in standard ASCII DXF format."""
    content = """0
SECTION
2
HEADER
9
$ACADVER
1
AC1009
0
ENDSEC
0
SECTION
2
TABLES
0
TABLE
2
LAYER
70
4
0
LAYER
2
WALLS
70
0
62
7
6
CONTINUOUS
0
LAYER
2
DOORS
70
0
62
1
6
CONTINUOUS
0
LAYER
2
FURNITURE
70
0
62
3
6
CONTINUOUS
0
LAYER
2
TEXT
70
0
62
4
6
CONTINUOUS
0
ENDTAB
0
ENDSEC
0
SECTION
2
ENTITIES
0
LINE
8
WALLS
10
0.0
20
0.0
11
400.0
21
0.0
0
LINE
8
WALLS
10
400.0
20
0.0
11
400.0
21
300.0
0
LINE
8
WALLS
10
400.0
20
300.0
11
0.0
21
300.0
0
LINE
8
WALLS
10
0.0
20
300.0
11
0.0
21
0.0
0
LINE
8
WALLS
10
150.0
20
0.0
11
150.0
21
300.0
0
LINE
8
DOORS
10
150.0
20
80.0
11
190.0
21
120.0
0
ARC
8
DOORS
10
150.0
20
80.0
40
40.0
50
0.0
51
90.0
0
CIRCLE
8
FURNITURE
10
280.0
20
150.0
40
45.0
0
TEXT
8
TEXT
10
50.0
20
160.0
40
14.0
1
CONFERENCE ROOM
0
TEXT
8
TEXT
10
230.0
20
250.0
40
14.0
1
OFFICE 101
0
ENDSEC
0
EOF
"""
    path.write_text(content, encoding="utf-8")


def build_gerber(path: Path) -> None:
    """Write a printed circuit board (PCB) sample in Gerber RS-274X format."""
    content = """%FSLAX25Y25*%
%MOMM*%
%LPD*%
%ADD10C,0.3000*%
%ADD11C,0.8000*%
%ADD12R,1.6000X1.6000*%
%ADD13O,2.0000X1.2000*%
D10*
X0500000Y0500000D02*
X4500000Y0500000D01*
X4500000Y3500000D01*
X0500000Y3500000D01*
X0500000Y0500000D01*
D12*
X1500000Y1500000D03*
X1500000Y2500000D03*
D13*
X3500000Y1500000D03*
X3500000Y2500000D03*
D11*
X2500000Y2000000D03*
D10*
X1500000Y1500000D02*
X2500000Y2000000D01*
X3500000Y2500000D01*
G36*
X2000000Y0800000D02*
X3000000Y0800000D01*
X3000000Y1200000D01*
X2000000Y1200000D01*
X2000000Y0800000D01*
G37*
M02*
"""
    path.write_text(content, encoding="utf-8")


def build_hpgl(path: Path) -> None:
    """Write an engineering plot in HP-GL / HP-GL/2 format."""
    content = (
        "IN;DF;IP0,0,10000,7500;SC0,1000,0,750;"
        "SP1;PW0.35;"
        "PA100,100;PD900,100,900,650,100,650,100,100;PU;"
        "SP2;PW0.5;"
        "PA500,375;CI200;"
        "SP3;PW0.25;"
        "PA300,375;PD700,375;PU;"
        "PA500,175;PD500,575;PU;"
        "SP4;PW0.35;"
        "PA500,375;AA500,375,90;"
        "SP0;\n"
    )
    path.write_text(content, encoding="utf-8")


def build_gcode(path: Path) -> None:
    """Write sample CNC milling G-code toolpath."""
    content = (
        "; Sample CNC milling toolpath\n"
        "G21 ; metric\n"
        "G90 ; absolute positioning\n"
        "G00 Z5.000 ; rapid lift\n"
        "G00 X10.000 Y10.000 ; rapid to start\n"
        "M03 S12000 ; spindle on\n"
        "G01 Z-1.500 F300 ; plunge\n"
        "G01 X90.000 Y10.000 F800 ; linear cut\n"
        "G02 X100.000 Y20.000 I0.0 J10.0 ; clockwise arc corner\n"
        "G01 X100.000 Y80.000 ; linear cut\n"
        "G02 X90.000 Y90.000 I-10.0 J0.0 ; clockwise arc corner\n"
        "G01 X10.000 Y90.000 ; linear cut\n"
        "G02 X0.000 Y80.000 I0.0 J-10.0 ; clockwise arc corner\n"
        "G01 X0.000 Y20.000 ; linear cut\n"
        "G02 X10.000 Y10.000 I10.0 J0.0 ; clockwise arc corner\n"
        "G00 Z5.000 ; rapid retract\n"
        "M05 ; spindle off\n"
        "M02 ; end of program\n"
    )
    path.write_text(content, encoding="utf-8")


def build_excellon(path: Path) -> None:
    """Write sample Excellon PCB drill file."""
    content = (
        "; Sample Excellon drill file\n"
        "M48\n"
        "METRIC,TZ\n"
        "T01C0.8\n"
        "T02C1.5\n"
        "T03C3.2\n"
        "%\n"
        "T01\n"
        "X10.000Y10.000\n"
        "X10.000Y20.000\n"
        "X20.000Y10.000\n"
        "X20.000Y20.000\n"
        "T02\n"
        "X50.000Y50.000\n"
        "X60.000Y50.000\n"
        "T03\n"
        "X5.000Y5.000\n"
        "X95.000Y5.000\n"
        "X95.000Y95.000\n"
        "X5.000Y95.000\n"
        "M30\n"
    )
    path.write_text(content, encoding="utf-8")


def build_stl(path: Path) -> None:
    """Write sample ASCII STL 3D geometry."""
    content = (
        "solid tetrahedron\n"
        "  facet normal 0 0 -1\n"
        "    outer loop\n"
        "      vertex 0 0 0\n"
        "      vertex 50 0 0\n"
        "      vertex 25 43.3 0\n"
        "    endloop\n"
        "  endfacet\n"
        "  facet normal 0 -0.816 0.577\n"
        "    outer loop\n"
        "      vertex 0 0 0\n"
        "      vertex 25 14.4 40\n"
        "      vertex 50 0 0\n"
        "    endloop\n"
        "  endfacet\n"
        "  facet normal 0.707 0.408 0.577\n"
        "    outer loop\n"
        "      vertex 50 0 0\n"
        "      vertex 25 14.4 40\n"
        "      vertex 25 43.3 0\n"
        "    endloop\n"
        "  endfacet\n"
        "  facet normal -0.707 0.408 0.577\n"
        "    outer loop\n"
        "      vertex 25 43.3 0\n"
        "      vertex 25 14.4 40\n"
        "      vertex 0 0 0\n"
        "    endloop\n"
        "  endfacet\n"
        "endsolid tetrahedron\n"
    )
    path.write_text(content, encoding="utf-8")


def build_vtk(path: Path) -> None:
    """Write sample VTK Legacy ASCII scalar simulation field."""
    content = (
        "# vtk DataFile Version 3.0\n"
        "Thermal stress 2D quad mesh\n"
        "ASCII\n"
        "DATASET UNSTRUCTURED_GRID\n"
        "POINTS 6 float\n"
        "0.0 0.0 0.0\n"
        "50.0 0.0 0.0\n"
        "100.0 0.0 0.0\n"
        "0.0 50.0 0.0\n"
        "50.0 50.0 0.0\n"
        "100.0 50.0 0.0\n"
        "CELLS 2 10\n"
        "4 0 1 4 3\n"
        "4 1 2 5 4\n"
        "CELL_TYPES 2\n"
        "9\n"
        "9\n"
        "CELL_DATA 2\n"
        "SCALARS temperature float 1\n"
        "LOOKUP_TABLE default\n"
        "25.5\n"
        "180.2\n"
    )
    path.write_text(content, encoding="utf-8")


def build_msh(path: Path) -> None:
    """Write sample Gmsh 2.2 FEA triangular/quad mesh."""
    content = (
        "$MeshFormat\n"
        "2.2 0 8\n"
        "$EndMeshFormat\n"
        "$Nodes\n"
        "4\n"
        "1 0.0 0.0 0.0\n"
        "2 100.0 0.0 0.0\n"
        "3 100.0 80.0 0.0\n"
        "4 0.0 80.0 0.0\n"
        "$EndNodes\n"
        "$Elements\n"
        "2\n"
        "1 2 2 1 1 1 2 3\n"
        "2 2 2 1 1 1 3 4\n"
        "$EndElements\n"
    )
    path.write_text(content, encoding="utf-8")


def build_su2(path: Path) -> None:
    """Write a small 2D SU2 mesh with four named boundary markers."""
    content = """NDIME= 2
NELEM= 4
5 0 1 4 0
5 1 2 4 1
5 2 3 4 2
5 3 0 4 3
NPOIN= 5
0.0 0.0 0
1.0 0.0 1
1.0 1.0 2
0.0 1.0 3
0.5 0.5 4
NMARK= 4
MARKER_TAG= lower
MARKER_ELEMS= 1
3 0 1
MARKER_TAG= right
MARKER_ELEMS= 1
3 1 2
MARKER_TAG= upper
MARKER_ELEMS= 1
3 2 3
MARKER_TAG= left
MARKER_ELEMS= 1
3 3 0
"""
    path.write_text(content, encoding="ascii")


def build_step(path: Path) -> None:
    """Write sample STEP ISO 10303-21 CAD assembly wireframe."""
    content = (
        "ISO-10303-21;\n"
        "HEADER;\n"
        "FILE_DESCRIPTION(('STEP AP203 Mechanical sample'),'2;1');\n"
        "FILE_NAME('sample.step','2026-09-10',('Engineer'),('Testing'),'','','');\n"
        "FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));\n"
        "ENDSEC;\n"
        "DATA;\n"
        "#10 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));\n"
        "#11 = CARTESIAN_POINT('', (60.0, 0.0, 0.0));\n"
        "#12 = CARTESIAN_POINT('', (60.0, 40.0, 0.0));\n"
        "#13 = CARTESIAN_POINT('', (0.0, 40.0, 0.0));\n"
        "#14 = CARTESIAN_POINT('', (0.0, 0.0, 30.0));\n"
        "#15 = CARTESIAN_POINT('', (60.0, 0.0, 30.0));\n"
        "#16 = CARTESIAN_POINT('', (60.0, 40.0, 30.0));\n"
        "#17 = CARTESIAN_POINT('', (0.0, 40.0, 30.0));\n"
        "#20 = VERTEX_POINT('', #10);\n"
        "#21 = VERTEX_POINT('', #11);\n"
        "#22 = VERTEX_POINT('', #12);\n"
        "#23 = VERTEX_POINT('', #13);\n"
        "#24 = VERTEX_POINT('', #14);\n"
        "#25 = VERTEX_POINT('', #15);\n"
        "#26 = VERTEX_POINT('', #16);\n"
        "#27 = VERTEX_POINT('', #17);\n"
        "#30 = EDGE_CURVE('', #20, #21, #0, .T.);\n"
        "#31 = EDGE_CURVE('', #21, #22, #0, .T.);\n"
        "#32 = EDGE_CURVE('', #22, #23, #0, .T.);\n"
        "#33 = EDGE_CURVE('', #23, #20, #0, .T.);\n"
        "#34 = EDGE_CURVE('', #24, #25, #0, .T.);\n"
        "#35 = EDGE_CURVE('', #25, #26, #0, .T.);\n"
        "#36 = EDGE_CURVE('', #26, #27, #0, .T.);\n"
        "#37 = EDGE_CURVE('', #27, #24, #0, .T.);\n"
        "#38 = EDGE_CURVE('', #20, #24, #0, .T.);\n"
        "#39 = EDGE_CURVE('', #21, #25, #0, .T.);\n"
        "#40 = EDGE_CURVE('', #22, #26, #0, .T.);\n"
        "#41 = EDGE_CURVE('', #23, #27, #0, .T.);\n"
        "ENDSEC;\n"
        "END-ISO-10303-21;\n"
    )
    path.write_text(content, encoding="utf-8")


def build_ifc(path: Path) -> None:
    """Write a small IFC4 tessellation with two placed building proxies."""
    content = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('Synthetic IFC4 tessellation sample'),'2;1');
FILE_NAME('sample.ifc','2026-09-14T00:00:00',(''),'','document-svg','','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCCARTESIANPOINTLIST3D(((0.,0.,0.),(2000.,0.,0.),(2000.,1000.,0.),(0.,1000.,0.),(0.,0.,1000.),(2000.,0.,1000.),(2000.,1000.,1000.),(0.,1000.,1000.)),$);
#2=IFCTRIANGULATEDFACESET(#1,$,.T.,((1,4,3),(1,3,2),(5,6,7),(5,7,8),(1,2,6),(1,6,5),(2,3,7),(2,7,6),(3,4,8),(3,8,7),(4,1,5),(4,5,8)),$);
#3=IFCSHAPEREPRESENTATION($,'Body','Tessellation',(#2));
#4=IFCPRODUCTDEFINITIONSHAPE($,$,(#3));
#5=IFCCARTESIANPOINT((10000.,5000.,0.));
#6=IFCDIRECTION((0.,0.,1.));
#7=IFCDIRECTION((0.,1.,0.));
#8=IFCAXIS2PLACEMENT3D(#5,#6,#7);
#9=IFCLOCALPLACEMENT(#25,#8);
#10=IFCBUILDINGELEMENTPROXY('2L$!ProxyA',$,'Preview column',$,$,#9,#4,$,$);
#11=IFCCARTESIANPOINT((16000.,5000.,0.));
#12=IFCAXIS2PLACEMENT3D(#11,#6,#7);
#13=IFCLOCALPLACEMENT(#25,#12);
#14=IFCBUILDINGELEMENTPROXY('2L$!ProxyB',$,'Preview wall',$,$,#13,#4,$,$);
#15=IFCCARTESIANPOINT((0.,0.,0.));
#16=IFCAXIS2PLACEMENT3D(#15,$,$);
#17=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.E-5,#16,$);
#18=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#19=IFCUNITASSIGNMENT((#18));
#20=IFCPROJECT('2L$!Project',$,'Sample IFC model',$,$,$,$,(#17),#19);
#21=IFCLOCALPLACEMENT($,#16);
#22=IFCSITE('2L$!Site',$,'Site',$,$,#21,$,'Site',.ELEMENT.,$,$,$,$,$);
#23=IFCLOCALPLACEMENT(#21,#16);
#24=IFCBUILDING('2L$!Building',$,'Building',$,$,#23,$,'Building',.ELEMENT.,$,$);
#25=IFCLOCALPLACEMENT(#23,#16);
#26=IFCBUILDINGSTOREY('2L$!Storey',$,'Ground floor',$,$,#25,$,'Ground floor',.ELEMENT.,0.);
#27=IFCRELAGGREGATES('2L$!ProjectSite',$,$,$,#20,(#22));
#28=IFCRELAGGREGATES('2L$!SiteBuilding',$,$,$,#22,(#24));
#29=IFCRELAGGREGATES('2L$!BuildingStorey',$,$,$,#24,(#26));
#30=IFCRELCONTAINEDINSPATIALSTRUCTURE('2L$!Contains',$,$,$,(#10,#14),#26);
ENDSEC;
END-ISO-10303-21;
"""
    path.write_text(content, encoding="utf-8")


def build_ifczip(path: Path) -> None:
    """Package the generated root IFC SPF sample in the IFCZIP convention."""
    import zipfile

    info = zipfile.ZipInfo("sample.ifc", date_time=(1980, 1, 1, 0, 0, 0))
    info.create_system = 3
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = 0o600 << 16
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr(info, (SOURCE / "sample.ifc").read_bytes())


def build_obj(path: Path) -> None:
    """Write sample Wavefront OBJ 3D model."""
    content = (
        "# Sample 3D Pyramid OBJ\n"
        "v 0.0 0.0 0.0\n"
        "v 60.0 0.0 0.0\n"
        "v 60.0 60.0 0.0\n"
        "v 0.0 60.0 0.0\n"
        "v 30.0 30.0 50.0\n"
        "f 1 2 3 4\n"
        "f 1 2 5\n"
        "f 2 3 5\n"
        "f 3 4 5\n"
        "f 4 1 5\n"
    )
    path.write_text(content, encoding="utf-8")


def build_dot(path: Path) -> None:
    """Write sample Graphviz DOT architecture diagram."""
    content = (
        "digraph CloudMicroservices {\n"
        "  rankdir=LR;\n"
        "  node [shape=box, style=filled, fillcolor=\"#f8fafc\", color=\"#64748b\", fontname=\"sans-serif\"];\n"
        "  edge [color=\"#475569\", fontname=\"sans-serif\", fontsize=10];\n\n"
        "  Client [label=\"Client (Web/Mobile)\", shape=ellipse, fillcolor=\"#e0e7ff\", color=\"#4338ca\"];\n"
        "  CDN [label=\"Cloud CDN & WAF\", fillcolor=\"#e0f2fe\", color=\"#0284c7\"];\n"
        "  Gateway [label=\"API Gateway\", fillcolor=\"#dbeafe\", color=\"#2563eb\"];\n\n"
        "  AuthService [label=\"Auth Service (OAuth2/JWT)\", fillcolor=\"#fef3c7\", color=\"#d97706\"];\n"
        "  UserService [label=\"User Service\", fillcolor=\"#f1f5f9\", color=\"#475569\"];\n"
        "  OrderService [label=\"Order & Checkout Service\", fillcolor=\"#f1f5f9\", color=\"#475569\"];\n"
        "  PaymentService [label=\"Payment Gateway Service\", fillcolor=\"#fee2e2\", color=\"#dc2626\"];\n\n"
        "  UserDB [label=\"User DB (PostgreSQL)\", shape=cylinder, fillcolor=\"#ecfdf5\", color=\"#059669\"];\n"
        "  OrderDB [label=\"Order DB (PostgreSQL)\", shape=cylinder, fillcolor=\"#ecfdf5\", color=\"#059669\"];\n"
        "  RedisCache [label=\"Redis Cache Cluster\", shape=box, fillcolor=\"#fef2f2\", color=\"#ef4444\"];\n"
        "  MsgQueue [label=\"Kafka Message Queue\", shape=cds, fillcolor=\"#fefce8\", color=\"#ca8a04\"];\n"
        "  AnalyticsWorker [label=\"Analytics & Event Consumer\", fillcolor=\"#f3e8ff\", color=\"#7e22ce\"];\n\n"
        "  Client -> CDN [label=\"HTTPS / TLS 1.3\"];\n"
        "  CDN -> Gateway [label=\"Reverse Proxy\"];\n"
        "  Gateway -> AuthService [label=\"Validate Token\"];\n"
        "  Gateway -> UserService [label=\"REST /users\"];\n"
        "  Gateway -> OrderService [label=\"REST /orders\"];\n"
        "  OrderService -> PaymentService [label=\"gRPC /charge\"];\n\n"
        "  UserService -> UserDB [label=\"Read / Write\"];\n"
        "  UserService -> RedisCache [label=\"Query Cache\"];\n"
        "  OrderService -> OrderDB [label=\"ACID Transactions\"];\n"
        "  OrderService -> MsgQueue [label=\"Emit OrderCreated\"];\n"
        "  MsgQueue -> AnalyticsWorker [label=\"Consume Events\"];\n"
        "}\n"
    )
    path.write_text(content, encoding="utf-8")


def build_mermaid(path: Path) -> None:
    """Write sample Mermaid sequence diagram."""
    content = (
        "sequenceDiagram\n"
        "  autonumber\n"
        "  actor User as End User\n"
        "  participant Browser as Web Browser (SPA)\n"
        "  participant AuthServer as OAuth 2.0 Auth Server\n"
        "  participant Gateway as API Gateway\n"
        "  participant ResourceAPI as Protected Resource API\n\n"
        "  User->>Browser: Navigate to Login Page\n"
        "  Browser->>AuthServer: GET /oauth/authorize?client_id=web&scope=openid\n"
        "  AuthServer-->>Browser: 302 Redirect with Auth Code\n"
        "  Browser->>Gateway: POST /api/auth/callback (code, state)\n"
        "  Gateway->>AuthServer: POST /oauth/token (code, client_secret)\n"
        "  AuthServer-->>Gateway: 200 OK (access_token, id_token, refresh_token)\n"
        "  Gateway-->>Browser: Set-Cookie: session_jwt (HttpOnly, Secure)\n"
        "  Browser->>Gateway: GET /api/v1/user/profile (Cookie: session_jwt)\n"
        "  Gateway->>ResourceAPI: Forward Request (X-User-Id: 42)\n"
        "  ResourceAPI-->>Gateway: 200 OK { id: 42, name: \"Alice\", role: \"admin\" }\n"
        "  Gateway-->>Browser: JSON User Profile Response\n"
    )
    path.write_text(content, encoding="utf-8")


def build_tex(path: Path) -> None:
    """Write sample LaTeX mathematical formula."""
    content = r"\frac{1}{\sqrt{2\pi \sigma^2}} \sum (x_i - \mu)^2 + \lambda \sqrt{w^2 + \epsilon}" + "\n"
    path.write_text(content, encoding="utf-8")


def build_chart(path: Path) -> None:
    """Write sample Chart specification JSON."""
    spec = {
        "type": "bar",
        "title": "2026 Fiscal Performance (USD Millions)",
        "labels": ["Q1", "Q2", "Q3", "Q4"],
        "series": [
            {
                "name": "Gross Revenue",
                "data": [145.2, 182.6, 210.4, 268.9],
                "color": "#3b82f6",
            },
            {
                "name": "Operating Cost",
                "data": [95.0, 110.5, 128.0, 142.3],
                "color": "#ef4444",
            },
            {
                "name": "Net Income",
                "data": [50.2, 72.1, 82.4, 126.6],
                "color": "#10b981",
            },
        ],
    }
    path.write_text(json.dumps(spec, indent=2) + "\n", encoding="utf-8")


def build_markdown(path: Path) -> None:
    """Write sample GFM Markdown table."""
    content = (
        "# Cloud Compute Instance Offerings\n\n"
        "| Instance Family | vCPU | Memory (GiB) | Storage | Network Bandwidth | Hourly Rate | Status |\n"
        "| :--- | :---: | :---: | :--- | :---: | ---: | :---: |\n"
        "| c7g.large | 2 | 4.0 | EBS-Only | Up to 12.5 Gbps | $0.0725 | Active |\n"
        "| c7g.xlarge | 4 | 8.0 | EBS-Only | Up to 12.5 Gbps | $0.1450 | Active |\n"
        "| m7g.xlarge | 4 | 16.0 | EBS-Only | Up to 12.5 Gbps | $0.1632 | Active |\n"
        "| r7g.2xlarge | 8 | 64.0 | 1x 474 NVMe | 15 Gbps | $0.4352 | Active |\n"
        "| g5g.4xlarge | 16 | 64.0 | 1x 950 NVMe | 25 Gbps | $1.1760 | Beta |\n"
        "| p5.48xlarge | 192 | 2048.0 | 8x 3.84TB NVMe | 3200 Gbps | $98.3200 | Dedicated |\n"
    )
    path.write_text(content, encoding="utf-8")


def build_qr(path: Path) -> None:
    """Write sample QR payload URL."""
    path.write_text("https://github.com/ryusui-hiro/document-svg\n", encoding="utf-8")


def build_raster(path: Path) -> None:
    """Write sample monochrome raster signature/stamp PNG image."""
    import math

    width, height = 120, 60

    def pixel(x: int, y: int) -> tuple[int, int, int]:
        dx1 = x - 35
        dy1 = y - 30
        in_loop = 180 <= (dx1 * dx1 * 1.5 + dy1 * dy1 * 3.0) <= 320
        in_swoosh = abs(y - (30 + int(12 * math.sin(x / 14.0)))) <= 2 and 15 <= x <= 105
        in_underline = (y == 48 or y == 49) and 25 <= x <= 95
        if in_loop or in_swoosh or in_underline:
            return (20, 20, 30)
        return (255, 255, 255)

    path.write_bytes(png_bytes(width, height, pixel))


def build_ascii_grid(path: Path) -> None:
    """Write a small elevation grid with a transparent NoData coastline."""
    rows = [
        [-9999, -9999, 12, 13, 15, 17, 19, 20, 19, 17, 15, 13, 12, -9999],
        [-9999, 12, 14, 16, 18, 21, 23, 24, 23, 21, 18, 16, 14, 12],
        [12, 14, 17, 20, 24, 28, 31, 32, 31, 28, 24, 20, 17, 14],
        [13, 16, 20, 25, 30, 34, 37, 38, 37, 34, 30, 25, 20, 16],
        [14, 18, 23, 29, 34, 39, 42, 43, 42, 39, 34, 29, 23, 18],
        [13, 16, 20, 25, 30, 34, 37, 38, 37, 34, 30, 25, 20, 16],
        [-9999, 14, 17, 20, 24, 28, 31, 32, 31, 28, 24, 20, 17, 14],
        [-9999, -9999, 12, 13, 15, 17, 19, 20, 19, 17, 15, 13, 12, -9999],
    ]
    text = [
        "ncols 14",
        f"nrows {len(rows)}",
        "xllcorner 134.50",
        "yllcorner 34.50",
        "cellsize 0.01",
        "NODATA_VALUE -9999",
        *(" ".join(map(str, row)) for row in rows),
    ]
    path.write_text("\n".join(text) + "\n", encoding="ascii")


def build_dbase(path: Path) -> None:
    """Write a dBASE III parcel table with varied values and one deleted row."""
    import struct

    fields = [
        ("PARCEL_ID", b"N", 5, 0),
        ("OWNER", b"C", 24, 0),
        ("AREA_HA", b"F", 10, 2),
        ("INSPECT", b"L", 1, 0),
        ("BUILT", b"D", 8, 0),
    ]
    records = [
        ("01001", "Café Moreno", "12.75", "Y", "20080922", False),
        ("01002", "Willow Farm", "8.50", "N", "19960510", True),
        ("01003", "Riverside Co-op", "24.10", "Y", "20141103", False),
        ("01004", "North Orchard", "5.25", "?", "00000000", False),
        ("01005", "Juniper Estate", "31.80", "Y", "20220719", False),
        ("01006", "West Meadow", "16.40", "N", "19840301", False),
        ("01007", "Hillview Garden", "3.05", "Y", "20180730", False),
    ]
    header_length = 32 + len(fields) * 32 + 1
    record_length = 1 + sum(width for _, _, width, _ in fields)
    header = bytearray(header_length)
    header[0:4] = bytes((0x03, 125, 5, 28))
    struct.pack_into("<IHH", header, 4, len(records), header_length, record_length)
    header[29] = 0x57  # Windows-1252 language driver
    for index, (name, kind, width, decimals) in enumerate(fields):
        offset = 32 + index * 32
        header[offset:offset + len(name)] = name.encode("ascii")
        header[offset + 11] = kind[0]
        header[offset + 16] = width
        header[offset + 17] = decimals
    header[-1] = 0x0D
    payload = bytearray(header)
    for parcel_id, owner, area, inspected, built, deleted in records:
        payload.append(ord("*") if deleted else ord(" "))
        payload.extend(parcel_id.rjust(5).encode("ascii"))
        payload.extend(owner.encode("cp1252").ljust(24, b" "))
        payload.extend(area.rjust(10).encode("ascii"))
        payload.extend(inspected.encode("ascii"))
        payload.extend(built.encode("ascii"))
    payload.append(0x1A)
    path.write_bytes(payload)


def build_geopackage(path: Path) -> None:
    """Write a GeoPackage with a vector polygon layer and a raster tile layer."""
    import math
    import sqlite3

    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        path.unlink()
    connection = sqlite3.connect(path)
    connection.executescript(
        """
        PRAGMA application_id = 1196444487;
        PRAGMA user_version = 10400;
        CREATE TABLE gpkg_spatial_ref_sys (
            srs_name TEXT NOT NULL, srs_id INTEGER PRIMARY KEY,
            organization TEXT NOT NULL, organization_coordsys_id INTEGER NOT NULL,
            definition TEXT NOT NULL, description TEXT
        );
        CREATE TABLE gpkg_contents (
            table_name TEXT NOT NULL PRIMARY KEY, data_type TEXT NOT NULL,
            identifier TEXT UNIQUE, description TEXT DEFAULT '',
            last_change DATETIME NOT NULL,
            min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER
        );
        CREATE TABLE gpkg_geometry_columns (
            table_name TEXT NOT NULL, column_name TEXT NOT NULL,
            geometry_type_name TEXT NOT NULL, srs_id INTEGER NOT NULL,
            z TINYINT NOT NULL, m TINYINT NOT NULL,
            PRIMARY KEY (table_name, column_name)
        );
        CREATE TABLE gpkg_tile_matrix_set (
            table_name TEXT NOT NULL PRIMARY KEY, srs_id INTEGER NOT NULL,
            min_x DOUBLE NOT NULL, min_y DOUBLE NOT NULL,
            max_x DOUBLE NOT NULL, max_y DOUBLE NOT NULL
        );
        CREATE TABLE gpkg_tile_matrix (
            table_name TEXT NOT NULL, zoom_level INTEGER NOT NULL,
            matrix_width INTEGER NOT NULL, matrix_height INTEGER NOT NULL,
            tile_width INTEGER NOT NULL, tile_height INTEGER NOT NULL,
            pixel_x_size DOUBLE NOT NULL, pixel_y_size DOUBLE NOT NULL,
            PRIMARY KEY (table_name, zoom_level)
        );
        """
    )
    connection.executemany(
        "INSERT INTO gpkg_spatial_ref_sys VALUES (?,?,?,?,?,?)",
        [
            ("Undefined Cartesian", -1, "NONE", -1, "undefined", ""),
            ("Undefined Geographic", 0, "NONE", 0, "undefined", ""),
            (
                "WGS 84",
                4326,
                "EPSG",
                4326,
                'GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]',
                "",
            ),
            (
                "WGS 84 / Pseudo-Mercator",
                3857,
                "EPSG",
                3857,
                'PROJCS["WGS 84 / Pseudo-Mercator",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Mercator_1SP"],PARAMETER["central_meridian",0],PARAMETER["scale_factor",1],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1]]',
                "",
            ),
        ],
    )

    def polygon_blob(rings: list[list[tuple[float, float]]]) -> bytes:
        wkb = bytearray(b"\x01" + struct.pack("<II", 3, len(rings)))
        for ring in rings:
            wkb.extend(struct.pack("<I", len(ring)))
            for coordinate in ring:
                wkb.extend(struct.pack("<2d", *coordinate))
        xs = [point[0] for ring in rings for point in ring]
        ys = [point[1] for ring in rings for point in ring]
        envelope = (min(xs), max(xs), min(ys), max(ys))
        return b"GP\x00\x03" + struct.pack("<i4d", 4326, *envelope) + bytes(wkb)

    outer = [(139.0, 35.0), (141.0, 35.0), (141.0, 37.0), (139.0, 37.0), (139.0, 35.0)]
    hole = [(139.7, 35.7), (139.7, 36.3), (140.3, 36.3), (140.3, 35.7), (139.7, 35.7)]
    harbor = [(141.2, 35.3), (141.8, 35.3), (141.8, 35.9), (141.2, 35.9), (141.2, 35.3)]
    connection.execute("CREATE TABLE districts (id INTEGER PRIMARY KEY, geom POLYGON, name TEXT)")
    connection.executemany(
        "INSERT INTO districts VALUES (?,?,?)",
        [
            (1, polygon_blob([outer, hole]), "Central district"),
            (2, polygon_blob([harbor]), "Harbor district"),
        ],
    )
    connection.execute(
        "INSERT INTO gpkg_contents(table_name,data_type,identifier,description,last_change,srs_id) "
        "VALUES('districts','features','Districts','Example polygon layer','2026-09-14T00:00:00.000Z',4326)"
    )
    connection.execute("INSERT INTO gpkg_geometry_columns VALUES('districts','geom','POLYGON',4326,0,0)")

    tile_width, tile_height = 256, 320
    radius = 6_378_137.0
    min_x = radius * math.radians(139.0)
    max_x = radius * math.radians(141.0)
    min_y = radius * math.log(math.tan(math.pi / 4 + math.radians(35.0) / 2))
    max_y = radius * math.log(math.tan(math.pi / 4 + math.radians(37.0) / 2))

    def map_pixel(x: int, y: int) -> tuple[int, int, int]:
        color = (242, 239, 226)
        if (18 <= x <= 86 and 30 <= y <= 100) or (160 <= x <= 232 and 190 <= y <= 276):
            color = (214, 231, 199)
        river_distance = abs(y - (228 - 0.47 * x))
        if river_distance < 11:
            color = (150, 204, 222)
        road_one = abs(y - (58 + 0.42 * x))
        road_two = abs(x - (196 - 0.22 * y))
        if road_one < 7 or road_two < 6:
            color = (201, 197, 187)
        if road_one < 4 or road_two < 3:
            color = (255, 252, 241)
        if river_distance < 11:
            color = (150, 204, 222)
        return color

    tile_png = png_bytes(tile_width, tile_height, map_pixel)
    connection.execute(
        "CREATE TABLE basemap (id INTEGER PRIMARY KEY, zoom_level INTEGER NOT NULL, "
        "tile_column INTEGER NOT NULL, tile_row INTEGER NOT NULL, tile_data BLOB NOT NULL, "
        "UNIQUE(zoom_level,tile_column,tile_row))"
    )
    connection.execute(
        "INSERT INTO gpkg_contents(table_name,data_type,identifier,description,last_change,srs_id) "
        "VALUES('basemap','tiles','Basemap','Self-authored illustrative map tile','2026-09-14T00:00:00.000Z',3857)"
    )
    connection.execute(
        "INSERT INTO gpkg_tile_matrix_set VALUES('basemap',3857,?,?,?,?)",
        (min_x, min_y, max_x, max_y),
    )
    connection.execute(
        "INSERT INTO gpkg_tile_matrix VALUES('basemap',10,1,1,?,?,?,?)",
        (tile_width, tile_height, (max_x - min_x) / tile_width, (max_y - min_y) / tile_height),
    )
    connection.execute("INSERT INTO basemap VALUES(1,10,0,0,?)", (tile_png,))

    connection.commit()
    connection.close()


def build_geojson_sequence(path: Path) -> None:
    """Write a heterogeneous RFC 8142 GeoJSON Text Sequence."""
    records = [
        {
            "type": "Feature",
            "id": "private-sample-point-id",
            "properties": {"name": "Private feature attribute"},
            "geometry": {"type": "Point", "coordinates": [139.7, 35.6]},
        },
        {
            "type": "LineString",
            "coordinates": [[139.0, 35.0], [139.7, 35.6], [140.3, 36.2]],
        },
        {
            "type": "FeatureCollection",
            "features": [
                {
                    "type": "Feature",
                    "properties": {"category": "omitted"},
                    "geometry": {
                        "type": "Polygon",
                        "coordinates": [[[139.3, 35.2], [140.0, 35.2], [140.0, 35.8], [139.3, 35.8], [139.3, 35.2]]],
                    },
                }
            ],
        },
    ]
    path.write_bytes(
        b"".join(
            b"\x1e" + json.dumps(record, separators=(",", ":"), ensure_ascii=False).encode("utf-8") + b"\n"
            for record in records
        )
    )


def build_topojson(path: Path) -> None:
    """Write a quantized TopoJSON topology with a shared polygon boundary."""
    topology = {
        "type": "Topology",
        "transform": {"scale": [0.01, 0.01], "translate": [139.0, 35.0]},
        "objects": {
            "districts": {
                "type": "GeometryCollection",
                "geometries": [
                    {"type": "Polygon", "arcs": [[0, 1, 2, 3]], "properties": {"name": "private west"}},
                    {"type": "Polygon", "arcs": [[4, 5, 6, -2]], "properties": {"name": "private east"}},
                ],
            },
            "stations": {
                "type": "Point",
                "coordinates": [150, 50],
                "properties": {"name": "private station"},
            },
        },
        # Arc 1 is shared by both district polygons; -2 uses it in reverse.
        # Every arc is delta-encoded according to the topology transform.
        "arcs": [
            [[0, 0], [100, 0]],
            [[100, 0], [0, 100]],
            [[100, 100], [-100, 0]],
            [[0, 100], [0, -100]],
            [[100, 0], [100, 0]],
            [[200, 0], [0, 100]],
            [[200, 100], [-100, 0]],
        ],
    }
    path.write_text(json.dumps(topology, indent=2) + "\n", encoding="utf-8")


def build_json_text_sequence(path: Path) -> None:
    """Write a heterogeneous RFC 7464 JSON Text Sequence."""
    records = [
        {"event": "ingest", "source": "field report", "features": 14},
        {"rows": [{"id": "A-1", "value": 12.5}, {"id": "B-2", "value": 8.75}]},
        ["validated", "rendered", "exported"],
        42,
    ]
    path.write_bytes(
        b"".join(
            b"\x1e" + json.dumps(record, separators=(",", ":"), ensure_ascii=False).encode("utf-8") + b"\n"
            for record in records
        )
    )


def build_toml(path: Path) -> None:
    """Write a nested inert TOML deployment configuration sample."""
    path.write_text(
        '''title = "Catalog deployment"
enabled = true
created_at = 2026-08-03T17:20:00Z
retry_delays = [1, 5, 30]
description = """Deploy the catalog API.
Configuration is shown as data and never executed."""

[service]
name = "catalog-api"
listen = "127.0.0.1:8080"
public = false

[service.limits]
requests_per_minute = 1200
burst = 50
error_rate = 0.25

[[targets]]
name = "staging"
regions = ["us-west-2", "eu-central-1"]

[[targets]]
name = "production"
regions = ["us-east-1", "ap-northeast-1"]
''',
        encoding="utf-8",
    )



def build_yaml(path: Path) -> None:
    """Write a YAML 1.2 config with sequences, an alias and a passive tag."""
    lines = [
        "---",
        "title: Catalog deployment",
        "enabled: true",
        "created_at: 2026-08-03T17:20:00Z",
        "retry_delays: [1, 5, 30]",
        "description: |",
        "  Deploy the catalog API.",
        "  Configuration is displayed as data.",
        "",
        "service:",
        "  name: catalog-api",
        "  listen: 127.0.0.1:8080",
        "  public: false",
        "  limits:",
        "    requests_per_minute: 1200",
        "    burst: 50",
        "    error_rate: 0.25",
        "",
        "targets:",
        "  - name: staging",
        "    regions: [us-west-2, eu-central-1]",
        "  - name: production",
        "    regions: [us-east-1, ap-northeast-1]",
        "",
        "defaults: &defaults",
        "  retries: 3",
        "use_defaults: *defaults",
        "literal_include: !include ./secrets.yml",
        "---",
        "kind: review",
        "request_id: 42",
        "...",
    ]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def build_generic_xml(path: Path) -> None:
    """Write a generic application configuration without a DTD or includes."""
    path.write_text(
        '''<?xml version="1.0" encoding="UTF-8"?>
<application xmlns="urn:example:application" version="2" environment="production">
  <!-- Values are previewed as data; no setting is executed. -->
  <service id="catalog-api">
    <endpoint>https://api.example.invalid/v1</endpoint>
    <listen host="127.0.0.1" port="8080" />
    <limits requests-per-minute="1200" burst="50" />
  </service>
  <feature name="search" enabled="true">full-text &amp; faceted</feature>
  <feature name="exports" enabled="false" />
</application>
''',
        encoding="utf-8",
    )


def build_properties(path: Path) -> None:
    """Write a Java Properties file with continuations, escapes and overrides."""
    path.write_text(
        r'''# Catalog service settings
app.name=Catalog API
app.region=eu-west-1
app.url=https\://api.example.invalid/v1
app.timeout.ms=1500
feature.search.enabled=true
welcome=Hello,\u0020Caf\u00E9!
workflow.steps=compile\
    test\
    package
literal.placeholder=${HOME} is displayed as text
maintenance.message=
release.channel=old
release.channel=stable
emoji=\uD83D\uDE80
''',
        encoding="ascii",
    )


def build_bpmn(path: Path) -> None:
    """Write a BPMN 2.0 order workflow with explicit BPMN DI geometry."""
    path.write_text(
        '''<?xml version="1.0" encoding="UTF-8"?>
<bpmn:definitions xmlns:bpmn="http://www.omg.org/spec/BPMN/20100524/MODEL"
 xmlns:bpmndi="http://www.omg.org/spec/BPMN/20100524/DI"
 xmlns:dc="http://www.omg.org/spec/DD/20100524/DC"
 xmlns:di="http://www.omg.org/spec/DD/20100524/DI"
 id="Definitions_Order" targetNamespace="https://example.invalid/process">
 <bpmn:process id="Process_Order" name="Order fulfillment" isExecutable="false">
  <bpmn:startEvent id="Start_Order" name="Order received" />
  <bpmn:userTask id="Validate_Order" name="Validate order" />
  <bpmn:exclusiveGateway id="Stock_Check" name="In stock?" />
  <bpmn:serviceTask id="Ship_Order" name="Ship order" />
  <bpmn:userTask id="Notify_Customer" name="Notify customer" />
  <bpmn:endEvent id="End_Order" name="Complete" />
  <bpmn:sequenceFlow id="Flow_Start_Validate" sourceRef="Start_Order" targetRef="Validate_Order" />
  <bpmn:sequenceFlow id="Flow_Validate_Stock" sourceRef="Validate_Order" targetRef="Stock_Check" />
  <bpmn:sequenceFlow id="Flow_In_Stock" name="yes" sourceRef="Stock_Check" targetRef="Ship_Order" />
  <bpmn:sequenceFlow id="Flow_Out_Stock" name="no" sourceRef="Stock_Check" targetRef="Notify_Customer" />
  <bpmn:sequenceFlow id="Flow_Ship_Complete" sourceRef="Ship_Order" targetRef="End_Order" />
  <bpmn:sequenceFlow id="Flow_Notify_Complete" sourceRef="Notify_Customer" targetRef="End_Order" />
 </bpmn:process>
 <bpmndi:BPMNDiagram id="Diagram_Order" name="Order fulfillment">
  <bpmndi:BPMNPlane bpmnElement="Process_Order">
   <bpmndi:BPMNShape id="Shape_Start" bpmnElement="Start_Order"><dc:Bounds x="40" y="180" width="36" height="36" /></bpmndi:BPMNShape>
   <bpmndi:BPMNShape id="Shape_Validate" bpmnElement="Validate_Order"><dc:Bounds x="140" y="160" width="120" height="76" /></bpmndi:BPMNShape>
   <bpmndi:BPMNShape id="Shape_Stock" bpmnElement="Stock_Check"><dc:Bounds x="320" y="173" width="52" height="52" /></bpmndi:BPMNShape>
   <bpmndi:BPMNShape id="Shape_Ship" bpmnElement="Ship_Order"><dc:Bounds x="470" y="70" width="130" height="76" /></bpmndi:BPMNShape>
   <bpmndi:BPMNShape id="Shape_Notify" bpmnElement="Notify_Customer"><dc:Bounds x="470" y="280" width="130" height="76" /></bpmndi:BPMNShape>
   <bpmndi:BPMNShape id="Shape_End" bpmnElement="End_Order"><dc:Bounds x="690" y="190" width="36" height="36" /></bpmndi:BPMNShape>
   <bpmndi:BPMNEdge id="Edge_Start_Validate" bpmnElement="Flow_Start_Validate"><di:waypoint x="76" y="198" /><di:waypoint x="140" y="198" /></bpmndi:BPMNEdge>
   <bpmndi:BPMNEdge id="Edge_Validate_Stock" bpmnElement="Flow_Validate_Stock"><di:waypoint x="260" y="198" /><di:waypoint x="320" y="199" /></bpmndi:BPMNEdge>
   <bpmndi:BPMNEdge id="Edge_In_Stock" bpmnElement="Flow_In_Stock"><di:waypoint x="360" y="173" /><di:waypoint x="410" y="108" /><di:waypoint x="470" y="108" /></bpmndi:BPMNEdge>
   <bpmndi:BPMNEdge id="Edge_Out_Stock" bpmnElement="Flow_Out_Stock"><di:waypoint x="360" y="225" /><di:waypoint x="410" y="318" /><di:waypoint x="470" y="318" /></bpmndi:BPMNEdge>
   <bpmndi:BPMNEdge id="Edge_Ship_Complete" bpmnElement="Flow_Ship_Complete"><di:waypoint x="600" y="108" /><di:waypoint x="650" y="108" /><di:waypoint x="708" y="190" /></bpmndi:BPMNEdge>
   <bpmndi:BPMNEdge id="Edge_Notify_Complete" bpmnElement="Flow_Notify_Complete"><di:waypoint x="600" y="318" /><di:waypoint x="650" y="318" /><di:waypoint x="708" y="226" /></bpmndi:BPMNEdge>
  </bpmndi:BPMNPlane>
 </bpmndi:BPMNDiagram>
</bpmn:definitions>
''',
        encoding="utf-8",
    )


def build_dmn(path: Path) -> None:
    """Write a DMN 1.5 decision table with local input requirements."""
    path.write_text(
        '''<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="https://www.omg.org/spec/DMN/20230324/MODEL/"
 id="Definitions_Loan" name="Loan review" namespace="https://example.invalid/loan">
  <inputData id="ApplicantAge" name="Applicant age">
    <variable id="ApplicantAgeVariable" name="Applicant age" typeRef="number" />
  </inputData>
  <inputData id="CreditScore" name="Credit score">
    <variable id="CreditScoreVariable" name="Credit score" typeRef="number" />
  </inputData>
  <decision id="LoanDecision" name="Loan approval">
    <informationRequirement><requiredInput href="#ApplicantAge" /></informationRequirement>
    <informationRequirement><requiredInput href="#CreditScore" /></informationRequirement>
    <decisionTable id="LoanRules" hitPolicy="FIRST">
      <input id="AgeInput" label="Age">
        <inputExpression id="AgeExpression" typeRef="number"><text>Applicant age</text></inputExpression>
      </input>
      <input id="ScoreInput" label="Score">
        <inputExpression id="ScoreExpression" typeRef="number"><text>Credit score</text></inputExpression>
      </input>
      <output id="ApprovalOutput" name="Approval" label="Approval" typeRef="string" />
      <output id="ReasonOutput" name="Reason" label="Reason" typeRef="string" />
      <rule id="Rule_Minor">
        <inputEntry><text>&lt; 18</text></inputEntry>
        <inputEntry><text>-</text></inputEntry>
        <outputEntry><text>\"declined\"</text></outputEntry>
        <outputEntry><text>\"Applicant is a minor\"</text></outputEntry>
      </rule>
      <rule id="Rule_Approve">
        <inputEntry><text>&gt;= 18</text></inputEntry>
        <inputEntry><text>&gt;= 700</text></inputEntry>
        <outputEntry><text>\"approved\"</text></outputEntry>
        <outputEntry><text>\"Score meets threshold\"</text></outputEntry>
      </rule>
      <rule id="Rule_Review">
        <inputEntry><text>&gt;= 18</text></inputEntry>
        <inputEntry><text>&lt; 700</text></inputEntry>
        <outputEntry><text>\"manual review\"</text></outputEntry>
        <outputEntry><text>\"Additional review required\"</text></outputEntry>
      </rule>
    </decisionTable>
  </decision>
</definitions>
''',
        encoding="utf-8",
    )


def build_cmmn(path: Path) -> None:
    """Write a CMMN 1.1 claim case with explicit CMMNDI geometry."""
    path.write_text(
        '''<?xml version="1.0" encoding="UTF-8"?>
<definitions xmlns="http://www.omg.org/spec/CMMN/20151109/MODEL"
 xmlns:cmmndi="http://www.omg.org/spec/CMMN/20151109/CMMNDI"
 xmlns:dc="http://www.omg.org/spec/CMMN/20151109/DC"
 xmlns:di="http://www.omg.org/spec/CMMN/20151109/DI"
 targetNamespace="https://example.invalid/claims">
  <case id="ClaimCase" name="Claim review">
    <casePlanModel id="ClaimPlan" name="Claims file">
      <planItem id="ReviewPlan" name="Review documents" definitionRef="ReviewTask" />
      <planItem id="DecisionPlan" name="Decide eligibility" definitionRef="DecisionTask">
        <entryCriterion id="DecisionEntry" sentryRef="ReviewComplete" />
      </planItem>
      <planItem id="NotifyPlan" name="Notify case owner" definitionRef="NotifyTask" />
      <sentry id="ReviewComplete" name="Review complete">
        <planItemOnPart id="ReviewOnPart" sourceRef="ReviewPlan">
          <standardEvent>complete</standardEvent>
        </planItemOnPart>
      </sentry>
      <sentry id="DecisionComplete" name="Decision complete">
        <planItemOnPart id="DecisionOnPart" sourceRef="DecisionPlan">
          <standardEvent>complete</standardEvent>
        </planItemOnPart>
      </sentry>
      <humanTask id="ReviewTask" name="Human review" />
      <decisionTask id="DecisionTask" name="Eligibility decision" />
      <humanTask id="NotifyTask" name="Send notification" />
      <milestone id="ClaimClosed" name="Claim closed" />
    </casePlanModel>
  </case>
  <cmmndi:CMMNDI>
    <cmmndi:CMMNDiagram id="ClaimDiagram" name="Claim review" cmmnElementRef="ClaimPlan">
      <cmmndi:CMMNShape id="Shape_Case" cmmnElementRef="ClaimPlan"><dc:Bounds x="30" y="30" width="620" height="280" /></cmmndi:CMMNShape>
      <cmmndi:CMMNShape id="Shape_Review" cmmnElementRef="ReviewPlan"><dc:Bounds x="80" y="120" width="140" height="70" /></cmmndi:CMMNShape>
      <cmmndi:CMMNShape id="Shape_Decision" cmmnElementRef="DecisionPlan"><dc:Bounds x="275" y="120" width="150" height="70" /></cmmndi:CMMNShape>
      <cmmndi:CMMNShape id="Shape_Notify" cmmnElementRef="NotifyPlan"><dc:Bounds x="490" y="120" width="130" height="70" /></cmmndi:CMMNShape>
      <cmmndi:CMMNEdge id="Edge_Review_Decision" cmmnElementRef="ReviewOnPart" sourceCMMNElementRef="ReviewPlan" targetCMMNElementRef="DecisionPlan">
        <di:waypoint x="220" y="155" /><di:waypoint x="275" y="155" />
      </cmmndi:CMMNEdge>
      <cmmndi:CMMNEdge id="Edge_Decision_Notify" cmmnElementRef="DecisionOnPart" sourceCMMNElementRef="DecisionPlan" targetCMMNElementRef="NotifyPlan">
        <di:waypoint x="425" y="155" /><di:waypoint x="490" y="155" />
      </cmmndi:CMMNEdge>
    </cmmndi:CMMNDiagram>
  </cmmndi:CMMNDI>
</definitions>
''',
        encoding="utf-8",
    )


def build_reqif(path: Path) -> None:
    """Write a ReqIF 1.0.1 systems-engineering requirements exchange."""
    path.write_text(
        '''<?xml version="1.0" encoding="UTF-8"?>
<REQ-IF xmlns="http://www.omg.org/spec/ReqIF/20110401/reqif.xsd" xml:lang="en">
  <THE-HEADER>
    <REQ-IF-HEADER IDENTIFIER="HEADER-VEHICLE">
      <TITLE>Vehicle braking requirements</TITLE>
      <CREATION-TIME>2026-09-15T09:00:00Z</CREATION-TIME>
      <REQ-IF-VERSION>1.0.1</REQ-IF-VERSION>
      <REQ-IF-TOOL-ID>document-svg sample generator</REQ-IF-TOOL-ID>
    </REQ-IF-HEADER>
  </THE-HEADER>
  <CORE-CONTENT>
    <REQ-IF-CONTENT>
      <SPEC-TYPES>
        <SPEC-OBJECT-TYPE IDENTIFIER="TYPE-FUNCTIONAL" LONG-NAME="Functional requirement" DESC="A requirement describing system behavior.">
          <SPEC-ATTRIBUTES>
            <ATTRIBUTE-DEFINITION-STRING IDENTIFIER="ATTR-TEXT" LONG-NAME="Requirement text" />
            <ATTRIBUTE-DEFINITION-ENUMERATION IDENTIFIER="ATTR-STATUS" LONG-NAME="Status" />
          </SPEC-ATTRIBUTES>
        </SPEC-OBJECT-TYPE>
        <SPEC-RELATION-TYPE IDENTIFIER="TYPE-DERIVE" LONG-NAME="Derives" />
      </SPEC-TYPES>
      <SPEC-OBJECTS>
        <SPEC-OBJECT IDENTIFIER="REQ-BRAKE-001" LONG-NAME="Stopping distance" DESC="The service brake shall stop the vehicle within the target distance.">
          <TYPE><SPEC-OBJECT-TYPE-REF>TYPE-FUNCTIONAL</SPEC-OBJECT-TYPE-REF></TYPE>
          <VALUES>
            <ATTRIBUTE-VALUE-STRING THE-VALUE="Stop within 40 m at 100 km/h"><DEFINITION><ATTRIBUTE-DEFINITION-STRING-REF>ATTR-TEXT</ATTRIBUTE-DEFINITION-STRING-REF></DEFINITION></ATTRIBUTE-VALUE-STRING>
            <ATTRIBUTE-VALUE-ENUMERATION><DEFINITION><ATTRIBUTE-DEFINITION-ENUMERATION-REF>ATTR-STATUS</ATTRIBUTE-DEFINITION-ENUMERATION-REF></DEFINITION><VALUES><ENUM-VALUE-REF>Approved</ENUM-VALUE-REF></VALUES></ATTRIBUTE-VALUE-ENUMERATION>
          </VALUES>
        </SPEC-OBJECT>
        <SPEC-OBJECT IDENTIFIER="REQ-BRAKE-002" LONG-NAME="Wear warning" DESC="The system shall warn the driver before the brake pad wear limit.">
          <TYPE><SPEC-OBJECT-TYPE-REF>TYPE-FUNCTIONAL</SPEC-OBJECT-TYPE-REF></TYPE>
          <VALUES><ATTRIBUTE-VALUE-STRING THE-VALUE="Warn before pad wear limit"><DEFINITION><ATTRIBUTE-DEFINITION-STRING-REF>ATTR-TEXT</ATTRIBUTE-DEFINITION-STRING-REF></DEFINITION></ATTRIBUTE-VALUE-STRING></VALUES>
        </SPEC-OBJECT>
        <SPEC-OBJECT IDENTIFIER="REQ-BRAKE-003" LONG-NAME="Brake diagnostics" DESC="Diagnostic information shall be available to service tools.">
          <TYPE><SPEC-OBJECT-TYPE-REF>TYPE-FUNCTIONAL</SPEC-OBJECT-TYPE-REF></TYPE>
          <VALUES><ATTRIBUTE-VALUE-XHTML><DEFINITION><ATTRIBUTE-DEFINITION-STRING-REF>ATTR-TEXT</ATTRIBUTE-DEFINITION-STRING-REF></DEFINITION><THE-VALUE><xhtml:div xmlns:xhtml="http://www.w3.org/1999/xhtml">Provide <xhtml:b>diagnostic</xhtml:b> status.</xhtml:div></THE-VALUE></ATTRIBUTE-VALUE-XHTML></VALUES>
        </SPEC-OBJECT>
      </SPEC-OBJECTS>
      <SPEC-RELATIONS>
        <SPEC-RELATION IDENTIFIER="REL-BRAKE-001"><SOURCE><SPEC-OBJECT-REF>REQ-BRAKE-001</SPEC-OBJECT-REF></SOURCE><TARGET><SPEC-OBJECT-REF>REQ-BRAKE-002</SPEC-OBJECT-REF></TARGET><TYPE><SPEC-RELATION-TYPE-REF>TYPE-DERIVE</SPEC-RELATION-TYPE-REF></TYPE></SPEC-RELATION>
      </SPEC-RELATIONS>
      <SPECIFICATIONS>
        <SPECIFICATION IDENTIFIER="SPEC-BRAKE" LONG-NAME="Brake system requirements" DESC="Requirements for the vehicle braking system.">
          <CHILDREN>
            <SPEC-HIERARCHY IDENTIFIER="H-BRAKE-001"><OBJECT><SPEC-OBJECT-REF>REQ-BRAKE-001</SPEC-OBJECT-REF></OBJECT><CHILDREN><SPEC-HIERARCHY IDENTIFIER="H-BRAKE-002"><OBJECT><SPEC-OBJECT-REF>REQ-BRAKE-002</SPEC-OBJECT-REF></OBJECT></SPEC-HIERARCHY><SPEC-HIERARCHY IDENTIFIER="H-BRAKE-003"><OBJECT><SPEC-OBJECT-REF>REQ-BRAKE-003</SPEC-OBJECT-REF></OBJECT></SPEC-HIERARCHY></CHILDREN></SPEC-HIERARCHY>
          </CHILDREN>
          <TYPE><SPECIFICATION-TYPE-REF>TYPE-FUNCTIONAL</SPECIFICATION-TYPE-REF></TYPE>
        </SPECIFICATION>
      </SPECIFICATIONS>
    </REQ-IF-CONTENT>
  </CORE-CONTENT>
</REQ-IF>
''',
        encoding="utf-8",
    )


def build_xmi(path: Path) -> None:
    """Write a small UML XMI model with classes, attributes and an association."""
    path.write_text(
        '''<?xml version="1.0" encoding="UTF-8"?>
<xmi:XMI xmi:version="2.1"
 xmlns:xmi="http://schema.omg.org/spec/XMI/2.1"
 xmlns:uml="http://www.eclipse.org/uml2/5.0.0/UML">
  <uml:Model xmi:type="uml:Model" xmi:id="model-vehicle" name="Vehicle control">
    <packagedElement xmi:type="uml:Package" xmi:id="pkg-brakes" name="Braking">
      <packagedElement xmi:type="uml:Class" xmi:id="class-controller" name="BrakeController" visibility="public">
        <ownedAttribute xmi:type="uml:Property" xmi:id="attr-pressure" name="pressure" type="double" visibility="private" />
        <ownedOperation xmi:type="uml:Operation" xmi:id="op-apply" name="applyBrake" />
      </packagedElement>
      <packagedElement xmi:type="uml:Class" xmi:id="class-sensor" name="WheelSensor">
        <ownedAttribute xmi:type="uml:Property" xmi:id="attr-speed" name="wheelSpeed" type="double" />
      </packagedElement>
      <packagedElement xmi:type="uml:Association" xmi:id="assoc-measures" name="measures" memberEnd="attr-pressure attr-speed" />
    </packagedElement>
  </uml:Model>
</xmi:XMI>
''',
        encoding="utf-8",
    )


def build_project_xml(path: Path) -> None:
    """Reuse the synthetic bounded Microsoft Project schedule fixture."""

    path.write_bytes((REPOSITORY / "tests/fixtures/sample.project.xml").read_bytes())


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------

BUILDERS = {
    "sample.pdf": build_pdf,
    "sample.docx": build_docx,
    "sample.pptx": build_pptx,
    "sample.xlsx": build_xlsx,
    "sample.drawio": build_drawio,
    "sample.dxf": build_dxf,
    "sample.gbr": build_gerber,
    "sample.plt": build_hpgl,
    "sample.nc": build_gcode,
    "sample.drl": build_excellon,
    "sample.stl": build_stl,
    "sample.vtk": build_vtk,
    "sample.msh": build_msh,
    "sample.su2": build_su2,
    "sample.step": build_step,
    "sample.ifc": build_ifc,
    "sample.ifczip": build_ifczip,
    "sample.obj": build_obj,
    "sample.dot": build_dot,
    "sample.mmd": build_mermaid,
    "sample.tex": build_tex,
    "sample.chart": build_chart,
    "sample.md": build_markdown,
    "sample.qr": build_qr,
    "sample.png": build_raster,
    "sample-elevation.asc": build_ascii_grid,
    "sample-parcels.dbf": build_dbase,
    "sample.gpkg": build_geopackage,
    "sample.geojsons": build_geojson_sequence,
    "sample.topojson": build_topojson,
    "sample.jsons": build_json_text_sequence,
    "sample.toml": build_toml,
    "sample.yaml": build_yaml,
    "sample-config.xml": build_generic_xml,
    "sample.properties": build_properties,
    "sample.bpmn": build_bpmn,
    "sample.dmn": build_dmn,
    "sample.cmmn": build_cmmn,
    "sample.reqif": build_reqif,
    "sample.xmi": build_xmi,
    "sample.project.xml": build_project_xml,
    "sample-stencils.xml": build_stencils,
}


def find_docsvg(explicit: str | None) -> str:
    if explicit:
        return explicit
    for candidate in (
        REPOSITORY / "target" / "release" / "docsvg",
        REPOSITORY / "target" / "debug" / "docsvg",
    ):
        if candidate.is_file():
            return str(candidate)
    found = shutil.which("docsvg")
    if found:
        return found
    raise SystemExit(
        "docsvg was not found. Build it with `cargo build --release` or pass --docsvg PATH."
    )


def relativize_report(report: Path) -> None:
    """Rewrite the machine-specific parts of the report docsvg records.

    A committed report should not carry whoever regenerated it home directory,
    nor how fast their machine was: `elapsed_ms` otherwise makes every sample
    regeneration show up as a change to every report.
    """

    if not report.is_file():
        return
    import json

    data = json.loads(report.read_text())
    for key in ("source", "output_directory"):
        value = data.get(key)
        if not isinstance(value, str):
            continue
        try:
            data[key] = str(Path(value).resolve().relative_to(REPOSITORY))
        except ValueError:
            pass
    if "elapsed_ms" in data:
        data["elapsed_ms"] = 0
    report.write_text(json.dumps(data, indent=2) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docsvg", help="path to the docsvg binary")
    parser.add_argument(
        "--sources-only",
        action="store_true",
        help="write samples/source but do not convert them",
    )
    arguments = parser.parse_args()

    SOURCE.mkdir(parents=True, exist_ok=True)
    for name, builder in BUILDERS.items():
        builder(SOURCE / name)
        print(f"wrote samples/source/{name}")

    provenance = {
        "generator": "scripts/make_samples.py",
        "license": "MIT OR Apache-2.0",
        "sha256": {f"source/{name}": hashlib.sha256((SOURCE / name).read_bytes()).hexdigest()
                   for name in BUILDERS},
    }
    (SAMPLES / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")

    if arguments.sources_only:
        return 0

    binary = find_docsvg(arguments.docsvg)
    if RENDERED.exists():
        shutil.rmtree(RENDERED)
    RENDERED.mkdir(parents=True)

    failed = False
    for name in BUILDERS:
        # The shape library is an input to a conversion, not a document.
        if name == "sample-stencils.xml":
            continue
        suffix = Path(name).suffix.lstrip(".")
        sample_key = "generic-xml" if name == "sample-config.xml" else suffix
        destination = RENDERED / sample_key
        arguments = [binary, str(SOURCE / name), "--output", str(destination)]
        if name == "sample.drawio":
            arguments += ["--stencils", str(SOURCE / "sample-stencils.xml")]
        result = subprocess.run(arguments, capture_output=True, text=True)
        if result.returncode != 0:
            failed = True
            print(f"FAILED {name}: {result.stderr.strip()}", file=sys.stderr)
            continue
        relativize_report(destination / "conversion.json")
        pages = sorted(destination.glob("page-*.svg"))
        print(f"converted {name} -> samples/svg/{sample_key}/ ({len(pages)} page(s))")

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
