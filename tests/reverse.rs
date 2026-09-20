use std::fs;
use std::io::Read;

use base64::Engine;
use document_svg::{
    ConvertOptions, OpenXmlFormat, ReverseFormat, ReverseOptions, convert_path, svg_to_document,
    svg_to_openxml,
};
use tempfile::TempDir;
use zip::ZipArchive;

#[test]
fn packages_svg_directory_as_pptx_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let input = make_svg_pages(&temporary);
    let output = temporary.path().join("pages.pptx");

    let report = svg_to_openxml(&input, &output, &ReverseOptions::default()).unwrap();

    assert_eq!(report.output_format, OpenXmlFormat::Pptx);
    assert_eq!(report.page_count, 2);
    assert_eq!(read_zip_part(&output, "ppt/media/image1.svg"), svg_one());
    assert_eq!(read_zip_part(&output, "ppt/media/image2.svg"), svg_two());
    assert!(
        read_zip_bytes(&output, "ppt/media/image1-fallback.png").starts_with(b"\x89PNG\r\n\x1a\n")
    );
    let slide = read_zip_part(&output, "ppt/slides/slide1.xml");
    assert!(slide.contains("<mc:AlternateContent>"));
    assert!(slide.contains("<asvg:svgBlip r:embed=\"rId2\"/>"));

    let round_trip = temporary.path().join("pptx-svg");
    let converted = convert_path(&output, &round_trip, &ConvertOptions::default()).unwrap();
    assert_eq!(converted.page_count, 2);
}

#[test]
fn packages_svg_directory_as_docx_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let input = make_svg_pages(&temporary);
    let output = temporary.path().join("pages.docx");

    let report = svg_to_openxml(&input, &output, &ReverseOptions::default()).unwrap();

    assert_eq!(report.output_format, OpenXmlFormat::Docx);
    assert_eq!(report.page_count, 2);
    assert_eq!(read_zip_part(&output, "word/media/image1.svg"), svg_one());
    assert_eq!(read_zip_part(&output, "word/media/image2.svg"), svg_two());
    assert!(
        read_zip_bytes(&output, "word/media/image1-fallback.png").starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert!(read_zip_part(&output, "word/document.xml").contains("<asvg:svgBlip"));

    let round_trip = temporary.path().join("docx-svg");
    let converted = convert_path(&output, &round_trip, &ConvertOptions::default()).unwrap();
    assert_eq!(converted.page_count, 2);
}

#[test]
fn packages_mixed_orientation_svg_pages_as_docx_without_blank_pages() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mixed-pages");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("page-0001.svg"), svg_one()).unwrap();
    fs::write(input.join("page-0002.svg"), svg_landscape()).unwrap();
    let output = temporary.path().join("mixed.docx");

    let report = svg_to_openxml(&input, &output, &ReverseOptions::default()).unwrap();

    assert_eq!(report.page_count, 2);
    let document = read_zip_part(&output, "word/document.xml");
    assert!(document.contains("w:orient=\"landscape\""));
    assert_eq!(document.matches("<w:sectPr>").count(), 2);
    let round_trip = temporary.path().join("mixed-svg");
    let converted = convert_path(&output, &round_trip, &ConvertOptions::default()).unwrap();
    assert_eq!(converted.page_count, 2);
    assert_eq!(converted.pages[0].width_points, 612.0);
    assert_eq!(converted.pages[0].height_points, 792.0);
    assert_eq!(converted.pages[1].width_points, 792.0);
    assert_eq!(converted.pages[1].height_points, 612.0);
}

#[test]
fn packages_svg_directory_as_xlsx_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let input = make_svg_pages(&temporary);
    let output = temporary.path().join("pages.xlsx");

    let report = svg_to_openxml(&input, &output, &ReverseOptions::default()).unwrap();

    assert_eq!(report.output_format, OpenXmlFormat::Xlsx);
    assert_eq!(report.page_count, 2);
    assert_eq!(read_zip_part(&output, "xl/media/image1.svg"), svg_one());
    assert_eq!(read_zip_part(&output, "xl/media/image2.svg"), svg_two());
    assert!(
        read_zip_bytes(&output, "xl/media/image1-fallback.png").starts_with(b"\x89PNG\r\n\x1a\n")
    );
    assert!(read_zip_part(&output, "xl/drawings/drawing1.xml").contains("<asvg:svgBlip"));
    assert!(read_zip_part(&output, "xl/worksheets/sheet1.xml").contains("fitToPage=\"1\""));

    let round_trip = temporary.path().join("xlsx-svg");
    let converted = convert_path(&output, &round_trip, &ConvertOptions::default()).unwrap();
    assert_eq!(converted.page_count, 2);
}

#[test]
fn accepts_single_svg_and_refuses_to_overwrite_output() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("page.svg");
    let output = temporary.path().join("page.pptx");
    fs::write(&input, svg_one()).unwrap();

    let report = svg_to_openxml(&input, &output, &ReverseOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);

    let error = svg_to_openxml(&input, &output, &ReverseOptions::default()).unwrap_err();
    assert!(error.to_string().contains("output already exists"));
}

#[test]
fn produces_deterministic_packages() {
    let temporary = TempDir::new().unwrap();
    let input = make_svg_pages(&temporary);
    let first = temporary.path().join("first.pptx");
    let second = temporary.path().join("second.pptx");

    svg_to_openxml(&input, &first, &ReverseOptions::default()).unwrap();
    svg_to_openxml(&input, &second, &ReverseOptions::default()).unwrap();

    assert_eq!(fs::read(first).unwrap(), fs::read(second).unwrap());
}

#[test]
fn rejects_active_and_external_svg_content() {
    let temporary = TempDir::new().unwrap();
    let samples = [
        (
            "css-escape",
            r#"<svg width="100" height="100"><style>rect{fill:u\72l(https://example.invalid/a)}</style></svg>"#,
        ),
        (
            "xml-base",
            r#"<svg width="100" height="100" xml:base="https://example.invalid/"/>"#,
        ),
        (
            "processing-instruction",
            r#"<?xml-stylesheet href="https://example.invalid/a.css"?><svg width="100" height="100"/>"#,
        ),
        (
            "script",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><script>alert(1)</script></svg>"#,
        ),
        (
            "event",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" onload="alert(1)"/>"#,
        ),
        (
            "external",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><image href="https://example.invalid/image.png"/></svg>"#,
        ),
        (
            "css-entity-split",
            // The reference and the text around it arrive as separate events;
            // only a check that joins them sees the url() this builds.
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><style>rect{fill:u&#114;l(https://example.invalid/a)}</style></svg>"#,
        ),
        (
            "css-entity-cdata",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><style><![CDATA[rect{fill:u]]>&#114;<![CDATA[l(https://example.invalid/a)}]]></style></svg>"#,
        ),
        (
            "foreign-object",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><foreignObject/></svg>"#,
        ),
    ];
    for (name, svg) in samples {
        let input = temporary.path().join(format!("{name}.svg"));
        let output = temporary.path().join(format!("{name}.pptx"));
        fs::write(&input, svg).unwrap();

        let error = svg_to_openxml(&input, &output, &ReverseOptions::default()).unwrap_err();

        assert!(error.to_string().contains("not allowed"), "{name}: {error}");
        assert!(!output.exists());
        assert!(!fs::read_dir(temporary.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "tmp")
        }));
    }
}

#[test]
fn accepts_scientific_notation_svg_dimensions() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("scientific.svg");
    let output = temporary.path().join("scientific.pptx");
    fs::write(
        &input,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="1e3" height="9e2"><rect width="1000" height="900" fill="white"/></svg>"#,
    )
    .unwrap();

    svg_to_openxml(&input, &output, &ReverseOptions::default()).unwrap();

    let presentation = read_zip_part(&output, "ppt/presentation.xml");
    assert!(presentation.contains("cx=\"9525000\""));
    assert!(presentation.contains("cy=\"8572500\""));
}

fn make_svg_pages(temporary: &TempDir) -> std::path::PathBuf {
    let input = temporary.path().join("pages");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("page-0002.svg"), svg_two()).unwrap();
    fs::write(input.join("page-0001.svg"), svg_one()).unwrap();
    input
}

fn svg_one() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="612pt" height="792pt" viewBox="0 0 612 792"><rect width="612" height="792" fill="white"/><text x="72" y="96">One</text></svg>"#
}

fn svg_two() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="8.5in" height="11in" viewBox="0 0 816 1056"><rect width="816" height="1056" fill="white"/><text x="96" y="128">Two</text></svg>"#
}

fn svg_landscape() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="792pt" height="612pt" viewBox="0 0 792 612"><rect width="792" height="612" fill="white"/><text x="72" y="96">Landscape</text></svg>"#
}

fn read_zip_part(path: &std::path::Path, name: &str) -> String {
    String::from_utf8(read_zip_bytes(path, name)).unwrap()
}

fn read_zip_bytes(path: &std::path::Path, name: &str) -> Vec<u8> {
    let file = fs::File::open(path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();
    let mut part = archive.by_name(name).unwrap();
    let mut bytes = Vec::new();
    part.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn packages_svg_pages_as_a_drawio_file_of_pictures() {
    let temporary = TempDir::new().unwrap();
    let input = make_svg_pages(&temporary);
    let output = temporary.path().join("pages.drawio");

    let report = svg_to_document(&input, &output, &ReverseOptions::default()).unwrap();

    assert_eq!(report.output_format, ReverseFormat::Drawio);
    assert_eq!(report.page_count, 2);
    assert!(
        report.warnings[0].contains("not editable shapes"),
        "{:?}",
        report.warnings
    );
    let file = fs::read_to_string(&output).unwrap();
    assert_eq!(file.matches("<diagram ").count(), 2);
    assert!(file.contains(r#"pages="2""#), "{file}");
    // 612 points is 816 draw.io pixels.
    assert!(file.contains(r#"width="816" height="1056""#), "{file}");
    assert!(file.contains("shape=image;"), "{file}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(svg_one());
    assert!(
        file.contains(&encoded),
        "the first page is embedded verbatim"
    );

    // draw.io can open it, and so can the converter that wrote it.
    let round_trip = temporary.path().join("drawio-svg");
    let converted = convert_path(&output, &round_trip, &ConvertOptions::default()).unwrap();
    assert_eq!(converted.page_count, 2);
    assert!(converted.warnings.is_empty(), "{:?}", converted.warnings);
    let page = fs::read_to_string(round_trip.join("page-0001.svg")).unwrap();
    assert!(page.contains("data:image/svg+xml;base64,"), "{page}");
}

#[test]
fn restores_the_diagram_a_drawio_svg_export_carries() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("export.svg");
    let output = temporary.path().join("restored.drawio");
    // What draw.io writes when "Include a copy of my diagram" is on: the
    // standard SVG 1.1 document type, HTML labels in foreignObject, and the
    // diagram itself in the root element's `content` attribute.
    fs::write(&input, r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg" width="120px" height="60px" content="&lt;mxfile host=&quot;app.diagrams.net&quot;&gt;&lt;diagram id=&quot;abc&quot; name=&quot;Page-1&quot;&gt;&lt;mxGraphModel&gt;&lt;root&gt;&lt;mxCell id=&quot;0&quot;/&gt;&lt;mxCell id=&quot;1&quot; parent=&quot;0&quot;/&gt;&lt;mxCell id=&quot;two&quot; value=&quot;Kept C:\path&quot; style=&quot;rounded=1;html=1;&quot; vertex=&quot;1&quot; parent=&quot;1&quot;&gt;&lt;mxGeometry x=&quot;0&quot; y=&quot;0&quot; width=&quot;120&quot; height=&quot;60&quot; as=&quot;geometry&quot;/&gt;&lt;/mxCell&gt;&lt;/root&gt;&lt;/mxGraphModel&gt;&lt;/diagram&gt;&lt;/mxfile&gt;"><foreignObject width="120" height="60"><div>Kept</div></foreignObject></svg>"##).unwrap();

    let report = svg_to_document(&input, &output, &ReverseOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert!(
        report.warnings[0].contains("restored"),
        "{:?}",
        report.warnings
    );
    let file = fs::read_to_string(&output).unwrap();
    assert!(
        file.contains(r#"<diagram id="abc" name="Page-1">"#),
        "{file}"
    );
    assert!(file.contains(r#"value="Kept C:\path""#), "{file}");
    assert!(
        !file.contains("foreignObject"),
        "the picture is dropped: {file}"
    );

    // The restored file is the diagram again, not an image of it.
    let pages = temporary.path().join("restored-svg");
    convert_path(&output, &pages, &ConvertOptions::default()).unwrap();
    let page = fs::read_to_string(pages.join("page-0001.svg")).unwrap();
    assert!(page.contains("</tspan>"), "{page}");
    assert!(!page.contains("<image"), "{page}");
}

#[test]
fn refuses_an_embedded_diagram_that_is_not_an_mxfile_or_declares_entities() {
    let temporary = TempDir::new().unwrap();
    for (name, content) in [
        ("not-a-diagram", "&lt;html&gt;&lt;/html&gt;"),
        (
            "entities",
            "&lt;!DOCTYPE mxfile [&lt;!ENTITY a \"b\"&gt;]&gt;&lt;mxfile&gt;&lt;diagram&gt;&lt;/diagram&gt;&lt;/mxfile&gt;",
        ),
    ] {
        let input = temporary.path().join(format!("{name}.svg"));
        let output = temporary.path().join(format!("{name}.drawio"));
        fs::write(
            &input,
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10" content="{content}"/>"#
            ),
        )
        .unwrap();

        let error = svg_to_document(&input, &output, &ReverseOptions::default()).unwrap_err();

        assert!(
            error.to_string().contains("embedded diagram source"),
            "{name}: {error}"
        );
        assert!(!output.exists());
    }
}

#[test]
fn accepts_the_plain_document_type_a_real_export_carries_but_not_its_entities() {
    let temporary = TempDir::new().unwrap();
    let plain = temporary.path().join("plain.svg");
    fs::write(
        &plain,
        concat!(
            "<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\"><rect width=\"100\" height=\"100\"/></svg>"
        ),
    )
    .unwrap();
    svg_to_document(
        &plain,
        temporary.path().join("plain.pptx"),
        &ReverseOptions::default(),
    )
    .unwrap();

    let declared = temporary.path().join("declared.svg");
    fs::write(
        &declared,
        concat!(
            "<!DOCTYPE svg [<!ENTITY payload SYSTEM \"file:///etc/passwd\">]>",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\"/>"
        ),
    )
    .unwrap();

    let error = svg_to_document(
        &declared,
        temporary.path().join("declared.pptx"),
        &ReverseOptions::default(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("not allowed"), "{error}");
}

/// draw.io releases before 2018 stored the embedded copy as
/// `encodeURIComponent` output rather than as the diagram itself, and those
/// exports are still in circulation: three of the seven SVG exports in the
/// project's own example repository are written that way.
#[test]
fn restores_a_diagram_from_a_uri_encoded_copy() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("old-export.svg");
    let output = temporary.path().join("old.drawio");
    let mxfile = concat!(
        "<mxfile userAgent=\"Mozilla/5.0\" version=\"7.6.5\">",
        "<diagram id=\"old\" name=\"Page-1\">",
        "<mxGraphModel><root><mxCell id=\"0\"/><mxCell id=\"1\" parent=\"0\"/>",
        "<mxCell id=\"box\" value=\"Old export\" style=\"rounded=0;html=1;\" vertex=\"1\" parent=\"1\">",
        "<mxGeometry x=\"0\" y=\"0\" width=\"160\" height=\"60\" as=\"geometry\"/>",
        "</mxCell></root></mxGraphModel></diagram></mxfile>"
    );
    let mut encoded = String::new();
    for byte in mxfile.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(byte) {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    fs::write(
        &input,
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="160px" height="60px" content="{encoded}"><rect width="160" height="60" fill="none"/></svg>"#
        ),
    )
    .unwrap();

    let report = svg_to_document(&input, &output, &ReverseOptions::default()).unwrap();

    assert!(
        report.warnings[0].contains("restored"),
        "{:?}",
        report.warnings
    );
    let diagram = fs::read_to_string(&output).unwrap();
    assert!(
        diagram.contains(r#"<diagram id="old" name="Page-1">"#),
        "{diagram}"
    );
    assert!(diagram.contains("Old export"), "{diagram}");
}

#[test]
fn reads_only_the_roots_diagram_copy_and_survives_a_malformed_one() {
    let temporary = TempDir::new().unwrap();
    // A `content` attribute on a child element is not where draw.io keeps the
    // copy, so the page is a picture rather than a restored diagram.
    let child = temporary.path().join("child.svg");
    fs::write(
        &child,
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">"#,
            r#"<rect width="100" height="100" content="&lt;mxfile&gt;&lt;diagram&gt;&lt;mxGraphModel/&gt;&lt;/diagram&gt;&lt;/mxfile&gt;"/></svg>"#
        ),
    )
    .unwrap();
    let output = temporary.path().join("child.drawio");

    let report = svg_to_document(&child, &output, &ReverseOptions::default()).unwrap();
    assert!(
        report.warnings[0].contains("not editable shapes"),
        "{:?}",
        report.warnings
    );

    // A copy whose diagram never closes is refused rather than half-copied.
    let broken = temporary.path().join("broken.svg");
    fs::write(
        &broken,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" content="&lt;mxfile&gt;&lt;diagram id=&quot;a&quot;&gt;&lt;mxGraphModel/&gt;&lt;/mxfile&gt;"/>"#,
    )
    .unwrap();

    let error = svg_to_document(
        &broken,
        temporary.path().join("broken.drawio"),
        &ReverseOptions::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("embedded diagram source"),
        "{error}"
    );

    // A diagram nested inside another is closed by its own end tag, not by the
    // first one the reader meets.
    let nested = temporary.path().join("nested.svg");
    fs::write(
        &nested,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" content="&lt;mxfile&gt;&lt;diagram id=&quot;outer&quot;&gt;&lt;mxGraphModel&gt;&lt;root&gt;&lt;diagram id=&quot;inner&quot;&gt;&lt;/diagram&gt;&lt;/root&gt;&lt;/mxGraphModel&gt;&lt;/diagram&gt;&lt;/mxfile&gt;"/>"#,
    )
    .unwrap();
    let nested_output = temporary.path().join("nested.drawio");

    svg_to_document(&nested, &nested_output, &ReverseOptions::default()).unwrap();

    let file = fs::read_to_string(&nested_output).unwrap();
    assert_eq!(file.matches("<diagram ").count(), 2, "{file}");
    assert!(file.contains(r#"pages="1""#), "{file}");
}

#[test]
fn packages_svg_as_dxf_and_round_trips() {
    use document_svg::{ConvertOptions, convert_path};

    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("drawing.svg");
    let dxf_path = temporary.path().join("reversed.dxf");
    let roundtrip_dir = temporary.path().join("roundtrip_out");

    let svg_content = r##"<svg width="800" height="600" viewBox="0 0 800 600" xmlns="http://www.w3.org/2000/svg">
  <g id="layer-Walls">
    <line x1="100" y1="100" x2="600" y2="100" stroke="#ff0000" />
    <circle cx="400" cy="300" r="80" stroke="#00ff00" />
    <rect x="200" y="200" width="150" height="100" stroke="#0000ff" />
  </g>
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &dxf_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Dxf);

    let dxf_text = fs::read_to_string(&dxf_path).unwrap();
    assert!(dxf_text.contains("SECTION\n  2\nHEADER"));
    assert!(dxf_text.contains("SECTION\n  2\nTABLES"));
    assert!(dxf_text.contains("LAYER\n  2\nWalls"));
    assert!(dxf_text.contains("LINE\n  8\nWalls"));
    assert!(dxf_text.contains("CIRCLE\n  8\nWalls"));
    assert!(dxf_text.contains("LWPOLYLINE\n  8\nWalls"));
    assert!(dxf_text.contains("EOF"));

    // Round-trip: Convert the generated DXF back to SVG!
    let roundtrip_report =
        convert_path(&dxf_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Dxf
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("id=\"layer-Walls\""));
}

#[test]
fn packages_svg_with_curves_and_text_as_dxf_and_round_trips() {
    use document_svg::{ConvertOptions, convert_path};

    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("drawing_curves.svg");
    let dxf_path = temporary.path().join("reversed_curves.dxf");
    let roundtrip_dir = temporary.path().join("roundtrip_out");

    let svg_content = r##"<svg width="1000" height="800" viewBox="0 0 1000 800" xmlns="http://www.w3.org/2000/svg">
  <g id="layer-Electrical">
    <text x="150" y="120" font-size="14" fill="#1a1a1a">MAIN PANEL 200A</text>
    <path d="M 100 200 C 150 250 250 250 300 200 S 450 150 500 200 Q 600 300 700 200 Z" stroke="#e67e22" />
  </g>
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &dxf_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Dxf);

    let dxf_text = fs::read_to_string(&dxf_path).unwrap();
    assert!(dxf_text.contains("TEXT\n  8\nElectrical"));
    assert!(dxf_text.contains("MAIN PANEL 200A"));
    assert!(dxf_text.contains("LWPOLYLINE\n  8\nElectrical"));

    // Round-trip back to SVG
    let roundtrip_report =
        convert_path(&dxf_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Dxf
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("id=\"layer-Electrical\""));
    assert!(roundtrip_svg.contains("MAIN PANEL 200A"));
}

#[test]
fn packages_svg_as_gcode_and_round_trips() {
    use document_svg::{ConvertOptions, convert_path};

    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("drawing_cut.svg");
    let gcode_path = temporary.path().join("toolpath.nc");
    let roundtrip_dir = temporary.path().join("roundtrip_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="300" height="150" stroke="#000000" />
  <line x1="100" y1="250" x2="400" y2="250" stroke="#ff0000" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &gcode_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Gcode);

    let gcode_text = fs::read_to_string(&gcode_path).unwrap();
    assert!(gcode_text.contains("G21"));
    assert!(gcode_text.contains("G90"));
    assert!(gcode_text.contains("G00 X50.000"));
    assert!(gcode_text.contains("M03 S1000"));
    assert!(gcode_text.contains("G01"));
    assert!(gcode_text.contains("M05"));
    assert!(gcode_text.contains("M02"));

    // Round-trip: Convert the generated G-code back to SVG!
    let roundtrip_report =
        convert_path(&gcode_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Gcode
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("data-semantic-role=\"cam:workspace\""));
    assert!(roundtrip_svg.contains("data-semantic-role=\"gcode:cut\""));
}

#[test]
fn packages_svg_as_gerber_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("pcb_layer.svg");
    let gerber_path = temporary.path().join("copper.gbr");
    let roundtrip_dir = temporary.path().join("gerber_out");

    let svg_content = r##"<svg width="500" height="400" viewBox="0 0 500 400" xmlns="http://www.w3.org/2000/svg">
  <circle cx="100" cy="100" r="1.5" stroke="#e8be38" />
  <circle cx="200" cy="100" r="1.5" stroke="#e8be38" />
  <line x1="100" y1="100" x2="200" y2="100" stroke="#e8be38" stroke-width="0.3" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &gerber_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Gerber);

    let gbr_text = fs::read_to_string(&gerber_path).unwrap();
    assert!(gbr_text.contains("%FSLAX25Y25*%"));
    assert!(gbr_text.contains("%MOMM*%"));
    assert!(gbr_text.contains("D10*"));
    assert!(gbr_text.contains("D03*")); // flash pad
    assert!(gbr_text.contains("M02*"));

    // Round-trip back to SVG
    let roundtrip_report =
        convert_path(&gerber_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Gerber
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("data-semantic-role=\"pcb:copper\""));
}

#[test]
fn packages_svg_as_hpgl_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("plot.svg");
    let hpgl_path = temporary.path().join("drawing.plt");
    let roundtrip_dir = temporary.path().join("hpgl_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="400" height="250" stroke="#0000ff" />
  <circle cx="250" cy="175" r="50" stroke="#ff0000" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &hpgl_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Hpgl);

    let plt_text = fs::read_to_string(&hpgl_path).unwrap();
    assert!(plt_text.contains("IN;DF;"));
    assert!(plt_text.contains("SP5;")); // Blue pen
    assert!(plt_text.contains("SP2;")); // Red pen
    assert!(plt_text.contains("CI")); // Circle
    assert!(plt_text.contains("SP0;")); // Park pen

    // Round-trip back to SVG
    let roundtrip_report =
        convert_path(&hpgl_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Hpgl
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("id=\"pen-"));
    assert!(roundtrip_svg.contains("data-semantic-role=\"hpgl:pen:"));
}

#[test]
fn packages_svg_as_excellon_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("drill.svg");
    let drl_path = temporary.path().join("board.drl");
    let roundtrip_dir = temporary.path().join("drl_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <circle cx="100" cy="100" r="0.8" fill="none" stroke="#000000" />
  <circle cx="200" cy="150" r="0.8" fill="none" stroke="#000000" />
  <circle cx="300" cy="250" r="1.5" fill="none" stroke="#000000" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &drl_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Excellon);

    let drl_text = fs::read_to_string(&drl_path).unwrap();
    assert!(drl_text.contains("M48"));
    assert!(drl_text.contains("METRIC,TZ"));
    assert!(drl_text.contains("T01C"));
    assert!(drl_text.contains("%"));
    assert!(drl_text.contains("X"));
    assert!(drl_text.contains("M30"));

    // Round-trip back to SVG
    let roundtrip_report =
        convert_path(&drl_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Excellon
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("drill-holes"));
}

#[test]
fn packages_svg_as_stl_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("model.svg");
    let stl_path = temporary.path().join("model.stl");
    let roundtrip_dir = temporary.path().join("stl_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="200" height="100" fill="#ffaa00" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &stl_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Stl);

    let stl_text = fs::read_to_string(&stl_path).unwrap();
    assert!(stl_text.contains("solid document_svg_extrusion"));
    assert!(stl_text.contains("facet normal"));
    assert!(stl_text.contains("vertex"));
    assert!(stl_text.contains("endsolid"));

    // Round-trip back to SVG via STL slicer
    let roundtrip_report =
        convert_path(&stl_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Stl
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("slice-background"));
}

#[test]
fn packages_svg_as_obj_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("model.svg");
    let obj_path = temporary.path().join("model.obj");
    let roundtrip_dir = temporary.path().join("obj_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="200" height="100" fill="#ffaa00" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &obj_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Obj);

    let obj_text = fs::read_to_string(&obj_path).unwrap();
    assert!(obj_text.contains("v "));
    assert!(obj_text.contains("f "));

    // Round-trip back to SVG via OBJ renderer
    let roundtrip_report =
        convert_path(&obj_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Obj
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("<path"));
}

#[test]
fn packages_svg_as_step_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("model.svg");
    let step_path = temporary.path().join("model.step");
    let roundtrip_dir = temporary.path().join("step_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="200" height="100" stroke="#000000" fill="none" />
  <line x1="50" y1="50" x2="250" y2="150" stroke="#000000" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &step_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Step);

    let step_text = fs::read_to_string(&step_path).unwrap();
    assert!(step_text.contains("ISO-10303-21;"));
    assert!(step_text.contains("FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));"));
    assert!(step_text.contains("CARTESIAN_POINT"));
    assert!(step_text.contains("EDGE_CURVE"));
    assert!(step_text.contains("END-ISO-10303-21;"));

    // Round-trip back to SVG via STEP isometric renderer
    let roundtrip_report =
        convert_path(&step_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Step
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("<path"));
}

#[test]
fn packages_svg_as_gmsh_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("model.svg");
    let msh_path = temporary.path().join("model.msh");
    let roundtrip_dir = temporary.path().join("msh_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="200" height="100" stroke="#000000" fill="none" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &msh_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Gmsh);

    let msh_text = fs::read_to_string(&msh_path).unwrap();
    assert!(msh_text.contains("$MeshFormat"));
    assert!(msh_text.contains("2.2 0 8"));
    assert!(msh_text.contains("$Nodes"));
    assert!(msh_text.contains("$Elements"));

    // Round-trip back to SVG via Gmsh FEA mesh renderer
    let roundtrip_report =
        convert_path(&msh_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Simulation
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("<path"));
}

#[test]
fn packages_svg_as_vtk_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("model.svg");
    let vtk_path = temporary.path().join("model.vtk");
    let roundtrip_dir = temporary.path().join("vtk_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="200" height="100" stroke="#000000" fill="none" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &vtk_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Vtk);

    let vtk_text = fs::read_to_string(&vtk_path).unwrap();
    assert!(vtk_text.contains("# vtk DataFile Version 3.0"));
    assert!(vtk_text.contains("DATASET POLYDATA"));
    assert!(vtk_text.contains("POINTS"));
    assert!(vtk_text.contains("POLYGONS"));
    assert!(vtk_text.contains("POINT_DATA"));

    // Round-trip back to SVG via VTK scalar field renderer
    let roundtrip_report =
        convert_path(&vtk_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Simulation
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("<path"));
}

#[test]
fn packages_svg_as_ply_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("model.svg");
    let ply_path = temporary.path().join("model.ply");
    let roundtrip_dir = temporary.path().join("ply_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="200" height="100" stroke="#ff0000" fill="none" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &ply_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Ply);

    let ply_text = fs::read_to_string(&ply_path).unwrap();
    assert!(ply_text.contains("ply"));
    assert!(ply_text.contains("format ascii 1.0"));
    assert!(ply_text.contains("element vertex"));
    assert!(ply_text.contains("element face"));
    assert!(ply_text.contains("end_header"));

    // Round-trip back to SVG via PLY 3D isometric renderer
    let roundtrip_report =
        convert_path(&ply_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Ply
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("<path"));
}

#[test]
fn packages_svg_with_elliptical_arcs_and_transforms_to_cad() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("arc_model.svg");
    let dxf_path = temporary.path().join("arc_model.dxf");

    // SVG containing elliptical arc ('A') and group transform
    let svg_content = r##"<svg width="800" height="600" viewBox="0 0 800 600" xmlns="http://www.w3.org/2000/svg">
  <g id="layer-MachinedParts" transform="translate(100, 50)">
    <!-- Semicircular slot with elliptical arc -->
    <path d="M 0 0 L 100 0 A 25 25 0 0 1 100 50 L 0 50 A 25 25 0 0 1 0 0 Z" style="stroke: red; stroke-width: 2;" />
  </g>
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &dxf_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Dxf);

    let dxf_text = fs::read_to_string(&dxf_path).unwrap();
    assert!(dxf_text.contains("MachinedParts"));
    assert!(dxf_text.contains("LWPOLYLINE"));
    // Color 1 is Red in AutoCAD Color Index (ACI)
    assert!(dxf_text.contains("  62\n1"));
}

#[test]
fn packages_svg_as_iges_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("model.svg");
    let iges_path = temporary.path().join("model.iges");
    let roundtrip_dir = temporary.path().join("iges_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="200" height="100" stroke="#000000" fill="none" />
  <line x1="50" y1="50" x2="250" y2="150" stroke="#000000" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &iges_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Iges);

    let iges_text = fs::read_to_string(&iges_path).unwrap();
    assert!(iges_text.contains("S      1"));
    assert!(iges_text.contains("G      1"));
    assert!(iges_text.contains("D      1"));
    assert!(iges_text.contains("P      1"));
    assert!(iges_text.contains("T      1"));
    assert!(iges_text.contains("110,")); // Entity 110: Line

    // Round-trip back to SVG via IGES wireframe isometric renderer
    let roundtrip_report =
        convert_path(&iges_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::Iges
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("<path"));
}

#[test]
fn packages_svg_as_threemf_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let svg_path = temporary.path().join("model.svg");
    let threemf_path = temporary.path().join("model.3mf");
    let roundtrip_dir = temporary.path().join("threemf_out");

    let svg_content = r##"<svg width="600" height="400" viewBox="0 0 600 400" xmlns="http://www.w3.org/2000/svg">
  <rect x="50" y="50" width="200" height="100" stroke="#0000ff" fill="none" />
</svg>"##;
    fs::write(&svg_path, svg_content).unwrap();

    let report = svg_to_document(&svg_path, &threemf_path, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::ThreeMf);

    // Verify 3MF OPC ZIP container contents
    let file = fs::File::open(&threemf_path).unwrap();
    let mut archive = ZipArchive::new(file).unwrap();
    assert!(archive.by_name("[Content_Types].xml").is_ok());
    assert!(archive.by_name("_rels/.rels").is_ok());
    let mut model_file = archive.by_name("3D/3dmodel.model").unwrap();
    let mut model_xml = String::new();
    model_file.read_to_string(&mut model_xml).unwrap();
    assert!(model_xml.contains("<mesh>"));
    assert!(model_xml.contains("<vertices>"));
    assert!(model_xml.contains("<triangles>"));

    // Round-trip back to SVG via 3MF isometric shaded mesh renderer
    let roundtrip_report =
        convert_path(&threemf_path, &roundtrip_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(
        roundtrip_report.source_format,
        document_svg::SourceFormat::ThreeMf
    );
    assert_eq!(roundtrip_report.page_count, 1);

    let roundtrip_svg = fs::read_to_string(roundtrip_dir.join("page-0001.svg")).unwrap();
    assert!(roundtrip_svg.contains("<path"));
}

#[test]
fn packages_svg_directory_as_pdf_and_round_trips() {
    let temporary = TempDir::new().unwrap();
    let input = make_svg_pages(&temporary);
    let output = temporary.path().join("pages.pdf");

    let report = svg_to_document(&input, &output, &ReverseOptions::default()).unwrap();

    assert_eq!(report.output_format, ReverseFormat::Pdf);
    assert_eq!(report.page_count, 2);
    assert!(output.exists());
    let pdf_bytes = fs::read(&output).unwrap();
    assert!(pdf_bytes.starts_with(b"%PDF-1.4"));

    // Verify lopdf can parse the generated PDF and extract the 2 pages
    let doc = lopdf::Document::load_mem(&pdf_bytes).unwrap();
    let pages = doc.get_pages();
    assert_eq!(pages.len(), 2);

    // Round-trip back to SVG via built-in PDF converter
    let round_trip = temporary.path().join("pdf-svg");
    let converted = convert_path(&output, &round_trip, &ConvertOptions::default()).unwrap();
    assert_eq!(converted.page_count, 2);
    assert_eq!(converted.source_format, document_svg::SourceFormat::Pdf);
    assert!(round_trip.join("page-0001.svg").exists());
    assert!(round_trip.join("page-0002.svg").exists());
}
