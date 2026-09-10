from pathlib import Path

import pytest

from document_svg import convert, reverse


def write_minimal_pdf(path: Path) -> None:
    objects = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
        "/Resources << >> /Contents 4 0 R >>",
        "<< /Length 0 >>\nstream\n\nendstream",
    ]
    pdf = bytearray(b"%PDF-1.4\n")
    offsets: list[int] = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(pdf))
        pdf.extend(f"{number} 0 obj\n{body}\nendobj\n".encode("ascii"))
    xref_offset = len(pdf)
    pdf.extend(f"xref\n0 {len(objects) + 1}\n".encode("ascii"))
    pdf.extend(b"0000000000 65535 f \n")
    for offset in offsets:
        pdf.extend(f"{offset:010} 00000 n \n".encode("ascii"))
    pdf.extend(
        (
            f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n"
            f"startxref\n{xref_offset}\n%%EOF\n"
        ).encode("ascii")
    )
    path.write_bytes(pdf)


def test_converts_pdf_and_returns_typed_report_shape(tmp_path: Path) -> None:
    source = tmp_path / "input.pdf"
    output = tmp_path / "out"
    write_minimal_pdf(source)

    report = convert(source, output, jobs=1)

    assert report["source_format"] == "pdf"
    assert report["page_count"] == 1
    assert (output / report["pages"][0]["svg"]).is_file()


def test_missing_input_is_reported_as_os_error(tmp_path: Path) -> None:
    with pytest.raises(OSError, match="I/O error"):
        convert(tmp_path / "missing.pdf", tmp_path / "out")


def test_unsupported_extension_is_reported_as_value_error(tmp_path: Path) -> None:
    source = tmp_path / "input.txt"
    source.write_text("not a document", encoding="utf-8")

    with pytest.raises(ValueError, match="unsupported input"):
        convert(source, tmp_path / "out")


def test_packages_svg_as_pptx(tmp_path: Path) -> None:
    source = tmp_path / "page.svg"
    output = tmp_path / "page.pptx"
    source.write_text(
        '<svg xmlns="http://www.w3.org/2000/svg" width="200pt" '
        'height="100pt"><text x="10" y="20">Hello</text></svg>',
        encoding="utf-8",
    )

    report = reverse(source, output)

    assert report["output_format"] == "pptx"
    assert report["page_count"] == 1
    assert output.is_file()


DRAWIO_DIAGRAM = (
    '<mxfile><diagram id="p1" name="Flow"><mxGraphModel><root>'
    '<mxCell id="0"/><mxCell id="1" parent="0"/>'
    '<mxCell id="a" value="Start" style="ellipse;whiteSpace=wrap;html=1;" vertex="1" parent="1">'
    '<mxGeometry x="20" y="20" width="120" height="60" as="geometry"/></mxCell>'
    "</root></mxGraphModel></diagram></mxfile>"
)


def test_converts_drawio_and_keeps_the_source_when_asked(tmp_path: Path) -> None:
    source = tmp_path / "diagram.drawio"
    source.write_text(DRAWIO_DIAGRAM, encoding="utf-8")

    plain = convert(source, tmp_path / "plain")
    assert plain["source_format"] == "drawio"
    assert plain["page_count"] == 1
    assert "content=" not in (tmp_path / "plain" / plain["pages"][0]["svg"]).read_text(
        encoding="utf-8"
    )

    embedded = convert(source, tmp_path / "embedded", embed_drawio_source=True)
    page = (tmp_path / "embedded" / embedded["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "content=" in page

    # The embedded copy is what makes the round trip give back a diagram.
    restored = tmp_path / "restored.drawio"
    report = reverse(tmp_path / "embedded", restored)
    assert report["output_format"] == "drawio"
    assert "restored" in report["warnings"][0]
    assert '<diagram id="p1" name="Flow">' in restored.read_text(encoding="utf-8")


def test_draws_a_shape_from_a_stencil_library(tmp_path: Path) -> None:
    stencils = tmp_path / "stencils"
    stencils.mkdir()
    (stencils / "demo.xml").write_text(
        '<shapes name="mxgraph.demo"><shape name="Badge" h="10" w="10" aspect="fixed">'
        '<connections/><background><ellipse x="0" y="0" w="10" h="10"/></background>'
        "<foreground><fillstroke/></foreground></shape></shapes>",
        encoding="utf-8",
    )
    source = tmp_path / "badge.drawio"
    source.write_text(
        '<mxfile><diagram name="Badge"><mxGraphModel><root>'
        '<mxCell id="0"/><mxCell id="1" parent="0"/>'
        '<mxCell id="b" style="shape=mxgraph.demo.badge;html=1;" vertex="1" parent="1">'
        '<mxGeometry x="0" y="0" width="40" height="40" as="geometry"/></mxCell>'
        "</root></mxGraphModel></diagram></mxfile>",
        encoding="utf-8",
    )

    without = convert(source, tmp_path / "without")
    assert any("mxgraph.demo.badge" in warning for warning in without["warnings"])

    with_library = convert(source, tmp_path / "with", stencil_paths=[str(stencils)])
    assert with_library["warnings"] == []
    page = (tmp_path / "with" / with_library["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "drawio-stencil" in page
