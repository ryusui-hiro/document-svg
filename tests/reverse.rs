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
