use document_svg::{TransformOptions, transform_svg};

#[test]
fn test_transform_svg_returns_error_for_invalid_xml() -> Result<(), Box<dyn std::error::Error>> {
    let invalid_svg = b"<svg><path></svg>";
    let result = transform_svg(
        invalid_svg,
        &TransformOptions {
            minify: true,
            responsive: true,
            precision: Some(2),
            ..Default::default()
        },
    );

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("failed to parse SVG input"));
    Ok(())
}
