use std::fs;
use std::io::Read;

use document_svg::{ConvertOptions, OpenXmlFormat, ReverseOptions, convert_path, svg_to_openxml};
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
