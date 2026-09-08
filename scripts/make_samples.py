#!/usr/bin/env python3
"""Write the sample documents in `samples/` and convert them with docsvg.

Every file this script produces is authored here, so the samples carry the
repository's own license instead of a third party's. Run it after changing a
renderer to refresh what `samples/svg/` shows.

    python3 scripts/make_samples.py

Python 3.10+, standard library only.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import struct
import subprocess
import sys
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
# Driver
# --------------------------------------------------------------------------

BUILDERS = {
    "sample.pdf": build_pdf,
    "sample.docx": build_docx,
    "sample.pptx": build_pptx,
    "sample.xlsx": build_xlsx,
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
    """Rewrite the absolute paths docsvg records so the samples can be shared.

    A committed report should not carry whoever regenerated it home directory.
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
        suffix = Path(name).suffix.lstrip(".")
        destination = RENDERED / suffix
        result = subprocess.run(
            [binary, str(SOURCE / name), "--output", str(destination)],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            failed = True
            print(f"FAILED {name}: {result.stderr.strip()}", file=sys.stderr)
            continue
        relativize_report(destination / "conversion.json")
        pages = sorted(destination.glob("page-*.svg"))
        print(f"converted {name} -> samples/svg/{suffix}/ ({len(pages)} page(s))")

    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
