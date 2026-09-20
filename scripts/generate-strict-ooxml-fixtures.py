"""Regenerate small ISO/IEC 29500 Strict OOXML test packages from project samples."""

import hashlib
import json
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "samples" / "source"
FIXTURES = ROOT / "tests" / "fixtures"

NAMESPACE_MAP = {
    b"http://schemas.openxmlformats.org/wordprocessingml/2006/main": b"http://purl.oclc.org/ooxml/wordprocessingml/main",
    b"http://schemas.openxmlformats.org/wordprocessingml/2006/math": b"http://purl.oclc.org/ooxml/officeDocument/math",
    b"http://schemas.openxmlformats.org/drawingml/2006/main": b"http://purl.oclc.org/ooxml/drawingml/main",
    b"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing": b"http://purl.oclc.org/ooxml/drawingml/wordprocessingDrawing",
    b"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing": b"http://purl.oclc.org/ooxml/drawingml/spreadsheetDrawing",
    b"http://schemas.openxmlformats.org/drawingml/2006/chart": b"http://purl.oclc.org/ooxml/drawingml/chart",
    b"http://schemas.openxmlformats.org/presentationml/2006/main": b"http://purl.oclc.org/ooxml/presentationml/main",
    b"http://schemas.openxmlformats.org/spreadsheetml/2006/main": b"http://purl.oclc.org/ooxml/spreadsheetml/main",
    b"http://schemas.openxmlformats.org/officeDocument/2006/relationships": b"http://purl.oclc.org/ooxml/officeDocument/relationships",
}


def strict_copy(source_name: str, target_name: str) -> None:
    with ZipFile(SOURCE / source_name) as original, ZipFile(
        FIXTURES / target_name, "w", ZIP_DEFLATED
    ) as strict:
        for item in original.infolist():
            data = original.read(item.filename)
            if item.filename.endswith((".xml", ".rels")):
                for transitional, strict_uri in NAMESPACE_MAP.items():
                    data = data.replace(transitional, strict_uri)
            strict.writestr(item, data)


strict_copy("sample.docx", "sample_strict.docx")
strict_copy("sample.xlsx", "sample_strict.xlsx")
strict_copy("sample.pptx", "sample_strict.pptx")

fixture_names = ("sample_strict.docx", "sample_strict.xlsx", "sample_strict.pptx")
manifest = {
    "generator": "scripts/generate-strict-ooxml-fixtures.py",
    "license": "MIT OR Apache-2.0",
    "sha256": {
        f"tests/fixtures/{name}": hashlib.sha256((FIXTURES / name).read_bytes()).hexdigest()
        for name in fixture_names
    },
    "source_files": [
        "samples/source/sample.docx",
        "samples/source/sample.xlsx",
        "samples/source/sample.pptx",
    ],
}
(FIXTURES / "strict_ooxml.provenance.json").write_text(
    json.dumps(manifest, indent=2) + "\n", encoding="utf-8"
)
