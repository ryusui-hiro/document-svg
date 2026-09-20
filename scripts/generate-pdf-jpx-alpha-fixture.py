#!/usr/bin/env python3
"""Generate a small PDF fixture that wraps the synthetic JPX alpha image."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
import zlib

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "fixtures"
SOURCE = FIXTURES / "sample_jpeg2000_rgba.jp2"
SOURCE_RGB = FIXTURES / "sample_jpeg2000_rgb.jp2"
SOURCE_PREBLENDED = FIXTURES / "sample_jpeg2000_rgba_preblended.jp2"
OUTPUT = FIXTURES / "sample_jpx_alpha.pdf"
OUTPUT_SOFT_MASK = FIXTURES / "sample_jpx_soft_mask.pdf"
OUTPUT_PREBLENDED = FIXTURES / "sample_jpx_smask_in_data_2.pdf"
MANIFEST = FIXTURES / "pdf_images.provenance.json"


def build_pdf(jpx: bytes) -> bytes:
    content = b"q 120 0 0 120 40 40 cm /JPX Do Q"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
        b"/Resources << /XObject << /JPX 5 0 R >> >> /Contents 4 0 R >>",
        b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"\nendstream",
        b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 "
        b"/ColorSpace /DeviceRGB /BitsPerComponent 8 /SMaskInData 1 "
        b"/Filter /JPXDecode /Length " + str(len(jpx)).encode() + b" >>\nstream\n"
        + jpx + b"\nendstream",
    ]
    pdf = bytearray(b"%PDF-1.7\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf.extend(f"{number} 0 obj\n".encode())
        pdf.extend(body)
        pdf.extend(b"\nendobj\n")
    xref_offset = len(pdf)
    pdf.extend(f"xref\n0 {len(objects) + 1}\n".encode())
    pdf.extend(b"0000000000 65535 f \n")
    for offset in offsets:
        pdf.extend(f"{offset:010} 00000 n \n".encode())
    pdf.extend(
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{xref_offset}\n%%EOF\n".encode()
    )
    return bytes(pdf)


def build_external_soft_mask_pdf(jpx: bytes) -> bytes:
    content = b"q 120 0 0 120 40 40 cm /JPX Do Q"
    mask = zlib.compress(bytes([0, 64, 128, 255]))
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
        b"/Resources << /XObject << /JPX 5 0 R >> >> /Contents 4 0 R >>",
        b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"\nendstream",
        b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 "
        b"/ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /JPXDecode "
        b"/SMask 6 0 R /Length " + str(len(jpx)).encode() + b" >>\nstream\n"
        + jpx + b"\nendstream",
        b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 "
        b"/ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode "
        b"/Length " + str(len(mask)).encode() + b" >>\nstream\n"
        + mask + b"\nendstream",
    ]
    pdf = bytearray(b"%PDF-1.7\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf.extend(f"{number} 0 obj\n".encode())
        pdf.extend(body)
        pdf.extend(b"\nendobj\n")
    xref_offset = len(pdf)
    pdf.extend(f"xref\n0 {len(objects) + 1}\n".encode())
    pdf.extend(b"0000000000 65535 f \n")
    for offset in offsets:
        pdf.extend(f"{offset:010} 00000 n \n".encode())
    pdf.extend(
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{xref_offset}\n%%EOF\n".encode()
    )
    return bytes(pdf)


def build_preblended_jpx_pdf(jpx: bytes) -> bytes:
    content = b"q 120 0 0 120 40 40 cm /JPX Do Q"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
        b"/Resources << /XObject << /JPX 5 0 R >> >> /Contents 4 0 R >>",
        b"<< /Length " + str(len(content)).encode() + b" >>\nstream\n" + content + b"\nendstream",
        b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 "
        b"/ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /JPXDecode "
        b"/SMaskInData 2 /Matte [0 0 1] /Length " + str(len(jpx)).encode()
        + b" >>\nstream\n" + jpx + b"\nendstream",
    ]
    pdf = bytearray(b"%PDF-1.7\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf.extend(f"{number} 0 obj\n".encode())
        pdf.extend(body)
        pdf.extend(b"\nendobj\n")
    xref_offset = len(pdf)
    pdf.extend(f"xref\n0 {len(objects) + 1}\n".encode())
    pdf.extend(b"0000000000 65535 f \n")
    for offset in offsets:
        pdf.extend(f"{offset:010} 00000 n \n".encode())
    pdf.extend(
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{xref_offset}\n%%EOF\n".encode()
    )
    return bytes(pdf)


def main() -> None:
    source_bytes = SOURCE.read_bytes()
    output_bytes = build_pdf(source_bytes)
    OUTPUT.write_bytes(output_bytes)
    soft_mask_source = SOURCE_RGB.read_bytes()
    soft_mask_output = build_external_soft_mask_pdf(soft_mask_source)
    OUTPUT_SOFT_MASK.write_bytes(soft_mask_output)
    preblended_source = SOURCE_PREBLENDED.read_bytes()
    preblended_output = build_preblended_jpx_pdf(preblended_source)
    OUTPUT_PREBLENDED.write_bytes(preblended_output)
    provenance = {
        "generator": "scripts/generate-pdf-jpx-alpha-fixture.py",
        "description": "Synthetic one-page PDFs wrapping self-authored 4x4 JPEG 2000 fixtures with embedded, external, and preblended soft masks.",
        "source_files": [
            "tests/fixtures/sample_jpeg2000_rgba.jp2",
            "tests/fixtures/sample_jpeg2000_rgb.jp2",
            "tests/fixtures/sample_jpeg2000_rgba_preblended.jp2",
        ],
        "sha256": {
            "tests/fixtures/sample_jpx_alpha.pdf": hashlib.sha256(output_bytes).hexdigest(),
            "tests/fixtures/sample_jpx_soft_mask.pdf": hashlib.sha256(soft_mask_output).hexdigest(),
            "tests/fixtures/sample_jpx_smask_in_data_2.pdf": hashlib.sha256(preblended_output).hexdigest(),
        },
    }
    MANIFEST.write_text(json.dumps(provenance, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
