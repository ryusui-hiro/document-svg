#!/usr/bin/env python3
"""Generate a deterministic, synthetic DICOM Encapsulated PDF test fixture."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "fixtures"
OUTPUT = FIXTURES / "sample_encapsulated_pdf.dcm"
MANIFEST = FIXTURES / "dicom_pdf.provenance.json"

SOP_CLASS_UID = "1.2.840.10008.5.1.4.1.1.104.1"
TRANSFER_SYNTAX_UID = "1.2.840.10008.1.2.1"


def pad(value: bytes, byte: bytes) -> bytes:
    return value if len(value) % 2 == 0 else value + byte


def element(group: int, number: int, vr: bytes, value: bytes) -> bytes:
    header = group.to_bytes(2, "little") + number.to_bytes(2, "little") + vr
    if vr in {b"OB", b"OD", b"OF", b"OL", b"OV", b"OW", b"SQ", b"UC", b"UR", b"UT", b"UN"}:
        return header + b"\0\0" + len(value).to_bytes(4, "little") + value
    if len(value) > 0xFFFF:
        raise ValueError("short-VR value exceeds 65535 bytes")
    return header + len(value).to_bytes(2, "little") + value


def synthetic_pdf() -> bytes:
    content = b"BT /F1 24 Tf 72 720 Td (Synthetic radiology report) Tj ET\n"
    bodies = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        b"<< /Length " + str(len(content)).encode("ascii") + b" >>\nstream\n" + content + b"endstream",
    ]
    output = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for index, body in enumerate(bodies, 1):
        offsets.append(len(output))
        output.extend(f"{index} 0 obj\n".encode("ascii"))
        output.extend(body)
        output.extend(b"\nendobj\n")
    xref = len(output)
    output.extend(f"xref\n0 {len(offsets)}\n".encode("ascii"))
    output.extend(b"0000000000 65535 f \n")
    for offset in offsets[1:]:
        output.extend(f"{offset:010d} 00000 n \n".encode("ascii"))
    output.extend(
        f"trailer\n<< /Size {len(offsets)} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode("ascii")
    )
    return bytes(output)


def dicom_encapsulated_pdf(pdf: bytes) -> bytes:
    meta_items = b"".join(
        [
            element(0x0002, 0x0001, b"OB", b"\0\1"),
            element(0x0002, 0x0002, b"UI", pad(SOP_CLASS_UID.encode("ascii"), b"\0")),
            element(0x0002, 0x0003, b"UI", pad(b"2.25.300000000000000000000000000000000001", b"\0")),
            element(0x0002, 0x0010, b"UI", pad(TRANSFER_SYNTAX_UID.encode("ascii"), b"\0")),
            element(0x0002, 0x0012, b"UI", pad(b"2.25.300000000000000000000000000000000002", b"\0")),
        ]
    )
    file_meta = element(0x0002, 0x0000, b"UL", len(meta_items).to_bytes(4, "little")) + meta_items
    document = pad(pdf, b"\0")
    dataset = b"".join(
        [
            element(0x0008, 0x0016, b"UI", pad(SOP_CLASS_UID.encode("ascii"), b"\0")),
            element(0x0008, 0x0018, b"UI", pad(b"2.25.300000000000000000000000000000000003", b"\0")),
            element(0x0010, 0x0010, b"PN", pad(b"Synthetic^Patient", b" ")),
            element(0x0042, 0x0010, b"ST", pad(b"Synthetic Encapsulated PDF", b" ")),
            element(0x0042, 0x0011, b"OB", document),
            element(0x0042, 0x0012, b"LO", pad(b"application/pdf", b" ")),
            element(0x0042, 0x0015, b"UL", len(pdf).to_bytes(4, "little")),
        ]
    )
    return bytes(128) + b"DICM" + file_meta + dataset


def main() -> None:
    pdf = synthetic_pdf()
    fixture = dicom_encapsulated_pdf(pdf)
    FIXTURES.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_bytes(fixture)
    manifest = {
        "generator": "scripts/generate-dicom-pdf-fixture.py",
        "source": "Small synthetic PDF with Helvetica text; DICOM patient name is synthetic and is used only to verify metadata exclusion.",
        "sha256": {"tests/fixtures/sample_encapsulated_pdf.dcm": hashlib.sha256(fixture).hexdigest()},
        "sizes": {"tests/fixtures/sample_encapsulated_pdf.dcm": len(fixture)},
    }
    MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"Wrote {OUTPUT.relative_to(ROOT)} ({len(fixture)} bytes)")


if __name__ == "__main__":
    main()
