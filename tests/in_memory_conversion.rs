use std::fs;

use document_svg::{ByteConvertLimits, ConvertOptions, convert_bytes, convert_path};

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn memory_conversion_matches_path_conversion_for_browser_formats() {
    for name in [
        "sample_browser.pdf",
        "sample_jpx_alpha.pdf",
        "sample_strict.docx",
        "sample_strict.xlsx",
        "sample_strict.pptx",
    ] {
        let path = fixture(name);
        let input = fs::read(&path).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let native = convert_path(&path, directory.path(), &ConvertOptions::default()).unwrap();
        let expected = native
            .pages
            .iter()
            .map(|page| fs::read_to_string(directory.path().join(&page.svg)).unwrap())
            .collect::<Vec<_>>();
        let mut actual = Vec::new();
        let mut text_spans = Vec::new();
        let browser = convert_bytes(
            name,
            &input,
            &ConvertOptions::default(),
            ByteConvertLimits::default(),
            |page| {
                actual.push(page.svg);
                text_spans.extend(page.text_spans.into_iter().map(|span| span.text));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(browser.source_format, native.source_format, "{name}");
        assert_eq!(browser.page_count, native.page_count, "{name}");
        assert_eq!(actual, expected, "{name}");
        if name == "sample_browser.pdf" {
            assert!(
                text_spans
                    .iter()
                    .any(|text| text.contains("Browser PDF preview")),
                "text layer omitted the synthetic PDF content"
            );
        }
    }
}

#[test]
fn memory_conversion_enforces_output_limits_before_emitting_an_oversized_page() {
    let name = "sample_strict.docx";
    let input = fs::read(fixture(name)).unwrap();
    let mut emitted = 0;
    let result = convert_bytes(
        name,
        &input,
        &ConvertOptions::default(),
        ByteConvertLimits {
            max_page_svg_bytes: 32,
            max_total_svg_bytes: 128,
            ..ByteConvertLimits::default()
        },
        |_| {
            emitted += 1;
            Ok(())
        },
    );

    assert!(matches!(result, Err(document_svg::Error::LimitExceeded(_))));
    assert_eq!(emitted, 0);
}

#[test]
fn memory_conversion_bounds_and_warns_about_the_search_text_layer() {
    let name = "sample_strict.docx";
    let input = fs::read(fixture(name)).unwrap();
    let mut total_spans = 0;
    let mut saw_truncation_warning = false;
    convert_bytes(
        name,
        &input,
        &ConvertOptions::default(),
        ByteConvertLimits {
            max_page_text_spans: 1,
            max_total_text_spans: 2,
            max_total_text_bytes: 256,
            ..ByteConvertLimits::default()
        },
        |page| {
            assert!(page.text_spans.len() <= 1);
            total_spans += page.text_spans.len();
            saw_truncation_warning |= page
                .warnings
                .iter()
                .any(|warning| warning.contains("text selection/search layer"));
            Ok(())
        },
    )
    .unwrap();

    assert!(total_spans <= 2);
    assert!(saw_truncation_warning);
}
