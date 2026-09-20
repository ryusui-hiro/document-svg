from pathlib import Path
import gzip
import zipfile

import pytest

from document_svg import convert, preview, reverse, transform


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


def write_minimal_tiff(path: Path) -> None:
    entries = [
        (256, 3, 2),  # ImageWidth
        (257, 3, 1),  # ImageLength
        (258, 3, 8),  # BitsPerSample
        (259, 3, 1),  # Compression: none
        (262, 3, 1),  # PhotometricInterpretation: black is zero
        (273, 4, 122),  # StripOffsets
        (277, 3, 1),  # SamplesPerPixel
        (278, 4, 1),  # RowsPerStrip
        (279, 4, 2),  # StripByteCounts
    ]
    data = bytearray(124)
    data[0:2] = b"II"
    data[2:4] = (42).to_bytes(2, "little")
    data[4:8] = (8).to_bytes(4, "little")
    data[8:10] = len(entries).to_bytes(2, "little")
    for index, (tag, kind, value) in enumerate(entries):
        offset = 10 + index * 12
        data[offset : offset + 2] = tag.to_bytes(2, "little")
        data[offset + 2 : offset + 4] = kind.to_bytes(2, "little")
        data[offset + 4 : offset + 8] = (1).to_bytes(4, "little")
        data[offset + 8 : offset + 10] = value.to_bytes(2, "little")
        if kind == 4:
            data[offset + 8 : offset + 12] = value.to_bytes(4, "little")
    data[10 + len(entries) * 12 : 14 + len(entries) * 12] = (0).to_bytes(4, "little")
    data[122:124] = bytes((32, 224))
    path.write_bytes(data)


def write_minimal_cbz(path: Path) -> None:
    import binascii
    import struct
    import zlib

    def chunk(kind: bytes, contents: bytes) -> bytes:
        return (
            struct.pack(">I", len(contents))
            + kind
            + contents
            + struct.pack(">I", binascii.crc32(kind + contents) & 0xFFFFFFFF)
        )

    def png(rgb: tuple[int, int, int]) -> bytes:
        header = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)
        data = bytes((0, *rgb))
        return (
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(data))
            + chunk(b"IEND", b"")
        )

    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("page10.png", png((0, 0, 255)))
        archive.writestr("page2.png", png((255, 0, 0)))


def write_minimal_vsdx(path: Path) -> None:
    entries = {
        "_rels/.rels": '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/visio/2010/relationships/visioDocument" Target="visio/document.xml"/></Relationships>',
        "visio/document.xml": '<VisioDocument xmlns="urn:visio"/>',
        "visio/_rels/document.xml.rels": '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdPages" Type="http://schemas.microsoft.com/visio/2010/relationships/pages" Target="pages/pages.xml"/></Relationships>',
        "visio/pages/pages.xml": '<Pages xmlns="urn:visio" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><Page ID="0" NameU="Main" r:id="rId1"/></Pages>',
        "visio/pages/_rels/pages.xml.rels": '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/visio/2010/relationships/page" Target="page1.xml"/></Relationships>',
        "visio/pages/page1.xml": '<PageContents xmlns="urn:visio"><PageSheet><Cell N="PageWidth" V="5"/><Cell N="PageHeight" V="4"/></PageSheet><Shapes><Shape ID="1" NameU="Process" Type="Shape"><Cell N="PinX" V="2.5"/><Cell N="PinY" V="2"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="FillForegnd" V="#FF0000"/><Section N="Geometry"><Row T="RelMoveTo"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row></Section><Text>Visio &amp; Python</Text></Shape></Shapes></PageContents>',
    }
    with zipfile.ZipFile(path, "w") as package:
        for name, contents in entries.items():
            package.writestr(name, contents)


def write_minimal_vdx(path: Path) -> None:
    path.write_text(
        '<?xml version="1.0"?><VisioDocument xmlns="urn:visio"><Pages>'
        '<Page ID="1" NameU="Legacy"><PageSheet><Cell N="PageWidth" V="4"/>'
        '<Cell N="PageHeight" V="3"/></PageSheet><Shapes><Shape ID="2" '
        'NameU="Process" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" '
        'V="1.5"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/>'
        '<Cell N="FillForegnd" V="#22AA44"/><Text>Legacy &amp; Python</Text>'
        '</Shape></Shapes></Page></Pages></VisioDocument>',
        encoding="utf-8",
    )


def write_minimal_odt(path: Path) -> None:
    with zipfile.ZipFile(path, "w") as package:
        package.writestr(
            "mimetype",
            "application/vnd.oasis.opendocument.text",
            compress_type=zipfile.ZIP_STORED,
        )
        package.writestr(
            "content.xml",
            '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
            'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">'
            '<office:body><office:text><text:h text:outline-level="1">ODT report</text:h>'
            '<text:p>OpenDocument Text from Python.</text:p></office:text></office:body></office:document-content>',
        )


def write_minimal_ods(path: Path) -> None:
    with zipfile.ZipFile(path, "w") as package:
        package.writestr(
            "mimetype",
            "application/vnd.oasis.opendocument.spreadsheet",
            compress_type=zipfile.ZIP_STORED,
        )
        package.writestr(
            "content.xml",
            '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
            'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
            'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">'
            '<office:body><office:spreadsheet><table:table table:name="Quarterly">'
            '<table:table-row><table:table-cell office:value-type="string"><text:p>Metric</text:p></table:table-cell>'
            '<table:table-cell office:value-type="string"><text:p>Value</text:p></table:table-cell></table:table-row>'
            '<table:table-row><table:table-cell office:value-type="string"><text:p>Requests</text:p></table:table-cell>'
            '<table:table-cell office:value-type="float" office:value="1200"><text:p>1200</text:p></table:table-cell>'
            '</table:table-row></table:table></office:spreadsheet></office:body></office:document-content>',
        )


def write_minimal_fodp(path: Path) -> None:
    path.write_text(
        '<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
        'xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" '
        'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
        'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
        'xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" '
        'xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0">'
        '<office:automatic-styles><style:page-layout style:name="wide">'
        '<style:page-layout-properties fo:page-width="20cm" fo:page-height="11.25cm"/>'
        '</style:page-layout></office:automatic-styles>'
        '<office:body><office:presentation><draw:page draw:name="Python slide">'
        '<draw:rect svg:x="1cm" svg:y="1cm" svg:width="12cm" svg:height="4cm">'
        '<text:p>OpenDocument presentation from Python.</text:p>'
        '</draw:rect></draw:page></office:presentation></office:body></office:document>',
        encoding="utf-8",
    )


def write_minimal_fodg(path: Path) -> None:
    path.write_text(
        '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
        'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
        'xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" '
        'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">'
        '<office:body><office:drawing><draw:page draw:name="Python drawing">'
        '<draw:rect svg:x="1cm" svg:y="1cm" svg:width="8cm" svg:height="4cm">'
        '<text:p>OpenDocument drawing from Python.</text:p>'
        '</draw:rect></draw:page></office:drawing></office:body></office:document-content>',
        encoding="utf-8",
    )


def write_minimal_rtf(path: Path) -> None:
    path.write_bytes(
        br"{\rtf1\ansi\ansicpg1252\uc1 R\'e9sum\'e9 \emdash  RTF from Python\par}"
    )


def write_minimal_rtf_cp932(path: Path) -> None:
    path.write_bytes(br"{\rtf1\ansi\ansicpg932\uc1 \'82\'a0\par}")


def test_converts_pdf_and_returns_typed_report_shape(tmp_path: Path) -> None:
    source = tmp_path / "input.pdf"
    output = tmp_path / "out"
    write_minimal_pdf(source)

    report = convert(source, output, jobs=1)

    assert report["source_format"] == "pdf"
    assert report["page_count"] == 1


def test_converts_pdf_jpx_embedded_alpha(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_jpx_alpha.pdf"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "pdf"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert "data:image/jp2;base64," not in svg
    assert "data:image/j2c;base64," not in svg


def test_converts_jpx_pdf_with_external_soft_mask(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_jpx_soft_mask.pdf"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "pdf"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert "data:image/jp2;base64," not in svg
    assert not report["warnings"]


def test_converts_jpx_pdf_smask_in_data_2_with_matte_unblending(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_jpx_smask_in_data_2.pdf"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "pdf"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert "data:image/jp2;base64," not in svg
    assert not report["warnings"]


def test_converts_legacy_word_binary_text(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_legacy.doc"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "doc"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "document-svg sample" in svg
    assert "Supported inputs" in svg
    assert any("layout" in warning for warning in report["warnings"])


def test_converts_legacy_powerpoint_binary_slides(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_legacy.ppt"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "ppt"
    assert report["page_count"] == 2
    first = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    second = (output / report["pages"][1]["svg"]).read_text(encoding="utf-8")
    assert "document-svg" in first
    assert "One page in, one SVG out" in second
    assert any("geometry" in warning for warning in report["warnings"])


def test_converts_kicad_pcb_board(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.kicad_pcb"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "kicad_pcb"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "PCB DEMO" in svg
    assert "R1" in svg
    assert any("2D artwork" in warning for warning in report["warnings"])


def test_converts_binary_dxf_drawing(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "binary_line.dxf"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "dxf"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-source-format="dxf"' in svg
    assert "<path" in svg
    assert (output / report["pages"][0]["svg"]).is_file()


def test_converts_tiff_image_directory(tmp_path: Path) -> None:
    source = tmp_path / "scan.tif"
    output = tmp_path / "out"
    write_minimal_tiff(source)

    report = convert(source, output)

    assert report["source_format"] == "tiff"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg


def test_converts_dicom_frames_without_emitting_patient_metadata(tmp_path: Path) -> None:
    fixture_directory = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    source = fixture_directory / "sample_multiframe.dcm"
    output = tmp_path / "dicom"

    report = convert(source, output)

    assert report["source_format"] == "dicom"
    assert report["page_count"] == 2
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="dicom:image-frame"' in svg
    assert "data:image/png;base64," in svg
    assert "Doe^DICOM QA" not in svg

    jpeg2000 = convert(
        fixture_directory / "sample_jpeg2000.dcm",
        tmp_path / "dicom-jpeg2000",
    )
    assert jpeg2000["source_format"] == "dicom"
    assert jpeg2000["page_count"] == 1
    jpeg2000_svg = (tmp_path / "dicom-jpeg2000" / jpeg2000["pages"][0]["svg"]).read_text(
        encoding="utf-8"
    )
    assert "data:image/png;base64," in jpeg2000_svg


def test_converts_dicom_encapsulated_pdf_without_serializing_dicom_attributes(tmp_path: Path) -> None:
    fixture_directory = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    report = convert(
        fixture_directory / "sample_encapsulated_pdf.dcm",
        tmp_path / "dicom-pdf",
    )
    assert report["source_format"] == "dicom"
    assert report["page_count"] == 1
    svg = (tmp_path / "dicom-pdf" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "DICOM Encapsulated PDF" in svg
    assert "Synthetic radiology report" in svg
    assert "Synthetic^Patient" not in svg
    assert any("not de-identification" in warning for warning in report["pages"][0]["warnings"])


def test_converts_v2000_sdf_records_to_sequential_molecule_pages(tmp_path: Path) -> None:
    fixture_directory = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    report = convert(
        fixture_directory / "sample_molecules.sdf",
        tmp_path / "sdf",
    )
    assert report["source_format"] == "sdf"
    assert report["page_count"] == 2
    first_svg = (tmp_path / "sdf" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    second_svg = (tmp_path / "sdf" / report["pages"][1]["svg"]).read_text(encoding="utf-8")
    assert "Synthetic Aromatic Ion" in first_svg
    assert "chemical:bond" in first_svg
    assert "N+" in first_svg
    assert "Synthetic Water" in second_svg


def test_converts_v3000_molfile_with_nonsequential_atom_identifiers(tmp_path: Path) -> None:
    fixture_directory = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    report = convert(
        fixture_directory / "sample_molecule_v3000.mol",
        tmp_path / "v3000-mol",
    )
    assert report["source_format"] == "mol"
    assert report["page_count"] == 1
    svg = (tmp_path / "v3000-mol" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Synthetic V3000 Isotope" in svg
    assert "18O-" in svg
    assert "chemical:stereo-bond" in svg
    assert any("Sgroups" in warning for warning in report["pages"][0]["warnings"])


def test_converts_v2000_rxn_reactants_and_products(tmp_path: Path) -> None:
    fixture_directory = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    report = convert(
        fixture_directory / "sample_reaction.rxn",
        tmp_path / "reaction",
    )
    assert report["source_format"] == "rxn"
    assert report["page_count"] == 1
    svg = (tmp_path / "reaction" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Reactants" in svg
    assert "Products" in svg
    assert "chemical:reaction-arrow" in svg
    assert "Reactant carbonyl" in svg
    assert "Product chloromethanol" in svg


def test_converts_dicomdir_file_set(tmp_path: Path) -> None:
    source = (
        Path(__file__).resolve().parents[3]
        / "tests"
        / "fixtures"
        / "dicom_set"
        / "DICOMDIR"
    )
    report = convert(source, tmp_path / "dicomdir")
    assert report["source_format"] == "dicomdir"
    assert report["page_count"] == 3
    svg = (tmp_path / "dicomdir" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-source-format="dicomdir"' in svg
    assert "Private^DirectoryPatient" not in svg


def test_converts_webp_raster_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.webp"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "raster"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "vectorized_path" in svg


def test_converts_first_frame_of_animated_gif(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.gif"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "raster"
    assert report["page_count"] == 1
    assert any("first frame" in warning for warning in report["warnings"])


def test_converts_cbz_pages_in_natural_order(tmp_path: Path) -> None:
    source = tmp_path / "comic.cbz"
    output = tmp_path / "out"
    write_minimal_cbz(source)

    report = convert(source, output)

    assert report["source_format"] == "cbz"
    assert report["page_count"] == 2
    first_svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in first_svg


def test_converts_abaqus_mesh_deck(tmp_path: Path) -> None:
    source = tmp_path / "triangle.inp"
    output = tmp_path / "out"
    source.write_text(
        "*Heading\nAbaqus triangle\n*Node\n"
        "1, 0, 0, 0\n2, 1, 0, 0\n3, 0, 1, 0\n"
        "*Element, type=CPS3\n1, 1, 2, 3\n",
        encoding="utf-8",
    )

    report = convert(source, output)

    assert report["source_format"] == "abaqus"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="simulation:mesh"' in svg


def test_converts_nastran_bulk_data_mesh(tmp_path: Path) -> None:
    source = tmp_path / "triangle.nas"
    output = tmp_path / "out"
    source.write_text(
        "$ Nastran triangle\nGRID 1 0 0 0 0\nGRID 2 0 1 0 0\n"
        "GRID 3 0 0 1 0\nCTRIA3 8 1 1 2 3\n",
        encoding="ascii",
    )

    report = convert(source, output)

    assert report["source_format"] == "nastran"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="simulation:mesh"' in svg


def test_converts_lsdyna_keyword_mesh(tmp_path: Path) -> None:
    source = tmp_path / "panel.key"
    output = tmp_path / "out"
    source.write_text(
        "*KEYWORD\n*TITLE\nLS-DYNA panel\n*ELEMENT_SHELL\n1,1,1,2,3,4\n"
        "*NODE\n1,0,0,0\n2,100,0,0\n3,100,50,0\n4,0,50,0\n*END\n",
        encoding="ascii",
    )

    report = convert(source, output)

    assert report["source_format"] == "lsdyna"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="simulation:mesh"' in svg


def test_converts_jupyter_notebook(tmp_path: Path) -> None:
    source = tmp_path / "analysis.ipynb"
    output = tmp_path / "out"
    source.write_text(
        '{"nbformat":4,"nbformat_minor":5,"metadata":{},"cells":['
        '{"cell_type":"markdown","metadata":{},"source":["# Notebook report\\n"]},'
        '{"cell_type":"code","execution_count":1,"metadata":{},"source":["print(2)"],'
        '"outputs":[{"output_type":"stream","name":"stdout","text":["2\\n"]}]}]}',
        encoding="utf-8",
    )

    report = convert(source, output)

    assert report["source_format"] == "jupyter"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Notebook report" in svg
    assert "print(2)" in svg


def test_converts_jupyter_markdown_attachment_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "jupyter_attachment.ipynb"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "jupyter"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert 'aria-label="attached chart"' in svg
    assert any("attachment images" in warning for warning in report["warnings"])


def test_converts_jats_article_with_local_figure(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.jats"
    target = tmp_path / "article.jats"
    assets = tmp_path / "assets"
    assets.mkdir()
    target.write_bytes(fixture.read_bytes())
    image = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "assets" / "red-blue.png"
    (assets / "red-blue.png").write_bytes(image.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "jats"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "JATS Article Preview" in svg
    assert "data:image/png;base64," in svg
    assert "Ready" in svg


def test_converts_docbook_article_with_local_figure(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.docbook"
    target = tmp_path / "article.dbk"
    assets = tmp_path / "assets"
    assets.mkdir()
    target.write_bytes(fixture.read_bytes())
    image = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "assets" / "red-blue.png"
    (assets / "red-blue.png").write_bytes(image.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "docbook"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "DocBook Preview" in svg
    assert "data:image/png;base64," in svg
    assert "Ready" in svg


def test_converts_dita_topic_and_local_topic_map(tmp_path: Path) -> None:
    fixture_root = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    target = tmp_path / "guide.dita"
    target.write_bytes((fixture_root / "sample.dita").read_bytes())
    (tmp_path / "assets").mkdir()
    (tmp_path / "assets" / "red-blue.png").write_bytes(
        (fixture_root / "assets" / "red-blue.png").read_bytes()
    )
    report = convert(target, tmp_path / "topic-out")
    assert report["source_format"] == "dita"
    svg = (tmp_path / "topic-out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "DITA Preview" in svg
    assert "data:image/png;base64," in svg

    (tmp_path / "guide.ditamap").write_bytes((fixture_root / "sample.ditamap").read_bytes())
    (tmp_path / "second.dita").write_bytes((fixture_root / "second.dita").read_bytes())
    map_report = convert(tmp_path / "guide.ditamap", tmp_path / "map-out")
    assert map_report["source_format"] == "dita"
    map_svg = (tmp_path / "map-out" / map_report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "DITA Guide Map" in map_svg
    assert "Second Topic" in map_svg


def test_converts_pdb_models(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.pdb"
    target = tmp_path / "models.pdb"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "pdb"
    assert report["page_count"] == 2
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "SAMPLE PDB PREVIEW" in svg
    assert "chemical:bond" in svg


def test_converts_hwpx_sections_and_package_image(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.hwpx"
    target = tmp_path / "report.hwpx"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "hwpx"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "HWPX Preview" in svg
    assert "Ready" in svg
    assert "data:image/png;base64," in svg


def test_converts_collada_triangle(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "triangle.dae"
    target = tmp_path / "triangle.dae"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "collada"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "obj:background" in svg
    assert any("materials" in warning for warning in report["warnings"])


def test_converts_x3d_triangle(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "triangle.x3d"
    target = tmp_path / "triangle.x3d"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "x3d"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "obj:background" in svg


def test_converts_xmind_outline(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.xmind"
    target = tmp_path / "map.xmind"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "xmind"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "XMind Preview" in svg
    assert "Child topic" in svg


def test_converts_xmind_json_content(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_json.xmind"
    target = tmp_path / "json-map.xmind"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "xmind"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "JSON XMind Preview" in svg
    assert "JSON Child" in svg


def test_converts_nifti_gzip_slices(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.nii.gz"
    target = tmp_path / "volume.nii.gz"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "nifti"
    assert report["page_count"] == 2
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "NIfTI slice 1" in svg
    assert "data:image/png;base64," in svg


def test_converts_fits_gzip_planes(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.fits.gz"
    target = tmp_path / "image.fits.gz"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "fits"
    assert report["page_count"] == 2
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "FITS image plane 1" in svg
    assert "data:image/png;base64," in svg


def test_converts_mrc_gzip_planes(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.mrc.gz"
    target = tmp_path / "map.mrc.gz"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "mrc"
    assert report["page_count"] == 2
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "MRC plane 1" in svg


def test_converts_netcdf_classic_dimensions_and_variables(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_netcdf.nc"
    target = tmp_path / "data.nc"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "netcdf"
    assert report["page_count"] == 1
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "NetCDF CDF-1" in svg
    assert "NetCDF demo" in svg
    assert "20" in svg


def test_converts_standalone_jpeg2000_images(tmp_path: Path) -> None:
    fixture_directory = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    for filename in ("sample_jpeg2000.jp2", "sample_jpeg2000.j2k", "sample_jpeg2000_rgba.jp2"):
        target = tmp_path / filename
        target.write_bytes((fixture_directory / filename).read_bytes())
        report = convert(target, tmp_path / f"{filename}-out")
        assert report["source_format"] == "jpeg2000"
        assert report["page_count"] == 1
        svg = (tmp_path / f"{filename}-out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
        assert "data:image/png;base64," in svg


def test_converts_xgmml_nodes_and_edges(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.xgmml"
    target = tmp_path / "network.xgmml"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "xgmml"
    assert report["page_count"] == 1
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Start order" in svg
    assert "approve" in svg


def test_converts_sqlite_user_tables(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.sqlite"
    target = tmp_path / "data.sqlite"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "sqlite"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "people" in svg
    assert "Alice" in svg


def test_converts_cif_atom_site_models(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.cif"
    target = tmp_path / "structure.cif"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "cif"
    assert report["page_count"] == 2
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "chemical:atom" in svg


def test_converts_mol2_structure(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.mol2"
    target = tmp_path / "structure.mol2"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "mol2"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "chemical:bond" in svg


def test_converts_turtle_statements(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.ttl"
    target = tmp_path / "graph.ttl"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "turtle"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "RDF statements" in svg
    assert "Alice" in svg


def test_converts_eps_vector_paths(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "triangle.eps"
    target = tmp_path / "triangle.eps"
    target.write_bytes(fixture.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "eps"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "eps:path" in svg


def test_converts_r_markdown_source_without_execution(tmp_path: Path) -> None:
    source = tmp_path / "report.Rmd"
    output = tmp_path / "out"
    source.write_text(
        "---\ntitle: R Markdown report\noutput: html_document\n---\n\n"
        "# Findings\n\n```{r}\nmean(c(1, 2, 3))\n```\n",
        encoding="utf-8",
    )

    report = convert(source, output)

    assert report["source_format"] == "quarto"
    assert report["page_count"] == 1
    assert any("are not executed" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "R Markdown report" in svg
    assert "mean(c(1, 2, 3))" in svg


def test_converts_quarto_local_and_reference_images(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "quarto_image.qmd"
    assets = tmp_path / "assets"
    assets.mkdir()
    target = tmp_path / "report.qmd"
    target.write_bytes(source.read_bytes())
    target_image = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "assets" / "red-blue.png"
    (assets / "red-blue.png").write_bytes(target_image.read_bytes())
    report = convert(target, tmp_path / "out")
    assert report["source_format"] == "quarto"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'aria-label="Local Quarto image"' in svg
    assert 'aria-label="Reference Quarto image"' in svg
    assert "example.invalid" not in svg


def test_converts_medit_ascii_mesh(tmp_path: Path) -> None:
    source = tmp_path / "surface.medit"
    output = tmp_path / "out"
    source.write_text(
        "MeshVersionFormatted 2\nDimension 2\nVertices 3\n"
        "0 0 0\n100 0 0\n0 100 0\nTriangles 1\n1 2 3 1\nEnd\n",
        encoding="ascii",
    )

    report = convert(source, output)

    assert report["source_format"] == "medit"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="simulation:mesh"' in svg


def test_converts_binary_medit_meshb(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "medit_binary_v2.meshb"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "medit"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "MEDIT binary finite-element mesh" in svg
    assert 'data-semantic-role="simulation:mesh"' in svg


def test_converts_off_polygon_mesh(tmp_path: Path) -> None:
    source = tmp_path / "surface.off"
    output = tmp_path / "out"
    source.write_text(
        "OFF\n4 1 4\n0 0 0\n100 0 0\n100 100 0\n"
        "0 100 0\n4 0 1 2 3\n",
        encoding="ascii",
    )

    report = convert(source, output)

    assert report["source_format"] == "off"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="obj:mesh"' in svg


def test_converts_ifc4_tessellated_geometry(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.ifc"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "ifc"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "IFC tessellated building model" in svg
    assert 'data-semantic-role="obj:mesh"' in svg
    assert any("materials" in warning for warning in report["warnings"])


def test_converts_reused_ifc_mapped_geometry(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_mapped.ifc"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "ifc"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "IFC tessellated building model" in svg
    assert 'data-semantic-role="obj:mesh"' in svg
    assert not any("mapped geometry" in warning for warning in report["warnings"])


def test_converts_ifc_extruded_area_solids(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_extruded.ifc"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "ifc"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "IFC tessellated building model" in svg
    assert 'data-semantic-role="obj:mesh"' in svg
    assert any("materials" in warning for warning in report["warnings"])


def test_converts_ifcxml_geometry(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_ifcxml.ifcxml"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "ifcxml"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="obj:mesh"' in svg
    assert any("materials" in warning for warning in report["warnings"])


def test_converts_ifczip_archive(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "samples" / "source" / "sample.ifczip"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "ifczip"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-source-format="ifczip"' in svg
    assert svg.count('<path id=""') == 24


def test_converts_su2_cfd_mesh(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.su2"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "su2"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="simulation:mesh"' in svg
    assert 'data-semantic-role="simulation:marker-boundary"' in svg
    assert any("marker name" in warning for warning in report["warnings"])


def test_converts_openfoam_poly_mesh_case(tmp_path: Path) -> None:
    source = (
        Path(__file__).resolve().parents[3]
        / "tests"
        / "fixtures"
        / "openfoam_case"
        / "sample.foam"
    )
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "openfoam"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="simulation:mesh"' in svg
    assert 'data-semantic-role="simulation:boundary-patch"' in svg
    assert any("patch names" in warning for warning in report["warnings"])


def test_converts_gzip_compressed_openfoam_poly_mesh(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "openfoam_case"
    case = tmp_path / "compressed-case"
    poly_mesh = case / "constant" / "polyMesh"
    poly_mesh.mkdir(parents=True)
    (case / "case.foam").write_bytes((source / "sample.foam").read_bytes())
    for name in ("points", "faces", "owner", "neighbour", "boundary"):
        data = (source / "constant" / "polyMesh" / name).read_bytes()
        (poly_mesh / f"{name}.gz").write_bytes(gzip.compress(data))
    output = tmp_path / "out"

    report = convert(case / "case.foam", output)

    assert report["source_format"] == "openfoam"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="simulation:boundary-patch"' in svg


def test_converts_vertex_only_ply_point_cloud(tmp_path: Path) -> None:
    source = tmp_path / "scan.ply"
    output = tmp_path / "out"
    source.write_text(
        "ply\nformat ascii 1.0\nelement vertex 4\n"
        "property float x\nproperty float y\nproperty float z\n"
        "end_header\n-10 0 0\n10 0 0\n0 -10 0\n0 10 0\n",
        encoding="ascii",
    )

    report = convert(source, output)

    assert report["source_format"] == "ply"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="ply:point-cloud"' in svg


def test_converts_pcd_point_cloud(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "point_cloud_ascii.pcd"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "pcd"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "PCD Point Cloud" in svg
    assert 'data-semantic-role="pcd:point-cloud"' in svg
    assert 'fill="#FF0000"' in svg
    assert 'fill="#00FF00"' in svg


def test_converts_legacy_excel_workbook(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "legacy_sample.xls"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "xls"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "sample — rows 1–4, columns A–B" in svg
    assert "Alpha" in svg


def test_converts_xlsb_workbook(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "calamine_any_sheets.xlsb"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "xlsb"
    assert report["page_count"] == 3
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Visible — rows 1–5" in svg


def test_converts_geojson_map(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.geojson"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "geojson"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "GeoJSON Map Preview" in svg
    assert 'data-semantic-role="geojson:polygon"' in svg


def test_converts_rfc8142_geojson_text_sequence(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.geojsons"
    report = convert(source, tmp_path / "geojson-sequence")
    assert report["source_format"] == "geojsonseq"
    assert report["page_count"] == 1
    svg = (tmp_path / "geojson-sequence" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="geojsonseq:point"' in svg
    assert 'data-semantic-role="geojsonseq:line"' in svg
    assert 'data-semantic-role="geojsonseq:polygon"' in svg
    assert "Private feature attribute" not in svg


def test_converts_topojson_shared_arcs(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.topojson"
    report = convert(source, tmp_path / "topojson")
    assert report["source_format"] == "topojson"
    assert report["page_count"] == 1
    svg = (tmp_path / "topojson" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="topojson:polygon"' in svg
    assert 'data-semantic-role="topojson:point"' in svg
    assert "private west" not in svg


def test_converts_generic_rfc7464_json_text_sequence(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.jsons"
    report = convert(source, tmp_path / "json-sequence")
    assert report["source_format"] == "jsonseq"
    assert report["page_count"] == 1
    svg = (tmp_path / "json-sequence" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "JSON sequence record 2" in svg
    assert "validated" in svg
    assert "42" in svg


def test_converts_esri_shapefile_polygon(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_polygon.shp"
    report = convert(source, tmp_path / "shapefile")
    assert report["source_format"] == "shapefile"
    assert report["page_count"] == 1
    svg = (tmp_path / "shapefile" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Shapefile Map Preview" in svg
    assert 'data-semantic-role="shapefile:polygon"' in svg
    assert "1 features, 1 geometries, and 15 positions" in svg


def test_converts_geopackage_vector_layers(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_features.gpkg"
    report = convert(source, tmp_path / "geopackage")
    assert report["source_format"] == "geopackage"
    assert report["page_count"] == 3
    first = (tmp_path / "geopackage" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="geopackage:polygon"' in first
    assert any("EPSG:3857" in warning for warning in report["warnings"])
    assert any("Z and M" in warning for warning in report["warnings"])


def test_converts_geopackage_raster_tiles(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_tile_mixed.gpkg"
    report = convert(source, tmp_path / "geopackage-tiles")
    assert report["source_format"] == "geopackage"
    assert report["page_count"] == 2
    svg = (tmp_path / "geopackage-tiles" / report["pages"][1]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="geopackage:tile"' in svg
    assert "data:image/png;base64," in svg
    assert any("re-encoded as PNG" in warning for warning in report["pages"][1]["warnings"])


def test_converts_georss_simple_feed(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.georss"
    report = convert(source, tmp_path / "georss")
    assert report["source_format"] == "georss"
    assert report["page_count"] == 1
    svg = (tmp_path / "georss" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="georss:polygon"' in svg
    assert 'data-semantic-role="georss:point"' in svg
    assert "6 features, 6 geometries, and 19 positions" in svg


def test_converts_gml_feature_geometries(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.gml"
    report = convert(source, tmp_path / "gml")
    assert report["source_format"] == "gml"
    assert report["page_count"] == 1
    svg = (tmp_path / "gml" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="gml:polygon"' in svg
    assert 'data-semantic-role="gml:line"' in svg
    assert "3 features, 3 geometries, and 14 positions" in svg


def test_converts_gpx_waypoints_routes_and_tracks(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.gpx"
    report = convert(source, tmp_path / "gpx")
    assert report["source_format"] == "gpx"
    assert report["page_count"] == 1
    svg = (tmp_path / "gpx" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="gpx:point"' in svg
    assert 'data-semantic-role="gpx:line"' in svg
    assert "3 features, 5 geometries, and 8 positions" in svg


def test_converts_wkt_simple_features(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.wkt"
    report = convert(source, tmp_path / "wkt")
    assert report["source_format"] == "wkt"
    assert report["page_count"] == 1
    svg = (tmp_path / "wkt" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="wkt:polygon"' in svg
    assert 'data-semantic-role="wkt:point"' in svg
    assert "1 features, 7 geometries, and 24 positions" in svg


def test_converts_kml_and_kmz_maps(tmp_path: Path) -> None:
    fixture_dir = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    kml_report = convert(fixture_dir / "sample.kml", tmp_path / "kml")
    assert kml_report["source_format"] == "kml"
    assert kml_report["page_count"] == 1
    kml_svg = (tmp_path / "kml" / kml_report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="kml:polygon"' in kml_svg
    assert 'data-semantic-role="kml:point"' in kml_svg

    kmz_report = convert(fixture_dir / "sample.kmz", tmp_path / "kmz")
    assert kmz_report["source_format"] == "kmz"
    assert kmz_report["page_count"] == 1
    kmz_svg = (tmp_path / "kmz" / kmz_report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="kmz:point"' in kmz_svg


def test_converts_las_point_cloud(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_rgb.las"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "las"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "LAS/LAZ Point Cloud" in svg
    assert 'data-semantic-role="las:point-cloud"' in svg
    assert 'fill="#FF0000"' in svg


def test_converts_multiscan_ptx_point_cloud(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.ptx"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "ptx"
    assert report["page_count"] == 2
    first = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    second = (output / report["pages"][1]["svg"]).read_text(encoding="utf-8")
    assert "PTX scan 1" in first
    assert 'data-semantic-role="ptx:point-cloud"' in first
    assert 'fill="#2563EB"' in first
    assert "PTX scan 2" in second
    assert any("mark no color" in warning for warning in report["pages"][0]["warnings"])
    assert all("scan 1" not in warning for warning in report["pages"][1]["warnings"])
    assert any("no-return" in warning for warning in report["warnings"])
    assert any("mark no color" in warning for warning in report["warnings"])


def test_converts_leica_pts_point_cloud(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.pts"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "pts"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="pts:point-cloud"' in svg
    assert 'fill="#FF0000"' in svg
    assert 'fill="#2563EB"' in svg
    assert any("mark no color" in warning for warning in report["warnings"])


def test_converts_public_astm_e57_point_cloud(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_e57_bunny.e57"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "e57"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="e57:point-cloud"' in svg
    assert any("image blobs" in warning for warning in report["warnings"])
    assert all("invalid E57 colors" not in warning for warning in report["pages"][0]["warnings"])


def test_converts_xyz_rgb_point_cloud(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.xyz"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "xyz"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="xyz:point-cloud"' in svg
    assert 'fill="#000000"' in svg
    assert 'fill="#FF0000"' in svg

    header_source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_xyz_header.xyz"
    header_report = convert(header_source, tmp_path / "header-out")
    assert header_report["source_format"] == "xyz"
    header_svg = (tmp_path / "header-out" / header_report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'fill="#00FF00"' in header_svg
    assert any("unrecognized XYZ header" in warning for warning in header_report["warnings"])


def test_converts_esri_ascii_grid_raster(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_elevation.asc"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "esri_ascii_grid"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="esri:ascii-grid-raster"' in svg
    assert "data:image/png;base64," in svg
    assert any("NODATA_VALUE" in warning for warning in report["warnings"])


def test_converts_rfc4180_quoted_csv_cells(tmp_path: Path) -> None:
    source = tmp_path / "quoted.csv"
    output = tmp_path / "csv-out"
    source.write_text('Name,Note\r\n"Acme, Inc.","She said ""hello"""\r\n', encoding="utf-8")

    report = convert(source, output)

    assert report["source_format"] == "csv"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Acme, Inc." in svg
    assert 'She said "hello"' in svg


def test_converts_arff_dense_and_sparse_records(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.arff"
    output = tmp_path / "arff-out"

    report = convert(source, output)

    assert report["source_format"] == "arff"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "temperature (numeric)" in svg
    assert "sunny" in svg
    assert "18" in svg


def test_converts_jsonld_nodes_and_named_graphs(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.jsonld"
    output = tmp_path / "jsonld-out"

    report = convert(source, output)

    assert report["source_format"] == "jsonld"
    assert report["page_count"] == 1
    assert any("@context" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "urn:book:1" in svg
    assert "urn:graph:1" in svg


def test_converts_graphml_nodes_and_edges(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.graphml"
    output = tmp_path / "graphml-out"

    report = convert(source, output)

    assert report["source_format"] == "graphml"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Start order" in svg
    assert "approve" in svg


def test_converts_dbase_attribute_table(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_attributes.dbf"
    output = tmp_path / "dbase-out"

    report = convert(source, output)

    assert report["source_format"] == "dbf"
    assert report["page_count"] == 1
    assert any("deleted dBASE record" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Café Moreno" in svg
    assert "AREA_HA" in svg
    assert "Willow Farm" not in svg


def test_converts_toml_configuration_without_executing_values(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_config.toml"
    output = tmp_path / "toml-out"

    report = convert(source, output)

    assert report["source_format"] == "toml"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "$.service.name (string)" in svg
    assert "$.targets[1].name" in svg
    assert "ap-northeast-1" in svg
    assert any("float value(s) were normalized" in warning for warning in report["warnings"])


def test_converts_yaml_documents_without_expanding_aliases_or_tags(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_config.yaml"
    output = tmp_path / "yaml-out"

    report = convert(source, output)

    assert report["source_format"] == "yaml"
    assert report["page_count"] == 2
    svg = "\n".join(
        (output / page["svg"]).read_text(encoding="utf-8")
        for page in report["pages"]
    )
    assert "$.service.name (scalar)" in svg
    assert "$.targets[1].name" in svg
    assert "Document 2" in svg
    assert "not expanded" in svg
    assert any("custom-tagged" in warning for warning in report["warnings"])


def test_converts_generic_xml_configuration_as_inert_path_rows(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_config.xml"
    output = tmp_path / "xml-out"

    report = convert(source, output)

    assert report["source_format"] == "xml"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'namespace ns1 = "urn:example:application"' in svg
    assert "/ns1:application (element)" in svg
    assert "catalog-api" in svg
    assert "full-text &amp; faceted" in svg


def test_converts_java_properties_without_evaluating_placeholders(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_properties.properties"
    output = tmp_path / "properties-out"

    report = convert(source, output)

    assert report["source_format"] == "properties"
    assert report["page_count"] == 1
    assert any("duplicate" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Catalog API" in svg
    assert "Café" in svg
    assert "${HOME} is displayed as text" in svg
    assert '$["release.channel"] = "stable"' in svg
    assert '$["release.channel"] = "old"' not in svg


def test_converts_bpmn_20_diagrams_with_di_geometry(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_bpmn.bpmn"
    output = tmp_path / "bpmn-out"

    report = convert(source, output)

    assert report["source_format"] == "bpmn"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Order fulfillment" in svg
    assert "Validate order" in svg
    assert "bpmn:sequence-flow-arrow" in svg
    assert report["warnings"] == []


def test_converts_dmn_decision_tables_without_evaluating_feel(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_dmn.dmn"
    output = tmp_path / "dmn-out"

    report = convert(source, output)

    assert report["source_format"] == "dmn"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Loan approval" in svg
    assert "Applicant age" in svg
    assert "manual review" in svg
    assert "FEEL is" in svg
    assert report["warnings"] == []


def test_converts_cmmn_case_plan_with_di_geometry(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_cmmn.cmmn"
    output = tmp_path / "cmmn-out"

    report = convert(source, output)

    assert report["source_format"] == "cmmn"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Claims file" in svg
    assert "Review documents" in svg
    assert "cmmn:connector" in svg
    assert any("not evaluated" in warning for warning in report["warnings"])


def test_converts_reqif_requirements_and_relations(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_requirements.reqif"
    output = tmp_path / "reqif-out"

    report = convert(source, output)

    assert report["source_format"] == "reqif"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Vehicle braking requirements" in svg
    assert "Requirement details" in svg
    assert "Stopping distance" in svg
    assert "Derives" in svg
    assert any("XHTML" in warning for warning in report["warnings"])


def test_converts_xmi_model_elements_without_external_dereferencing(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_model.xmi"
    output = tmp_path / "xmi-out"

    report = convert(source, output)

    assert report["source_format"] == "xmi"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "BrakeController" in svg
    assert "WheelSensor" in svg
    assert "type=double" in svg


def test_converts_rfc4180_quoted_csv_cells(tmp_path: Path) -> None:
    source = tmp_path / "quoted.csv"
    output = tmp_path / "csv-out"
    source.write_text('Name,Note\r\n"Acme, Inc.","She said ""hello"""\r\n', encoding="utf-8")

    report = convert(source, output)

    assert report["source_format"] == "csv"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Acme, Inc." in svg
    assert 'She said "hello"' in svg


def test_converts_visio_open_xml_page(tmp_path: Path) -> None:
    source = tmp_path / "flow.vsdx"
    output = tmp_path / "out"
    write_minimal_vsdx(source)

    report = convert(source, output)

    assert report["source_format"] == "visio"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-semantic-role="office:visio-shape"' in svg
    assert "Visio &amp; Python" in svg


def test_converts_legacy_visio_xml_drawing(tmp_path: Path) -> None:
    source = tmp_path / "legacy.vdx"
    output = tmp_path / "out"
    write_minimal_vdx(source)

    report = convert(source, output)

    assert report["source_format"] == "visio"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Legacy &amp; Python" in svg


def test_converts_eml_message(tmp_path: Path) -> None:
    source = tmp_path / "message.eml"
    output = tmp_path / "out"
    source.write_text(
        "From: Alice <alice@example.test>\r\n"
        "Subject: EML preview\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n\r\n"
        "A readable message body.\r\n",
        encoding="utf-8",
    )

    report = convert(source, output)

    assert report["source_format"] == "eml"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "EML preview" in svg
    assert "A readable message body." in svg


def test_converts_rfc3676_flowed_plain_text(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "flowed_message.eml"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "eml"
    assert any("RFC 3676 flowed plain text" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "A flowed line with a preserved word." in svg
    assert "From this sender." in svg
    assert "&gt; quoted words continue." in svg
    assert "Signature line." in svg


def test_converts_apple_mail_emlx_message_with_exact_declared_length(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.emlx"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "emlx"
    assert report["page_count"] == 1
    assert any("property-list metadata was ignored" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Apple Mail message" in svg
    assert "日本語の本文です。" in svg
    assert "plist" not in svg
    assert "flags" not in svg


def test_converts_eml_cid_inline_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "eml_cid_image.eml"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "eml"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert svg.count("data:image/png;base64,") == 2
    assert "brand logo" in svg
    assert "location logo" in svg
    assert "example.invalid" not in svg
    assert "must not render" not in svg


def test_converts_outlook_msg_message(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "outlook_message.msg"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "msg"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Outlook preview" in svg
    assert "Rendered" in svg
    assert "日本語表示" in svg
    assert "Attachments: 1 omitted" in svg
    assert "window.alert" not in svg
    assert "example.invalid" not in svg


def test_converts_mbox_messages_to_sequential_pages(tmp_path: Path) -> None:
    source = tmp_path / "archive.mbox"
    output = tmp_path / "out"
    source.write_text(
        "From alice@example.test Sat Sep 14 10:00:00 2024\r\n"
        "From: Alice <alice@example.test>\r\n"
        "Subject: First archived message\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n\r\n"
        "First mailbox body.\r\n\r\n"
        "From bob@example.test Sun Sep 15 11:30:00 2024\r\n"
        "From: Bob <bob@example.test>\r\n"
        "Subject: Second archived message\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n\r\n"
        "Second mailbox body.\r\n",
        encoding="utf-8",
    )

    report = convert(source, output)

    assert report["source_format"] == "mbox"
    assert report["page_count"] == 2
    first = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    second = (output / report["pages"][1]["svg"]).read_text(encoding="utf-8")
    assert "First archived message" in first
    assert "Second archived message" in second


def test_converts_mhtml_saved_web_page(tmp_path: Path) -> None:
    source = tmp_path / "saved.mhtml"
    output = tmp_path / "out"
    source.write_text(
        "MIME-Version: 1.0\r\n"
        'Content-Type: multipart/related; boundary=page; type="text/html"\r\n\r\n'
        "--page\r\nContent-Type: text/html; charset=utf-8\r\n\r\n"
        "<html><body><h1>Saved MHTML</h1><p>Archived page from Python.</p>"
        "</body></html>\r\n--page--\r\n",
        encoding="utf-8",
    )

    report = convert(source, output)

    assert report["source_format"] == "mhtml"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Saved MHTML" in svg
    assert "Archived page from Python." in svg


def test_converts_local_html_images_without_fetching_remote_paths(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "local_image.html"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "html"
    assert report["page_count"] == 1
    assert any("external image resources" in warning for warning in report["warnings"])
    assert any("srcset uses its first candidate" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert 'aria-label="red-blue HTML image"' in svg
    assert "example.invalid" not in svg


def test_converts_markdown_with_local_image_blocks(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "markdown_image.md"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "markdown"
    assert report["page_count"] == 1
    assert any("external image resources" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert 'aria-label="red-blue Markdown image"' in svg
    assert 'aria-label="red-blue inline image"' in svg
    assert 'aria-label="red-blue reference image"' in svg
    assert "example.invalid" not in svg


def test_converts_asciidoc_with_local_block_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "asciidoc_image.adoc"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "asciidoc"
    assert report["page_count"] == 1
    assert any("not a validated local PNG/JPEG" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert 'aria-label="Red and blue test image"' in svg
    assert "example.invalid" not in svg


def test_converts_complete_latex_without_executing_tex(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample_latex.tex"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "tex"
    assert report["page_count"] == 1
    assert any("input/include/shell" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "LaTeX Document Preview" in svg
    assert "data:image/png;base64," in svg
    assert 'aria-label="red blue"' in svg
    assert "not-loaded.tex" not in svg


def test_converts_fictionbook_with_embedded_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.fb2"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "fb2"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "FictionBook Preview" in svg
    assert "Chapter One" in svg
    assert "data:image/png;base64," in svg
    assert "Notes are not part of the main flow" not in svg


def test_converts_zip_wrapped_fictionbook(tmp_path: Path) -> None:
    fixture = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.fb2"
    source = tmp_path / "book.fb2.zip"
    with zipfile.ZipFile(source, "w") as archive:
        archive.writestr("books/book.fb2", fixture.read_bytes())

    report = convert(source, tmp_path / "out")

    assert report["source_format"] == "fb2"
    assert report["page_count"] == 1
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "FictionBook Preview" in svg


def test_converts_bounded_mobi_palmdoc_text(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.mobi"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "mobi"
    assert report["page_count"] == 1
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "MOBI Preview" in svg
    assert "PalmDOC text from a bounded record" in svg


def test_converts_restructuredtext_with_safe_local_images(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.rst"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "rst"
    assert report["page_count"] >= 1
    assert any("include directive was not evaluated" in warning for warning in report["warnings"])
    assert any("raw directive was not evaluated" in warning for warning in report["warnings"])
    assert any("external image resources" in warning for warning in report["warnings"])
    svg = "\n".join(
        (output / page["svg"]).read_text(encoding="utf-8") for page in report["pages"]
    )
    assert "Document Conversion Notes" in svg
    assert "named target" in svg
    assert ":ref:" not in svg
    assert "data:image/png;base64," in svg
    assert 'aria-label="RST red and blue image"' in svg
    assert "Figure caption is retained" in svg
    assert "example.invalid" not in svg
    assert "must-not-be-read" not in svg
    assert "must-not-render" not in svg


def test_converts_org_mode_without_executing_babel_or_includes(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.org"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "org"
    assert report["page_count"] >= 1
    assert any("#+INCLUDE was not evaluated" in warning for warning in report["warnings"])
    assert any("Babel calls were not executed" in warning for warning in report["warnings"])
    svg = "\n".join(
        (output / page["svg"]).read_text(encoding="utf-8") for page in report["pages"]
    )
    assert "Org-mode Architecture Notes" in svg
    assert "delete-file" in svg
    assert "data:image/png;base64," in svg
    assert 'width="96" height="48"' in svg
    assert "Local Org image caption" in svg
    assert "example.invalid" not in svg
    assert "/etc/passwd" not in svg


def test_converts_gettext_po_context_and_plural_forms(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.po"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "po"
    assert any("obsolete gettext entries" in warning for warning in report["warnings"])
    assert any("fuzzy gettext translations" in warning for warning in report["warnings"])
    svg = "\n".join(
        (output / page["svg"]).read_text(encoding="utf-8") for page in report["pages"]
    )
    assert "Language fr" in svg
    assert "main-menu" in svg
    assert "Bienvenue" in svg
    assert "[0] Un fichier" in svg
    assert "[1] %d fichiers" in svg
    assert "[fuzzy] À vérifier" in svg
    assert "Obsolete source text" not in svg


def test_converts_bibtex_without_expanding_macros(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.bib"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "bib"
    assert any("@string macros are not expanded" in warning for warning in report["warnings"])
    svg = "\n".join(
        (output / page["svg"]).read_text(encoding="utf-8") for page in report["pages"]
    )
    assert "smith2024" in svg
    assert "Nested Unicode Study" in svg
    assert "A value, with punctuation." in svg
    assert "joc Review" in svg
    assert "Catalog preview example" not in svg


def test_converts_rtf_with_embedded_png_picture(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "rtf_image.rtf"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "rtf"
    assert report["page_count"] == 1
    assert any("picture anchors" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert 'aria-label="Embedded RTF picture"' in svg


def test_converts_ical_event(tmp_path: Path) -> None:
    source = tmp_path / "calendar.ics"
    output = tmp_path / "out"
    source.write_bytes(
        b"BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\n"
        b"UID:event-1\r\nDTSTART:20241012T090000Z\r\n"
        b"SUMMARY:Calendar API event\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )

    report = convert(source, output)

    assert report["source_format"] == "ical"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Calendar API event" in svg


def test_converts_legacy_vcalendar_event(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "meeting.vcs"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "vcalendar"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Legacy vCalendar meeting" in svg
    assert "Conference room" in svg


def test_converts_vcard_contacts(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "contact.vcf"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "vcard"
    assert report["page_count"] == 2
    first = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    second = (output / report["pages"][1]["svg"]).read_text(encoding="utf-8")
    assert "山田花子" in first
    assert "Ren Tanaka" in second
    assert "example.invalid/avatar.jpg" not in first


def test_converts_vcard_21_quoted_printable_contact(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "legacy_contact_21.vcf"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "vcard"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Hanako 山田花子" in svg
    assert "hana@example.test" in svg
    assert "AA==" not in svg


def test_converts_open_document_text(tmp_path: Path) -> None:
    source = tmp_path / "report.odt"
    output = tmp_path / "out"
    write_minimal_odt(source)

    report = convert(source, output)

    assert report["source_format"] == "odt"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "ODT report" in svg
    assert "OpenDocument Text from Python." in svg


def test_converts_strict_ooxml_word_excel_and_powerpoint(tmp_path: Path) -> None:
    fixture_directory = Path(__file__).resolve().parents[3] / "tests" / "fixtures"
    cases = [
        ("sample_strict.docx", "docx", 2, "document-svg sample"),
        ("sample_strict.xlsx", "xlsx", 4, "Quarterly sales by region"),
        (
            "sample_strict.pptx",
            "pptx",
            2,
            "PDF and Office documents as self-contained SVG pages",
        ),
    ]
    for filename, source_format, page_count, expected_text in cases:
        output = tmp_path / filename
        report = convert(fixture_directory / filename, output)
        assert report["source_format"] == source_format
        assert report["page_count"] == page_count
        assert report["warnings"] == []
        svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
        assert expected_text in svg


def test_converts_microsoft_project_xml_schedule(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "sample.project.xml"
    output = tmp_path / "project-xml"

    report = convert(source, output)

    assert report["source_format"] == "projectxml"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Website Refresh Plan" in svg
    assert "office:project-task-bar" in svg
    assert "office:project-dependency-arrow" in svg
    assert "office:project-milestone" in svg
    assert "Private project resource" not in svg
    assert "Notes are data" not in svg


def test_converts_open_document_text_with_package_linked_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "odt_image.odt"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "odt"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert 'aria-label="red-blue sample"' in svg


def test_converts_flat_open_document_text_inline_binary_image(tmp_path: Path) -> None:
    source = tmp_path / "inline.fodt"
    source.write_text(
        '<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
        'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
        'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
        'xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text>'
        '<text:p>Before<draw:frame draw:name="inline"><draw:image xlink:href="fallback.png">'
        '<office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data>'
        '</draw:image></draw:frame>After</text:p></office:text></office:body></office:document>',
        encoding="utf-8",
    )
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "odt"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert not any("binary-data images are omitted" in warning for warning in report["warnings"])


def test_converts_open_document_text_styles(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "odt_styles.odt"
    output = tmp_path / "out"
    report = convert(source, output)

    assert report["source_format"] == "odt"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Styled ODT heading" in svg
    assert 'font-weight="700"' in svg
    assert 'font-style="italic"' in svg
    assert "#1D4ED8" in svg
    assert "#DC2626" in svg
    assert "#16A34A" in svg


def test_converts_epub_with_package_linked_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "epub_image.epub"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "epub"
    assert report["page_count"] == 1
    assert any("remote EPUB image" in warning for warning in report["warnings"])
    assert any("srcset uses its first candidate" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert 'aria-label="red-blue EPUB image"' in svg
    assert "example.invalid" not in svg


def test_converts_open_document_spreadsheet(tmp_path: Path) -> None:
    source = tmp_path / "metrics.ods"
    output = tmp_path / "out"
    write_minimal_ods(source)

    report = convert(source, output)

    assert report["source_format"] == "ods"
    assert report["page_count"] == 1
    assert any("styles" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Quarterly" in svg
    assert "Requests" in svg
    assert "1200" in svg


def test_converts_open_document_spreadsheet_with_package_linked_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "ods_image.ods"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "ods"
    assert report["page_count"] == 1
    assert any("external ODS image" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert 'aria-label="red-blue sheet image"' in svg
    assert 'aria-label="cell-anchored image"' in svg
    assert "example.invalid" not in svg


def test_converts_glb_mesh_scene(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "triangle.glb"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "gltf"
    assert report["page_count"] == 1
    assert any("materials, textures" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-source-format="gltf"' in svg


def test_converts_json_gltf_with_local_buffer(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "triangle.gltf"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "gltf"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert 'data-source-format="gltf"' in svg
    assert 'data-semantic-role="gltf:mesh"' in svg


def test_converts_open_document_presentation(tmp_path: Path) -> None:
    source = tmp_path / "slides.fodp"
    output = tmp_path / "out"
    write_minimal_fodp(source)

    report = convert(source, output)

    assert report["source_format"] == "odp"
    assert report["page_count"] == 1
    assert any("ODP text wrapping" in warning for warning in report["warnings"])
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "OpenDocument presentation from Python." in svg


def test_converts_package_linked_open_document_image(tmp_path: Path) -> None:
    source = Path(__file__).resolve().parents[3] / "tests" / "fixtures" / "odp_image.odp"
    output = tmp_path / "out"

    report = convert(source, output)

    assert report["source_format"] == "odp"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg
    assert "ODP embedded images" not in svg


def test_converts_open_document_drawing(tmp_path: Path) -> None:
    source = tmp_path / "diagram.fodg"
    output = tmp_path / "out"
    write_minimal_fodg(source)

    report = convert(source, output)
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")

    assert report["source_format"] == "odg"
    assert report["page_count"] == 1
    assert "OpenDocument drawing from Python." in svg


def test_converts_flat_open_document_drawing_inline_binary_image(tmp_path: Path) -> None:
    source = tmp_path / "inline.fodg"
    source.write_text(
        '<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
        'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
        'xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" '
        'xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:drawing>'
        '<draw:page draw:name="Inline"><draw:frame draw:name="image" svg:x="1cm" svg:y="1cm" '
        'svg:width="4cm" svg:height="2cm"><draw:image xlink:href="fallback.png">'
        '<office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data>'
        '</draw:image></draw:frame></draw:page></office:drawing></office:body></office:document-content>',
        encoding="utf-8",
    )
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "odg"
    svg = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "data:image/png;base64," in svg


def test_converts_rtf_document(tmp_path: Path) -> None:
    source = tmp_path / "memo.rtf"
    output = tmp_path / "out"
    write_minimal_rtf(source)

    report = convert(source, output)

    assert report["source_format"] == "rtf"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Résumé — RTF from Python" in svg


def test_converts_japanese_rtf_ansi_codepage(tmp_path: Path) -> None:
    source = tmp_path / "japanese.rtf"
    output = tmp_path / "out"
    write_minimal_rtf_cp932(source)

    report = convert(source, output)
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")

    assert report["source_format"] == "rtf"
    assert "あ" in svg
    assert not any("code page 932" in warning for warning in report["warnings"])


def test_missing_input_is_reported_as_os_error(tmp_path: Path) -> None:
    with pytest.raises(OSError, match="I/O error"):
        convert(tmp_path / "missing.pdf", tmp_path / "out")


def test_unsupported_extension_is_reported_as_value_error(tmp_path: Path) -> None:
    source = tmp_path / "input.bin"
    source.write_text("not a document", encoding="utf-8")

    with pytest.raises(ValueError, match="unsupported input"):
        convert(source, tmp_path / "out")


def test_plain_text_file_is_converted_as_a_markdown_document(tmp_path: Path) -> None:
    source = tmp_path / "notes.txt"
    output = tmp_path / "out"
    source.write_text("Release notes\n\nThis build contains plain text.", encoding="utf-8")

    report = convert(source, output)

    assert report["source_format"] == "markdown"
    assert report["page_count"] == 1
    svg = (output / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "This build contains plain text." in svg


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


def test_converts_cad_dxf(tmp_path: Path) -> None:
    source = tmp_path / "drawing.dxf"
    source.write_text(
        "0\nSECTION\n2\nENTITIES\n0\nLINE\n8\n0\n10\n0.0\n20\n0.0\n11\n100.0\n21\n50.0\n0\nENDSEC\n0\nEOF\n",
        encoding="utf-8",
    )

    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "dxf"
    assert report["page_count"] == 1
    assert (tmp_path / "out" / report["pages"][0]["svg"]).is_file()


def test_converts_and_previews_subrip_and_webvtt(tmp_path: Path) -> None:
    fixtures = [
        (
            "captions.srt",
            "srt",
            "1\n00:00:01,000 --> 00:00:02,500\nHello <b>world</b>.\n",
            ["00:00:01.000", "Hello world."],
        ),
        (
            "captions.vtt",
            "vtt",
            "WEBVTT\n\n00:01.000 --> 00:02.000\n<v Speaker>Welcome aboard.</v>\n",
            ["00:00:01.000", "Speaker: Welcome aboard."],
        ),
    ]
    for filename, source_format, content, expected in fixtures:
        source = tmp_path / filename
        source.write_text(content, encoding="utf-8")
        report = convert(source, tmp_path / f"{filename}-out")
        assert report["source_format"] == source_format
        assert report["page_count"] == 1
        page = (tmp_path / f"{filename}-out" / report["pages"][0]["svg"]).read_text(
            encoding="utf-8"
        )
        assert all(text in page for text in expected)


def test_converts_ttml_text_cues_without_loading_active_content(tmp_path: Path) -> None:
    source = tmp_path / "captions.ttml"
    source.write_text(
        '<tt xmlns="http://www.w3.org/ns/ttml"><head><layout><region xml:id="r"/></layout></head>'
        '<body><div><p xml:id="line-1" begin="00:00:01.000" dur="2s">'
        "Safe &lt;script&gt;text&lt;/script&gt;."
        "</p></div></body></tt>",
        encoding="utf-8",
    )
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "ttml"
    assert report["page_count"] == 1
    assert any("styles, regions" in warning for warning in report["warnings"])
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "00:00:01.000–00:00:03.000 [line-1]" in page
    assert "&lt;script&gt;text&lt;/script&gt;." in page
    assert "<script>" not in page


def test_converts_xliff_source_and_target_segments(tmp_path: Path) -> None:
    source = tmp_path / "messages.xliff"
    source.write_text(
        '<xliff xmlns="urn:oasis:names:tc:xliff:document:2.0" version="2.0" srcLang="en" trgLang="fr">'
        '<file original="menu.xml"><unit id="open"><segment state="translated">'
        '<source>Open <ph id="1"/></source><target>Ouvrir <ph id="1"/></target>'
        "</segment></unit></file></xliff>",
        encoding="utf-8",
    )
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "xliff"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Languages: en → fr" in page
    assert "Source (en): Open [ph:1]" in page
    assert "Segment 1 [translated]" in page
    assert "Target (fr): Ouvrir [ph:1]" in page


def test_converts_an_ideas_unv_mesh(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.unv"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "unv"
    assert report["page_count"] == 1
    assert any("nonzero Z" in warning for warning in report["warnings"])
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "UNV Universal FEA mesh" in page


def test_converts_a_tecplot_ascii_mesh(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_tecplot.dat"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "tecplot"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Tecplot finite-element sample" in page


def test_converts_an_ensight_gold_case(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_ensight.case"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "ensight"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "EnSight: EnSight Gold ASCII sample" in page


def test_converts_a_plot3d_ascii_grid(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_plot3d.p3d"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "plot3d"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "PLOT3D structured grid" in page


def test_converts_a_vrml97_indexed_mesh(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.wrl"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "vrml"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Wavefront OBJ 3D Model" in page


def test_converts_netpbm_ppm_raster(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_rgb.ppm"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "raster"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "vectorized_path" in page


def test_converts_sylk_spreadsheet(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.slk"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "sylk"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "SYLK spreadsheet" in page


def test_converts_dif_spreadsheet(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.dif"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "dif"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "DIF spreadsheet" in page


def test_converts_fasta_and_fastq_records(tmp_path: Path) -> None:
    fixture_root = Path(__file__).parents[3] / "tests" / "fixtures"
    for filename, source_format in (("sample.fasta", "fasta"), ("sample.fastq", "fastq")):
        report = convert(fixture_root / filename, tmp_path / source_format)
        assert report["source_format"] == source_format
        assert report["page_count"] == 1
        page = (tmp_path / source_format / report["pages"][0]["svg"]).read_text(encoding="utf-8")
        assert f"{source_format.upper()} sequence records" in page


def test_converts_gff3_and_gtf_annotations(tmp_path: Path) -> None:
    fixture_root = Path(__file__).parents[3] / "tests" / "fixtures"
    for filename, source_format in (("sample.gff3", "gff3"), ("sample.gtf", "gtf")):
        report = convert(fixture_root / filename, tmp_path / source_format)
        assert report["source_format"] == source_format
        assert report["page_count"] == 1
        page = (tmp_path / source_format / report["pages"][0]["svg"]).read_text(encoding="utf-8")
        assert f"{source_format.upper()} feature annotations" in page


def test_converts_bed_and_bedgraph_intervals(tmp_path: Path) -> None:
    fixture_root = Path(__file__).parents[3] / "tests" / "fixtures"
    for filename, source_format in (("sample.bed", "bed"), ("sample.bedgraph", "bedgraph")):
        report = convert(fixture_root / filename, tmp_path / source_format)
        assert report["source_format"] == source_format
        assert report["page_count"] == 1
        page = (tmp_path / source_format / report["pages"][0]["svg"]).read_text(encoding="utf-8")
        assert f"{source_format.upper()} interval annotations" in page


def test_converts_vcf_variants(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_variants.vcf"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "vcf"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "VCF variant annotations" in page


def test_converts_sam_alignment_records(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.sam"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "sam"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "SAM alignment records" in page


def test_converts_wig_signals(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.wig"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "wig"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "WIG continuous signal" in page


def test_converts_maf_alignment_blocks(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.maf"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "maf"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "MAF multiple alignments" in page


def test_converts_newick_phylogenetic_tree(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.nwk"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "newick"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Newick phylogenetic tree" in page
    assert "Homo_sapiens" in page


def test_converts_stockholm_multiple_alignments(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.sto"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "stockholm"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Stockholm multiple alignments" in page
    assert "AC-GTT" in page


def test_converts_clustal_block_alignment(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.aln"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "clustal"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "CLUSTAL multiple alignments" in page
    assert "AC-GTT" in page


def test_converts_nexus_tree_block(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.nex"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "nexus"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "NEXUS phylogenetic tree" in page
    assert "Mammals" in page


def test_converts_genbank_records(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.gb"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "genbank"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "GenBank records" in page
    assert "DEMO0001" in page


def test_converts_embl_records(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.embl"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "embl"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "EMBL-Bank records" in page
    assert "DEMO0001" in page


def test_converts_uniprot_records(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_uniprot.dat"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "uniprot"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "UniProtKB protein records" in page
    assert "DEMO_HUMAN" in page


def test_converts_ris_bibliography(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.ris"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "ris"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "RIS bibliography" in page
    assert "Safe document conversion" in page


def test_converts_spice_netlist(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.cir"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "spice"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "SPICE netlist" in page
    assert "R1" in page


def test_converts_legacy_kicad_schematic(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_legacy.sch"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "kicad_sch_legacy"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "KiCad legacy schematic" in page
    assert "FILTERED_OUT" in page


def test_converts_modern_kicad_schematic(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.kicad_sch"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "kicad_sch"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "KiCad schematic" in page
    assert "FILTERED_OUT" in page


def test_converts_ltspice_ascii_schematic(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_ltspice.asc"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "ltspice_asc"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "LTspice schematic" in page
    assert "R1" in page


def test_converts_eagle_xml_schematic(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_eagle.sch"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "eagle_sch"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "EAGLE schematic" in page
    assert "R1" in page


def test_converts_openapi_json_and_yaml(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    report = convert(root / "sample.openapi.json", tmp_path / "json-out")
    assert report["source_format"] == "openapi"
    assert report["page_count"] == 1
    page = (tmp_path / "json-out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "OpenAPI description" in page
    assert "listPets" in page

    yaml_report = convert(root / "sample.openapi.yaml", tmp_path / "yaml-out")
    assert yaml_report["source_format"] == "openapi"
    assert yaml_report["page_count"] == 1
    yaml_page = (tmp_path / "yaml-out" / yaml_report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Catalog YAML API" in yaml_page
    assert "deletePet" in yaml_page


def test_converts_asyncapi_json_and_yaml(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    report = convert(root / "sample.asyncapi.json", tmp_path / "json-out")
    assert report["source_format"] == "asyncapi"
    assert report["page_count"] == 1
    page = (tmp_path / "json-out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "AsyncAPI description" in page
    assert "onUserSigned" in page

    yaml_report = convert(root / "sample.asyncapi.yaml", tmp_path / "yaml-out")
    assert yaml_report["source_format"] == "asyncapi"
    assert yaml_report["page_count"] == 1
    yaml_page = (tmp_path / "yaml-out" / yaml_report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Event Catalog YAML" in yaml_page
    assert "publishInvoice" in yaml_page


def test_converts_json_schema_json_and_yaml(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    report = convert(root / "sample.schema.json", tmp_path / "json-out")
    assert report["source_format"] == "jsonschema"
    assert report["page_count"] == 1
    page = (tmp_path / "json-out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "JSON Schema" in page
    assert "$.name" in page

    yaml_report = convert(root / "sample.schema.yaml", tmp_path / "yaml-out")
    assert yaml_report["source_format"] == "jsonschema"
    assert yaml_report["page_count"] == 1
    yaml_page = (tmp_path / "yaml-out" / yaml_report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Catalog YAML schema" in yaml_page
    assert "$.enabled" in yaml_page


def test_converts_ansys_cdb_mesh(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.cdb"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "cdb"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "ANSYS CDB mesh" in page


def test_converts_har_with_safe_redaction(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.har"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "har"
    assert report["page_count"] == 1
    assert any("never fetched" in warning for warning in report["warnings"])
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "HAR network archive" in page
    assert "very-secret" not in page


def test_converts_warc_records_and_gzip(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    report = convert(root / "sample.warc", tmp_path / "warc-out")
    assert report["source_format"] == "warc"
    assert report["page_count"] == 1
    page = (tmp_path / "warc-out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "WARC web archive" in page
    assert "200" in page
    assert "private body" not in page

    gzip_report = convert(root / "sample.warc.gz", tmp_path / "warc-gzip-out")
    assert gzip_report["source_format"] == "warc"
    assert gzip_report["page_count"] == 1


def test_converts_wacz_manifest_and_pages(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.wacz"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "wacz"
    assert report["page_count"] == 1
    assert any("payloads" in warning for warning in report["warnings"])
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "WACZ web archive" in page


def test_converts_postman_collection_with_safe_redaction(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.postman_collection.json"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "postman"
    assert report["page_count"] == 1
    assert any("executed" in warning for warning in report["warnings"])
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Postman collection" in page
    assert "very-secret" not in page


def test_converts_graphql_sdl(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.graphql"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "graphql"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "GraphQL schema" in page
    assert "Query" in page


def test_converts_protobuf_schema(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.proto"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "protobuf"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Protocol Buffers schema" in page
    assert "GetPet" in page


def test_converts_kubernetes_yaml_and_json_manifests(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    report = convert(root / "sample.k8s.yaml", tmp_path / "yaml-out")
    assert report["source_format"] == "kubernetes"
    assert report["page_count"] == 1
    assert any("kubectl" in warning for warning in report["warnings"])
    page = (tmp_path / "yaml-out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "Kubernetes manifests" in page
    assert "very-secret" not in page

    json_report = convert(root / "sample.k8s.json", tmp_path / "json-out")
    assert json_report["source_format"] == "kubernetes"
    assert json_report["page_count"] == 1


def test_previews_general_json_as_safe_path_value_rows(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.json"
    report = convert(source, tmp_path / "out")
    assert report["source_format"] == "json"
    assert report["page_count"] == 1
    page = (tmp_path / "out" / report["pages"][0]["svg"]).read_text(encoding="utf-8")
    assert "$.service.enabled (boolean): true" in page
    assert "$.service.ports[1] (number): 8081" in page
    assert "&lt;script&gt;alert(1)&lt;/script&gt;" in page
    assert "<script>" not in page


def test_reverses_svg_to_cad_formats(tmp_path: Path) -> None:
    source = tmp_path / "page.svg"
    source.write_text(
        '<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">'
        '<line x1="10" y1="10" x2="90" y2="90" stroke="black"/>'
        "</svg>",
        encoding="utf-8",
    )

    dxf_out = tmp_path / "drawing.dxf"
    rep_dxf = reverse(source, dxf_out)
    assert rep_dxf["output_format"] == "dxf"
    assert dxf_out.is_file()

    gcode_out = tmp_path / "toolpath.gcode"
    rep_gcode = reverse(source, gcode_out)
    assert rep_gcode["output_format"] == "gcode"
    assert gcode_out.is_file()

    gbr_out = tmp_path / "layer.gbr"
    rep_gbr = reverse(source, gbr_out)
    assert rep_gbr["output_format"] == "gerber"
    assert gbr_out.is_file()

    plt_out = tmp_path / "plot.plt"
    rep_plt = reverse(source, plt_out)
    assert rep_plt["output_format"] == "hpgl"
    assert plt_out.is_file()

    drl_out = tmp_path / "drill.drl"
    rep_drl = reverse(source, drl_out)
    assert rep_drl["output_format"] == "excellon"
    assert drl_out.is_file()

    stl_out = tmp_path / "model.stl"
    rep_stl = reverse(source, stl_out)
    assert rep_stl["output_format"] == "stl"
    assert stl_out.is_file()

    obj_out = tmp_path / "mesh.obj"
    rep_obj = reverse(source, obj_out)
    assert rep_obj["output_format"] == "obj"
    assert obj_out.is_file()

    step_out = tmp_path / "part.step"
    rep_step = reverse(source, step_out)
    assert rep_step["output_format"] == "step"
    assert step_out.is_file()

    msh_out = tmp_path / "mesh.msh"
    rep_msh = reverse(source, msh_out)
    assert rep_msh["output_format"] == "gmsh"
    assert msh_out.is_file()

    vtk_out = tmp_path / "field.vtk"
    rep_vtk = reverse(source, vtk_out)
    assert rep_vtk["output_format"] == "vtk"
    assert vtk_out.is_file()

    html_out = tmp_path / "page.html"
    rep_html = reverse(source, html_out)
    assert rep_html["output_format"] == "html"
    assert html_out.is_file()
    assert "<!DOCTYPE html>" in html_out.read_text(encoding="utf-8")

    webp_out = tmp_path / "page.webp"
    rep_webp = reverse(source, webp_out)
    assert rep_webp["output_format"] == "webp"
    assert webp_out.is_file()
    assert webp_out.read_bytes()[:4] == b"RIFF"


def test_python_preview_api(tmp_path: Path) -> None:
    source = tmp_path / "doc.pdf"
    write_minimal_pdf(source)

    rep = preview(source)
    assert rep["source_format"] == "pdf"
    assert rep["page_count"] == 1
    assert len(rep["pages"]) == 1
    assert "<svg" in rep["pages"][0]["svg"]
    assert rep["pages"][0]["number"] == 1
    assert not rep["needs_review"]


def test_python_transform_api() -> None:
    svg_in = (
        '<!-- Generator -->\n'
        '<svg width="100px" height="80px" viewBox="0 0 100 80" data-origin="test">\n'
        '  <metadata><author>Alice</author></metadata>\n'
        '  <desc>Sample diagram</desc>\n'
        '  <rect x="0" y="0" width="50.1234" height="40.5678" fill="#ff0000" stroke="#0000ff" stroke-width="1.2345" data-id="r1" />\n'
        '</svg>'
    )

    # 1. Minify + responsive
    res1 = transform(svg_in, minify=True, responsive=True)
    assert isinstance(res1, str)
    assert "<!-- Generator -->" not in res1
    assert 'width="100px"' not in res1
    assert "viewBox=" in res1

    # 2. Monochrome + precision + remove_metadata
    res2 = transform(
        svg_in,
        monochrome="#333333",
        precision=1,
        remove_metadata=True,
    )
    assert isinstance(res2, str)
    assert 'fill="#333333"' in res2
    assert 'stroke="#333333"' in res2
    assert 'width="50.1"' in res2
    assert "data-origin" not in res2
    assert "data-id" not in res2
    assert "Alice" not in res2
    assert "metadata" not in res2
    assert "desc" not in res2

    cleaned = transform(
        '<svg viewBox="0 0 20 20">'
        '<g id="empty-group"><g id="nested-empty"></g></g>'
        '<g id="content-group"><path d="M 0 0 L 10 10 L 10 10 Z Z" /></g>'
        '</svg>',
        clean_paths=True,
        strip_empty_groups=True,
    )
    assert "empty-group" not in cleaned
    assert "nested-empty" not in cleaned
    assert "content-group" in cleaned
    assert 'd="M 0 0 L 10 10 Z"' in cleaned

    # 3. Bytes roundtrip
    bytes_in = svg_in.encode("utf-8")
    res3 = transform(bytes_in, minify=True)
    assert isinstance(res3, bytes)
    assert b"<!-- Generator -->" not in res3


def test_converts_compose_yaml_and_json_safely(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    report = convert(root / "sample.compose.yaml", tmp_path / "yaml-out")
    assert report["source_format"] == "compose"
    assert report["page_count"] == 1
    json_report = convert(root / "sample.compose.json", tmp_path / "json-out")
    assert json_report["source_format"] == "compose"
    assert json_report["page_count"] == 1
    assert any("daemon" in warning for warning in report["warnings"])


def test_converts_github_actions_workflow_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.github.workflow.yml"
    report = convert(source, tmp_path / "workflow-out")
    assert report["source_format"] == "github-actions"
    assert report["page_count"] == 1
    assert any("never" in warning for warning in report["warnings"])


def test_converts_junit_xml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.junit.xml"
    report = convert(source, tmp_path / "junit-out")
    assert report["source_format"] == "junit"
    assert report["page_count"] == 1
    assert any("never" in warning for warning in report["warnings"])


def test_converts_sarif_results_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.sarif"
    report = convert(source, tmp_path / "sarif-out")
    assert report["source_format"] == "sarif"
    assert report["page_count"] == 1
    assert any("never" in warning for warning in report["warnings"])


def test_converts_terraform_plan_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.tfplan.json"
    report = convert(source, tmp_path / "terraform-plan-out")
    assert report["source_format"] == "terraform-plan"
    assert report["page_count"] == 1
    assert any("never" in warning for warning in report["warnings"])


def test_converts_cyclonedx_json_and_xml_safely(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    report = convert(root / "sample.cdx.json", tmp_path / "cdx-json-out")
    assert report["source_format"] == "cyclonedx"
    assert report["page_count"] == 1
    xml_report = convert(root / "sample.cdx.xml", tmp_path / "cdx-xml-out")
    assert xml_report["source_format"] == "cyclonedx"
    assert xml_report["page_count"] == 1


def test_converts_spdx_json_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.spdx.json"
    report = convert(source, tmp_path / "spdx-out")
    assert report["source_format"] == "spdx"
    assert report["page_count"] == 1
    tag_report = convert(source.with_suffix(""), tmp_path / "spdx-tag-out")
    assert tag_report["source_format"] == "spdx"
    assert tag_report["page_count"] == 1


def test_converts_coverage_xml_safely(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    report = convert(root / "sample.jacoco.xml", tmp_path / "jacoco-out")
    assert report["source_format"] == "coverage"
    assert report["page_count"] == 1
    cobertura_report = convert(root / "sample.cobertura.xml", tmp_path / "cobertura-out")
    assert cobertura_report["source_format"] == "coverage"
    assert cobertura_report["page_count"] == 1


def test_converts_lcov_tracefile_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.lcov.info"
    report = convert(source, tmp_path / "lcov-out")
    assert report["source_format"] == "lcov"
    assert report["page_count"] == 1


def test_converts_json_patch_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.jsonpatch"
    report = convert(source, tmp_path / "jsonpatch-out")
    assert report["source_format"] == "jsonpatch"
    assert report["page_count"] == 1


def test_converts_json_merge_patch_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.mergepatch"
    report = convert(source, tmp_path / "mergepatch-out")
    assert report["source_format"] == "jsonmergepatch"
    assert report["page_count"] == 1


def test_converts_openfoam_field_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.foamfield"
    report = convert(source, tmp_path / "foam-field-out")
    assert report["source_format"] == "openfoam-field"
    assert report["page_count"] == 1


def test_converts_csl_json_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.csl.json"
    report = convert(source, tmp_path / "csl-out")
    assert report["source_format"] == "csl-json"
    assert report["page_count"] == 1


def test_converts_json_feed_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.jsonfeed"
    report = convert(source, tmp_path / "jsonfeed-out")
    assert report["source_format"] == "jsonfeed"
    assert report["page_count"] == 1


def test_converts_cloud_events_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.cloudevent.json"
    report = convert(source, tmp_path / "cloudevents-out")
    assert report["source_format"] == "cloudevents"
    assert report["page_count"] == 1


def test_converts_fhir_json_bundle_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.fhir.json"
    report = convert(source, tmp_path / "fhir-out")
    assert report["source_format"] == "fhir-json"
    assert report["page_count"] == 1


def test_converts_avro_schema_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.avsc"
    report = convert(source, tmp_path / "avro-out")
    assert report["source_format"] == "avro"
    assert report["page_count"] == 1


def test_converts_otlp_json_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.otlp.json"
    report = convert(source, tmp_path / "otlp-out")
    assert report["source_format"] == "otlp-json"
    assert report["page_count"] == 1


def test_converts_ocel_json_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.jsonocel"
    report = convert(source, tmp_path / "ocel-out")
    assert report["source_format"] == "ocel-json"
    assert report["page_count"] == 1


def test_converts_json_api_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.jsonapi"
    report = convert(source, tmp_path / "jsonapi-out")
    assert report["source_format"] == "json-api"
    assert report["page_count"] == 1


def test_converts_opendrive_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xodr"
    report = convert(source, tmp_path / "opendrive-out")
    assert report["source_format"] == "opendrive"
    assert report["page_count"] == 1


def test_converts_openscenario_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xosc"
    report = convert(source, tmp_path / "openscenario-out")
    assert report["source_format"] == "openscenario"
    assert report["page_count"] == 1


def test_converts_openlabel_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.openlabel.json"
    report = convert(source, tmp_path / "openlabel-out")
    assert report["source_format"] == "openlabel"
    assert report["page_count"] == 1


def test_converts_citygml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.citygml"
    report = convert(source, tmp_path / "citygml-out")
    assert report["source_format"] == "citygml"
    assert report["page_count"] == 1


def test_converts_cityjson_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.cityjson"
    report = convert(source, tmp_path / "cityjson-out")
    assert report["source_format"] == "cityjson"
    assert report["page_count"] == 1


def test_converts_stix_json_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.stix.json"
    report = convert(source, tmp_path / "stix-out")
    assert report["source_format"] == "stix-json"
    assert report["page_count"] == 1


def test_converts_opencrg_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.crg"
    report = convert(source, tmp_path / "opencrg-out")
    assert report["source_format"] == "opencrg"
    assert report["page_count"] == 1


def test_converts_taxii_json_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.taxii.json"
    report = convert(source, tmp_path / "taxii-out")
    assert report["source_format"] == "taxii-json"
    assert report["page_count"] == 1


def test_converts_wsdl_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.wsdl"
    report = convert(source, tmp_path / "wsdl-out")
    assert report["source_format"] == "wsdl"
    assert report["page_count"] == 1


def test_converts_opml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.opml"
    report = convert(source, tmp_path / "opml-out")
    assert report["source_format"] == "opml"
    assert report["page_count"] == 1


def test_converts_rss_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.rss"
    report = convert(source, tmp_path / "rss-out")
    assert report["source_format"] == "feed"
    assert report["page_count"] == 1


def test_converts_xml_plist_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.plist"
    report = convert(source, tmp_path / "plist-out")
    assert report["source_format"] == "plist"
    assert report["page_count"] == 1


def test_converts_tei_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.tei"
    report = convert(source, tmp_path / "tei-out")
    assert report["source_format"] == "tei"
    assert report["page_count"] == 1


def test_converts_alto_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.alto"
    report = convert(source, tmp_path / "alto-out")
    assert report["source_format"] == "alto"
    assert report["page_count"] == 1


def test_converts_mets_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.mets"
    report = convert(source, tmp_path / "mets-out")
    assert report["source_format"] == "mets"
    assert report["page_count"] == 1


def test_converts_marcxml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.marcxml"
    report = convert(source, tmp_path / "marcxml-out")
    assert report["source_format"] == "marcxml"
    assert report["page_count"] == 1


def test_converts_mods_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.mods"
    report = convert(source, tmp_path / "mods-out")
    assert report["source_format"] == "mods"
    assert report["page_count"] == 1


def test_converts_premis_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.premis"
    report = convert(source, tmp_path / "premis-out")
    assert report["source_format"] == "premis"
    assert report["page_count"] == 1


def test_converts_iiif_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.iiif.json"
    report = convert(source, tmp_path / "iiif-out")
    assert report["source_format"] == "iiif"
    assert report["page_count"] == 1


def test_converts_ead_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.ead"
    report = convert(source, tmp_path / "ead-out")
    assert report["source_format"] == "ead"
    assert report["page_count"] == 1


def test_converts_eac_cpf_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.eac-cpf"
    report = convert(source, tmp_path / "eac-out")
    assert report["source_format"] == "eac-cpf"
    assert report["page_count"] == 1


def test_converts_dublin_core_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.dc.xml"
    report = convert(source, tmp_path / "dc-out")
    assert report["source_format"] == "dublin-core"
    assert report["page_count"] == 1


def test_converts_s1000d_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.s1000d"
    report = convert(source, tmp_path / "s1000d-out")
    assert report["source_format"] == "s1000d"
    assert report["page_count"] == 1


def test_converts_dicom_structured_report_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample_sr.dcm"
    report = convert(source, tmp_path / "dicom-sr-out")
    assert report["source_format"] == "dicom-sr"
    assert report["page_count"] == 1


def test_converts_spreadsheetml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.spreadsheetml"
    report = convert(source, tmp_path / "xmlss-out")
    assert report["source_format"] == "spreadsheetml"
    assert report["page_count"] == 1


def test_converts_rdfxml_graph_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.rdf"
    report = convert(source, tmp_path / "rdfxml-out")
    assert report["source_format"] == "rdf-xml"
    assert report["page_count"] == 1


def test_converts_bcfzip_issue_package_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.bcfzip"
    report = convert(source, tmp_path / "bcf-out")
    assert report["source_format"] == "bcfzip"
    assert report["page_count"] == 1


def test_converts_flat_opc_word_package_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.flatopc"
    report = convert(source, tmp_path / "flat-opc-out")
    assert report["source_format"] == "flat-opc"
    assert report["page_count"] == 1


def test_converts_aasx_asset_package_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.aasx"
    report = convert(source, tmp_path / "aasx-out")
    assert report["source_format"] == "aasx"
    assert report["page_count"] == 1


def test_previews_openscad_source_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.scad"
    report = convert(source, tmp_path / "openscad-out")
    assert report["source_format"] == "openscad"
    assert report["page_count"] == 1


def test_converts_amf_mesh_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.amf"
    report = convert(source, tmp_path / "amf-out")
    assert report["source_format"] == "amf"
    assert report["page_count"] == 1


def test_converts_plmxml_product_structure_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.plmxml"
    report = convert(source, tmp_path / "plmxml-out")
    assert report["source_format"] == "plmxml"
    assert report["page_count"] == 1


def test_converts_step_xml_product_data_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.stepxml"
    report = convert(source, tmp_path / "stepxml-out")
    assert report["source_format"] == "stepxml"
    assert report["page_count"] == 1


def test_converts_qif_inspection_data_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.qif"
    report = convert(source, tmp_path / "qif-out")
    assert report["source_format"] == "qif"
    assert report["page_count"] == 1


def test_converts_b2mml_manufacturing_data_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.b2mml"
    report = convert(source, tmp_path / "b2mml-out")
    assert report["source_format"] == "b2mml"
    assert report["page_count"] == 1


def test_converts_jdf_job_ticket_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.jdf"
    report = convert(source, tmp_path / "jdf-out")
    assert report["source_format"] == "jdf"
    assert report["page_count"] == 1


def test_converts_xjdf_job_ticket_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xjdf"
    report = convert(source, tmp_path / "xjdf-out")
    assert report["source_format"] == "xjdf"
    assert report["page_count"] == 1


def test_converts_cml_chemical_document_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.cml"
    report = convert(source, tmp_path / "cml-out")
    assert report["source_format"] == "cml"
    assert report["page_count"] == 1


def test_converts_xdp_package_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xdp"
    report = convert(source, tmp_path / "xdp-out")
    assert report["source_format"] == "xdp"
    assert report["page_count"] == 1


def test_converts_xmp_metadata_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xmp"
    report = convert(source, tmp_path / "xmp-out")
    assert report["source_format"] == "xmp"
    assert report["page_count"] == 1


def test_converts_mathml_formula_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.mathml"
    report = convert(source, tmp_path / "mathml-out")
    assert report["source_format"] == "mathml"
    assert report["page_count"] == 1


def test_converts_landxml_civil_model_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.landxml"
    report = convert(source, tmp_path / "landxml-out")
    assert report["source_format"] == "landxml"
    assert report["page_count"] == 1


def test_converts_xfdf_form_data_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xfdf"
    report = convert(source, tmp_path / "xfdf-out")
    assert report["source_format"] == "xfdf"
    assert report["page_count"] == 1


def test_converts_fdf_form_data_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.fdf"
    report = convert(source, tmp_path / "fdf-out")
    assert report["source_format"] == "fdf"
    assert report["page_count"] == 1


def test_converts_xbrl_instance_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xbrl.xml"
    report = convert(source, tmp_path / "xbrl-out")
    assert report["source_format"] == "xbrl"
    assert report["page_count"] == 1


def test_converts_ubl_business_document_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.ubl.xml"
    report = convert(source, tmp_path / "ubl-out")
    assert report["source_format"] == "ubl"
    assert report["page_count"] == 1


def test_converts_iso19115_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.iso19115.xml"
    report = convert(source, tmp_path / "iso19115-out")
    assert report["source_format"] == "iso19115"
    assert report["page_count"] == 1


def test_converts_marc21_iso2709_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.marc"
    report = convert(source, tmp_path / "marc-out")
    assert report["source_format"] == "marc21"
    assert report["page_count"] == 1


def test_converts_tmx_translation_memory_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.tmx"
    report = convert(source, tmp_path / "tmx-out")
    assert report["source_format"] == "tmx"
    assert report["page_count"] == 1


def test_converts_tbx_terminology_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.tbx"
    report = convert(source, tmp_path / "tbx-out")
    assert report["source_format"] == "tbx"
    assert report["page_count"] == 1


def test_converts_gbxml_building_model_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.gbxml"
    report = convert(source, tmp_path / "gbxml-out")
    assert report["source_format"] == "gbxml"
    assert report["page_count"] == 1


def test_converts_fhir_xml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.fhir.xml"
    report = convert(source, tmp_path / "fhir-xml-out")
    assert report["source_format"] == "fhir-xml"
    assert report["page_count"] == 1


def test_converts_idml_package_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.idml"
    report = convert(source, tmp_path / "idml-out")
    assert report["source_format"] == "idml"
    assert report["page_count"] == 1


def test_converts_xpdl_workflow_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xpdl"
    report = convert(source, tmp_path / "xpdl-out")
    assert report["source_format"] == "xpdl"
    assert report["page_count"] == 1


def test_converts_onix_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.onix"
    report = convert(source, tmp_path / "onix-out")
    assert report["source_format"] == "onix"
    assert report["page_count"] == 1


def test_converts_oai_pmh_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.oaipmh"
    report = convert(source, tmp_path / "oaipmh-out")
    assert report["source_format"] == "oaipmh"
    assert report["page_count"] == 1


def test_converts_cda_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.cda"
    report = convert(source, tmp_path / "cda-out")
    assert report["source_format"] == "cda"
    assert report["page_count"] == 1


def test_converts_iso20022_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.iso20022.xml"
    report = convert(source, tmp_path / "iso20022-out")
    assert report["source_format"] == "iso20022"
    assert report["page_count"] == 1


def test_converts_sbml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.sbml"
    report = convert(source, tmp_path / "sbml-out")
    assert report["source_format"] == "sbml"
    assert report["page_count"] == 1


def test_converts_cellml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.cellml"
    report = convert(source, tmp_path / "cellml-out")
    assert report["source_format"] == "cellml"
    assert report["page_count"] == 1


def test_converts_ocel_xml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xmlocel"
    report = convert(source, tmp_path / "ocel-xml-out")
    assert report["source_format"] == "ocel-xml"
    assert report["page_count"] == 1


def test_converts_energyplus_idf_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.idf"
    report = convert(source, tmp_path / "idf-out")
    assert report["source_format"] == "energyplus-idf"
    assert report["page_count"] == 1


def test_converts_energyplus_epw_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.epw"
    report = convert(source, tmp_path / "epw-out")
    assert report["source_format"] == "energyplus-epw"
    assert report["page_count"] == 1


def test_converts_rinex_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.rnx"
    report = convert(source, tmp_path / "rinex-out")
    assert report["source_format"] == "rinex"
    assert report["page_count"] == 1


def test_converts_acis_sat_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.sat"
    report = convert(source, tmp_path / "sat-out")
    assert report["source_format"] == "sat"
    assert report["page_count"] == 1


def test_converts_sedml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.sedml"
    report = convert(source, tmp_path / "sedml-out")
    assert report["source_format"] == "sedml"
    assert report["page_count"] == 1


def test_converts_sbgnml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.sbgnml"
    report = convert(source, tmp_path / "sbgnml-out")
    assert report["source_format"] == "sbgnml"
    assert report["page_count"] == 1


def test_converts_omex_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.omex"
    report = convert(source, tmp_path / "omex-out")
    assert report["source_format"] == "omex"
    assert report["page_count"] == 1


def test_converts_xdmf_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xdmf"
    report = convert(source, tmp_path / "xdmf-out")
    assert report["source_format"] == "xdmf"
    assert report["page_count"] == 1


def test_converts_pvd_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.pvd"
    report = convert(source, tmp_path / "pvd-out")
    assert report["source_format"] == "pvd"
    assert report["page_count"] == 1


def test_converts_fds_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.fds"
    report = convert(source, tmp_path / "fds-out")
    assert report["source_format"] == "fds"
    assert report["page_count"] == 1


def test_converts_abiword_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.abw"
    report = convert(source, tmp_path / "abw-out")
    assert report["source_format"] == "abiword"
    assert report["page_count"] == 1


def test_converts_neuroml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.nml"
    report = convert(source, tmp_path / "neuroml-out")
    assert report["source_format"] == "neuroml"
    assert report["page_count"] == 1


def test_converts_biopax_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.biopax.xml"
    report = convert(source, tmp_path / "biopax-out")
    assert report["source_format"] == "biopax"
    assert report["page_count"] == 1


def test_converts_xsd_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xsd"
    report = convert(source, tmp_path / "xsd-out")
    assert report["source_format"] == "xsd"
    assert report["page_count"] == 1


def test_converts_xslt_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xsl"
    report = convert(source, tmp_path / "xslt-out")
    assert report["source_format"] == "xslt"
    assert report["page_count"] == 1


def test_converts_xsl_fo_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.fo"
    report = convert(source, tmp_path / "xslfo-out")
    assert report["source_format"] == "xsl-fo"
    assert report["page_count"] == 1


def test_converts_xproc_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xproc"
    report = convert(source, tmp_path / "xproc-out")
    assert report["source_format"] == "xproc"
    assert report["page_count"] == 1


def test_converts_wadl_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.wadl"
    report = convert(source, tmp_path / "wadl-out")
    assert report["source_format"] == "wadl"
    assert report["page_count"] == 1


def test_converts_opensearch_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.osdd"
    report = convert(source, tmp_path / "opensearch-out")
    assert report["source_format"] == "opensearch"
    assert report["page_count"] == 1


def test_converts_saml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.saml.xml"
    report = convert(source, tmp_path / "saml-out")
    assert report["source_format"] == "saml"
    assert report["page_count"] == 1


def test_converts_xacml_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.xacml"
    report = convert(source, tmp_path / "xacml-out")
    assert report["source_format"] == "xacml"
    assert report["page_count"] == 1


def test_converts_legacy_openoffice_packages_safely(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    expected = {"sxw": "odt", "sxc": "ods", "sxi": "odp"}
    for extension, source_format in expected.items():
        report = convert(root / f"sample.{extension}", tmp_path / f"{extension}-out")
        assert report["source_format"] == source_format
        assert report["page_count"] >= 1


def test_converts_legacy_visio_cfb_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.vsd"
    report = convert(source, tmp_path / "vsd-out")
    assert report["source_format"] == "vsd"
    assert report["page_count"] == 1


def test_converts_hdf5_and_cgns_superblocks_safely(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    for extension, source_format in (("h5", "hdf5"), ("cgns", "cgns"), ("exo", "exodus")):
        report = convert(root / f"sample.{extension}", tmp_path / f"{extension}-out")
        assert report["source_format"] == source_format
        assert report["page_count"] == 1


def test_converts_iwork_packages_safely(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    for extension in ("pages", "numbers", "key"):
        report = convert(root / f"sample.{extension}", tmp_path / f"{extension}-out")
        assert report["source_format"] == "iwork"
        assert report["page_count"] == 1


def test_converts_dwg_header_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.dwg"
    report = convert(source, tmp_path / "dwg-out")
    assert report["source_format"] == "dwg"
    assert report["page_count"] == 1


def test_converts_rhino_3dm_marker_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.3dm"
    report = convert(source, tmp_path / "3dm-out")
    assert report["source_format"] == "3dm"
    assert report["page_count"] == 1


def test_converts_access_ace_and_jet_headers_safely(tmp_path: Path) -> None:
    root = Path(__file__).parents[3] / "tests" / "fixtures"
    for extension in ("accdb", "mdb"):
        report = convert(root / f"sample.{extension}", tmp_path / f"{extension}-out")
        assert report["source_format"] == "access"
        assert report["page_count"] == 1


def test_converts_3dxml_product_structure_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.3dxml"
    report = convert(source, tmp_path / "3dxml-out")
    assert report["source_format"] == "3dxml"
    assert report["page_count"] == 1


def test_converts_dwfx_fixed_page_alias_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.dwfx"
    report = convert(source, tmp_path / "dwfx-out")
    assert report["source_format"] == "xps"
    assert report["page_count"] == 1


def test_preflights_nastran_op2_records_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.op2"
    report = convert(source, tmp_path / "op2-out")
    assert report["source_format"] == "op2"
    assert report["page_count"] == 1


def test_previews_ipc2581_pcb_exchange_structure_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.ipc2581"
    report = convert(source, tmp_path / "ipc2581-out")
    assert report["source_format"] == "ipc2581"
    assert report["page_count"] == 1


def test_previews_siemens_jt_header_safely(tmp_path: Path) -> None:
    source = Path(__file__).parents[3] / "tests" / "fixtures" / "sample.jt"
    report = convert(source, tmp_path / "jt-out")
    assert report["source_format"] == "jt"
    assert report["page_count"] == 1
