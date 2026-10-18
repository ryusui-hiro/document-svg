use document_svg::{TransformOptions, transform_svg};

#[test]
fn test_transform_svg_returns_error_for_invalid_xml() -> Result<(), Box<dyn std::error::Error>> {
    let invalid_svg = b"<svg viewBox=\"0 0 10 10\"><path></svg>";
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

#[test]
fn transform_precision_is_bounded_and_does_not_truncate_large_exponents() {
    let svg = br#"<svg viewBox="0 0 1e100 10.12345"/>"#;
    let error = transform_svg(
        svg,
        &TransformOptions {
            precision: Some(usize::MAX),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("precision must be between 0 and 12")
    );

    let transformed = transform_svg(
        svg,
        &TransformOptions {
            precision: Some(2),
            ..Default::default()
        },
    )
    .unwrap();
    let text = String::from_utf8(transformed).unwrap();
    assert!(text.contains("viewBox=\"0 0 1e100 10.12\""), "{text}");
}

#[test]
fn minify_preserves_text_and_explicit_space_while_removing_indentation() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg">
  <g><text xml:space="preserve">  A  <tspan> </tspan>B  </text></g>
  <text> A </text>
  <g xml:space="preserve"> <rect width="1" height="1"/> </g>
</svg>"#;
    let transformed = transform_svg(
        svg,
        &TransformOptions {
            minify: true,
            strip_empty_groups: true,
            ..Default::default()
        },
    )
    .unwrap();
    let text = String::from_utf8(transformed).unwrap();
    assert!(
        text.contains("<text xml:space=\"preserve\">  A  <tspan> </tspan>B  </text>"),
        "{text}"
    );
    assert!(text.contains("<text> A </text>"), "{text}");
    assert!(
        text.contains("<g xml:space=\"preserve\"> <rect width=\"1\" height=\"1\"/> </g>"),
        "{text}"
    );
    assert!(!text.contains("\n  <g>"), "{text}");
}

#[test]
fn strip_empty_groups_keeps_nested_rendering_order() {
    let svg = br#"<svg><g id="outer"><g id="empty"></g><g id="first"><rect width="1" height="1"/></g><g id="second"><circle r="1"/></g></g></svg>"#;
    let transformed = transform_svg(
        svg,
        &TransformOptions {
            strip_empty_groups: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        std::str::from_utf8(&transformed).unwrap(),
        "<svg><g id=\"outer\"><g id=\"first\"><rect width=\"1\" height=\"1\"/></g><g id=\"second\"><circle r=\"1\"/></g></g></svg>"
    );
}

#[test]
fn minify_keeps_doctype_needed_by_entity_references() {
    let svg = br#"<?xml version="1.0" standalone="yes"?><!DOCTYPE svg [<!ENTITY label "Hello">]><svg><text>&label;</text></svg>"#;
    let transformed = transform_svg(
        svg,
        &TransformOptions {
            minify: true,
            ..Default::default()
        },
    )
    .unwrap();
    let text = std::str::from_utf8(&transformed).unwrap();
    assert!(
        text.starts_with("<?xml version=\"1.0\" standalone=\"yes\"?>"),
        "{text}"
    );
    assert!(
        text.contains("<!DOCTYPE svg [<!ENTITY label \"Hello\">]>"),
        "{text}"
    );
    assert!(text.contains("<text>&label;</text>"), "{text}");
}

#[test]
fn responsive_requires_a_usable_viewbox_or_absolute_dimensions() {
    let percentage = br#"<svg width="100%" height="100%"><rect width="10"/></svg>"#;
    let error = transform_svg(
        percentage,
        &TransformOptions {
            responsive: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires a viewBox or positive absolute width")
    );

    let with_viewbox = br#"<svg width="100%" height="100%" viewBox="0 0 10 10"/>"#;
    let transformed = transform_svg(
        with_viewbox,
        &TransformOptions {
            responsive: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(transformed, br#"<svg viewBox="0 0 10 10"/>"#);
}

#[test]
fn responsive_uses_each_nested_svg_size_without_rounding_it_to_zero() {
    let svg = br#"<svg width="100" height="80"><svg width="0.1" height="0.2"/></svg>"#;
    let transformed = transform_svg(
        svg,
        &TransformOptions {
            responsive: true,
            precision: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        transformed,
        br#"<svg viewBox="0 0 100 80"><svg viewBox="0 0 0.1 0.2"/></svg>"#
    );
}

#[test]
fn transform_requires_one_svg_root() {
    for input in [
        &b""[..],
        &b"<html/>"[..],
        &b"<svg/><svg/>"[..],
        &b"<svg/>trailing"[..],
        &b"&amp;<svg/>"[..],
        &b"<svg xmlns=\"https://example.invalid/not-svg\"/>"[..],
        &b"<s:svg xmlns:s=\"https://example.invalid/not-svg\"/>"[..],
    ] {
        assert!(
            transform_svg(input, &TransformOptions::default()).is_err(),
            "accepted {}",
            String::from_utf8_lossy(input)
        );
    }
}

#[test]
fn transform_keeps_entity_references_inside_their_group() {
    let svg = br#"<!DOCTYPE svg [<!ENTITY label "Hello">]><svg><g id="name">&label;</g></svg>"#;
    let transformed = transform_svg(
        svg,
        &TransformOptions {
            strip_empty_groups: true,
            ..Default::default()
        },
    )
    .unwrap();
    let text = std::str::from_utf8(&transformed).unwrap();
    assert!(text.contains("<g id=\"name\">&label;</g>"), "{text}");
}

#[test]
fn transform_preserves_a_namespaced_svg_root() {
    let svg = br#"<s:svg xmlns:s="http://www.w3.org/2000/svg" width="24" height="12"><s:rect width="24" height="12"/></s:svg>"#;
    let transformed = transform_svg(
        svg,
        &TransformOptions {
            responsive: true,
            ..Default::default()
        },
    )
    .unwrap();
    let text = std::str::from_utf8(&transformed).unwrap();
    assert!(
        text.starts_with("<s:svg xmlns:s=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 12\">"),
        "{text}"
    );
    assert!(
        text.contains("<s:rect width=\"24\" height=\"12\"/>"),
        "{text}"
    );
}
