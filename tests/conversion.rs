use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use base64::Engine;
use document_svg::{ConvertOptions, SourceFormat, convert_path};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

#[path = "../src/pdf/font_test_data.rs"]
mod font_test_data;

#[test]
fn converts_embedded_true_type_to_outlines_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("embedded-font.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = font_test_data::font(None, false, false);
    let font_file = document.add_object(Stream::new(
        dictionary! { "Length1" => bytes.len() as i64 },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "Arial", "Flags" => 32,
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 900.into()],
        "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80,
        "FontFile2" => font_file,
    });
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "TrueType", "BaseFont" => "Arial",
        "FirstChar" => 32, "LastChar" => 66,
        "Widths" => (32..=66).map(|code| Object::Integer(if code == 32 { 250 } else { 500 })).collect::<Vec<_>>(),
        "FontDescriptor" => descriptor, "Encoding" => "WinAnsiEncoding",
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 20 80 Td (A B) Tj ET".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let options = ConvertOptions {
        outline_embedded_pdf_text: true,
        ..Default::default()
    };
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    let report = convert_path(&input, &first, &options).unwrap();
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(first.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"text-outline\""), "{svg}");
    assert!(svg.contains("L 0.5 0"), "{svg}");
    convert_path(&input, &second, &options).unwrap();
    assert_eq!(
        svg,
        fs::read_to_string(second.join("page-0001.svg")).unwrap()
    );
}

#[test]
fn refuses_nonempty_output_without_modifying_existing_files() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.pdf");
    let output = temporary.path().join("out");
    make_pdf(&input);
    fs::create_dir(&output).unwrap();
    let existing = output.join("page-0001.svg");
    fs::write(&existing, "keep me").unwrap();
    assert!(convert_path(&input, &output, &ConvertOptions::default()).is_err());
    assert_eq!(fs::read_to_string(existing).unwrap(), "keep me");
}

#[test]
fn converts_minimal_pdf() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.pdf");
    let output = temporary.path().join("out");
    make_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Pdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello PDF"));
    assert!(svg.contains("data-source-format=\"pdf\""));
    assert!(svg.contains("<tspan x=\"0\" y=\"0\">H</tspan>"));
    assert!(svg.contains("xml:space=\"preserve\""));
}

#[test]
fn applies_pdf_page_user_unit_to_dimensions_and_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("user-unit.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let resources_id = document.add_object(dictionary! {});
    let content = Content {
        operations: vec![
            Operation::new("re", vec![10.into(), 10.into(), 20.into(), 10.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    document.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "Contents" => Object::Reference(content_id),
            "Resources" => Object::Reference(resources_id),
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 50.into()],
            "UserUnit" => 2,
        }),
    );
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    document.trailer.set("Root", Object::Reference(catalog_id));
    document.save(&input).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.pages[0].width_points, 200.0);
    assert_eq!(report.pages[0].height_points, 100.0);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("width=\"200pt\" height=\"100pt\""));
    assert!(svg.contains("transform=\"matrix(2 0 0 -2 0 100)\""));
}

#[test]
fn applies_pdf_ext_gstate_line_style_parameters() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ext-gstate-line-style.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let state_id = document.add_object(dictionary! {
        "Type" => "ExtGState",
        "LW" => 14,
        "LC" => 2,
        "LJ" => 1,
        "ML" => 4,
        "D" => vec![Object::Array(vec![8.into(), 3.into()]), 2.into()],
    });
    let resources_id = document.add_object(dictionary! {
        "ExtGState" => dictionary! { "GS1" => Object::Reference(state_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
            Operation::new("m", vec![20.into(), 20.into()]),
            Operation::new("l", vec![200.into(), 20.into()]),
            Operation::new("S", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("stroke-width=\"14\""));
    assert!(svg.contains("stroke-linecap=\"square\""));
    assert!(svg.contains("stroke-linejoin=\"round\""));
    assert!(svg.contains("stroke-dasharray=\"8 3\""));
    assert!(svg.contains("stroke-dashoffset=\"2\""));
}

#[test]
fn restores_pdf_text_state_parameters_across_q_and_q_restore() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("text-state-restore.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => Object::Reference(font_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new("Tr", vec![2.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 60.into()],
            ),
            Operation::new(
                "Tj",
                vec![Object::String(
                    b"Outlined".to_vec(),
                    lopdf::StringFormat::Literal,
                )],
            ),
            Operation::new("ET", vec![]),
            Operation::new("Q", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 30.into()],
            ),
            Operation::new(
                "Tj",
                vec![Object::String(
                    b"Body".to_vec(),
                    lopdf::StringFormat::Literal,
                )],
            ),
            Operation::new("ET", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let outlined = svg.split("aria-label=\"Outlined\"").next().unwrap();
    assert!(
        outlined
            .rsplit("<text")
            .next()
            .unwrap()
            .contains("stroke=\"#000000\"")
    );
    let body = svg.split("aria-label=\"Body\"").next().unwrap();
    assert!(
        body.rsplit("<text")
            .next()
            .unwrap()
            .contains("stroke=\"none\"")
    );
}

#[test]
fn ignores_inline_image_tokens_inside_pdf_strings() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bi-string.pdf");
    let output = temporary.path().join("out");
    make_pdf_with_text(&input, "BI without inline ID");

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("BI without inline ID"));
}

#[test]
fn ignores_unknown_pdf_operators_inside_compatibility_sections() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("compatibility.pdf");
    let output = temporary.path().join("out");
    make_pdf_compatibility_section(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"vector\""));
    assert!(svg.contains("fill=\"#FF0000\""));
}

#[test]
fn converts_pdf_type3_charproc_glyphs() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("type3.pdf");
    let output = temporary.path().join("out");
    make_type3_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"type3-text\""));
    assert!(svg.contains("aria-label=\"A\""));
    assert!(svg.contains("M 1 0 L 300 700 L 600 0 Z"));
    assert!(svg.contains("fill=\"#FF0000\""));
}

#[test]
fn converts_pdf_packed_samples_and_stencil_images() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("packed-images.pdf");
    let output = temporary.path().join("out");
    make_packed_images_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data:image/png;base64,").count(), 4);
    assert_eq!(svg.matches("data-content-kind=\"image\"").count(), 2);
    assert_eq!(svg.matches("image-rendering=\"pixelated\"").count(), 2);
}

#[test]
fn converts_pdf_jpeg_soft_mask_to_png_alpha() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("jpeg-soft-mask.pdf");
    let output = temporary.path().join("out");
    make_jpeg_soft_mask_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("data:image/png;base64,")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap();
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!((info.width, info.height), (2, 1));
    assert!(pixels[3] < 16, "alpha={}", pixels[3]);
    assert!(pixels[7] > 239, "alpha={}", pixels[7]);
}

#[test]
fn normalizes_inverted_pdf_cmyk_jpeg_to_rgb() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("cmyk-jpeg.pdf");
    let output = temporary.path().join("out");
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 100)
        .encode(
            &[255, 255, 255, 255],
            1,
            1,
            jpeg_encoder::ColorType::CmykAsYcck,
        )
        .unwrap();
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 1,
            "Height" => 1,
            "ColorSpace" => "DeviceCMYK",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        jpeg,
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Cmyk" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Cmyk".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("data:image/jpeg;base64,")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .unwrap();
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
    let pixels = decoder.decode().unwrap();
    assert!(pixels.iter().all(|value| *value > 240), "{pixels:?}");
}

#[test]
fn rejects_pdf_image_dimension_bombs_before_allocation() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("image-bomb.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 100_000,
            "Height" => 100_000,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
        },
        Vec::new(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Bomb" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Bomb".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("expands to 100000x100000 pixels")
    );
}

#[test]
fn skips_pdf_images_with_invalid_packed_samples_and_reports_warning() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("invalid-packed-image.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 2,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
        },
        vec![0],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Broken" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Broken".to_vec())])],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("invalid packed samples and was skipped"));
    assert!(output.join("page-0001.svg").exists());
}

#[test]
fn converts_pdf_axial_shading_to_svg_gradient() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shading.pdf");
    let output = temporary.path().join("out");
    make_shading_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<linearGradient"));
    assert!(svg.contains("#FF0000"));
    assert!(svg.contains("#0000FF"));
    assert!(svg.contains("data-content-kind=\"gradient\""));
}

#[test]
fn converts_pdf_function_shading_with_calculator_function() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("function-shading.pdf");
    let output = temporary.path().join("out");
    make_function_shading_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"function-shading-cell\""));
    assert!(svg.contains("#01FE40"));
    assert!(
        svg.matches("data-content-kind=\"function-shading-cell\"")
            .count()
            >= 100
    );
}

#[test]
fn converts_pdf_luminosity_soft_mask_to_svg_mask() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("soft-mask.pdf");
    let output = temporary.path().join("out");
    make_soft_mask_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<mask id=\"pdf-mask-1-1\""));
    assert!(svg.contains("mask-type:luminance"));
    assert!(svg.contains("mask=\"url(#pdf-mask-1-1)\""));
}

#[test]
fn converts_pdf_soft_mask_transfer_function_to_svg_filter() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("soft-mask-transfer.pdf");
    let output = temporary.path().join("out");
    make_soft_mask_transfer_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"pdf-mask-1-1-transfer\""));
    assert!(svg.contains("<feColorMatrix"));
    assert!(svg.contains("<feFuncR type=\"table\""));
    assert!(svg.contains(" 0.25 "));
}

#[test]
fn preserves_nonisolated_pdf_transparency_group() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("nonisolated.pdf");
    let output = temporary.path().join("out");
    make_nonisolated_group_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"transparency-group\""));
    assert!(!svg.contains("isolation:isolate"));
}

#[test]
fn converts_pdf_special_color_spaces_and_tint_functions() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("special-colors.pdf");
    let output = temporary.path().join("out");
    make_special_color_space_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("fill=\"#33CC4D\""));
    assert!(svg.contains("fill=\"#33CC80\""));
    assert!(svg.contains("fill=\"#777777\""));
    assert!(svg.contains("fill=\"#FF0000\""));
}

#[test]
fn converts_pdf_shading_pattern_with_background_bbox_and_alpha() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shading-pattern.pdf");
    let output = temporary.path().join("out");
    make_shading_pattern_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<linearGradient"));
    assert!(svg.contains("id=\"pdf-pattern-clip-1-1\""));
    assert!(svg.contains("stop-color=\"#CC3300\" stop-opacity=\"0.9375\""));
    assert!(svg.contains("stop-color=\"#00FF00\" stop-opacity=\"0.75\""));
}

#[test]
fn converts_pdf_colored_and_uncolored_tiling_patterns() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("tiling-patterns.pdf");
    let output = temporary.path().join("out");
    make_tiling_pattern_pdf(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.matches("<pattern id=\"pdf-pattern-").count() >= 4);
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("fill=\"#0000FF\""));
    assert!(svg.contains("fill=\"#00FF00\""));
    assert!(svg.matches("fill=\"url(#pdf-pattern-").count() >= 4);
}

#[test]
fn converts_minimal_pptx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Title"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill></p:spPr><p:txBody><a:p><a:r><a:rPr sz="2400" b="1"/><a:t>Hello PPTX</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Pptx);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello PPTX"));
    assert!(svg.contains("#DDEEFF"));
}

#[test]
fn converts_pptx_outer_shadow_to_bounded_svg_filter() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("outer-shadow.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Shadow card"/></p:nvSpPr><p:spPr><a:xfrm rot="1800000"><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="1270000"/></a:xfrm><a:prstGeom prst="roundRect"/><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill><a:effectLst><a:outerShdw blurRad="25400" dist="25400" dir="5400000" rotWithShape="0"><a:srgbClr val="123456"><a:alpha val="50000"/></a:srgbClr></a:outerShdw></a:effectLst></p:spPr><p:txBody><a:p><a:r><a:rPr sz="1200"/><a:t>Shadow text</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"outer-shadow-pptx-slide-1-1\""));
    assert!(svg.contains("stdDeviation=\"1\""));
    assert!(svg.contains("dx=\"0\" dy=\"2\""));
    assert!(svg.contains("flood-color=\"#123456\" flood-opacity=\"0.5\""));
    assert!(svg.contains("fill=\"#DDEEFF\""));
    assert_eq!(svg.matches("filter=\"url(#outer-shadow-").count(), 1);
    assert!(svg.contains("Shadow text"));
}

#[test]
fn converts_pptx_connector_glow_to_bounded_svg_filter() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("connector-glow.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:a14="a14"><p:cSld><p:spTree><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="1" name="Glowing connector"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="1270000"/></a:xfrm><a:prstGeom prst="line"/><a:ln w="25400"><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln><a:effectLst><a:glow rad="63500"><a:schemeClr val="accent2"><a:satMod val="175000"/><a:alpha val="70000"/></a:schemeClr></a:glow></a:effectLst><a14:hiddenEffects><a:effectLst><a:glow rad="127000"><a:srgbClr val="FF00FF"/></a:glow></a:effectLst></a14:hiddenEffects></p:spPr></p:cxnSp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("filter=\"url(#glow-").count(), 1);
    assert!(svg.contains("id=\"glow-pptx-slide-1-1\""));
    assert!(svg.contains("stdDeviation=\"2.5\" result=\"glow-blur\""));
    assert!(svg.contains("flood-color=\"#FF7A1F\""));
    assert!(svg.contains("flood-opacity=\"0.7\" result=\"glow-color\""));
    assert!(svg.contains("<feMergeNode in=\"glow\"/>"));
}

#[test]
fn converts_pptx_text_effects_but_ignores_hidden_compatibility_effects() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("text-shadow.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:a14="a14"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Text effects"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="1270000"/></a:xfrm><a:prstGeom prst="rect"/><a:noFill/><a14:hiddenFill><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a14:hiddenFill><a14:hiddenLine><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a14:hiddenLine><a14:hiddenEffects><a:effectLst><a:outerShdw dist="127000" dir="0"><a:srgbClr val="FF00FF"/></a:outerShdw><a:glow rad="127000"><a:srgbClr val="FF00FF"/></a:glow></a:effectLst></a14:hiddenEffects></p:spPr><p:txBody><a:p><a:r><a:rPr sz="1800"><a:effectLst><a:outerShdw blurRad="38100" dist="38100" dir="2700000"><a:srgbClr val="000000"><a:alpha val="43137"/></a:srgbClr></a:outerShdw><a:glow rad="25400"><a:srgbClr val="00AAFF"><a:alpha val="50000"/></a:srgbClr></a:glow></a:effectLst></a:rPr><a:t>Visible text effects</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("filter=\"url(#drawingml-effects-").count(), 1);
    assert!(svg.contains("id=\"drawingml-effects-pptx-slide-1-1-text-1-1\""));
    assert!(svg.contains("stdDeviation=\"1.5\""));
    assert!(svg.contains("dx=\"2.12132\" dy=\"2.12132\""));
    assert!(svg.contains("flood-opacity=\"0.43137\""));
    assert!(svg.contains("flood-color=\"#00AAFF\" flood-opacity=\"0.5\""));
    assert!(svg.contains("<feMergeNode in=\"shadow\"/><feMergeNode in=\"glow\"/>"));
    assert!(!svg.contains("<path id=\"pptx-slide-1-1\""));
}

#[test]
fn inherits_pptx_master_layout_and_placeholder_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inherited.pptx");
    let output = temporary.path().join("out");
    make_master_layout_pptx(&input);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Pptx);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#112233"));
    assert!(svg.contains("#445566"));
    assert!(svg.contains("Inherited title"));
    assert!(svg.contains("x=\"149\""));
    assert!(!svg.contains("MASTER SAMPLE"));
    assert!(!svg.contains("LAYOUT SAMPLE"));
}

#[test]
fn inherits_pptx_layout_background_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("layout-background.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                br#"<p:sldLayout xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:bg><p:bgPr><a:blipFill><a:blip r:embed="rId2"/><a:stretch><a:fillRect/></a:stretch></a:blipFill></p:bgPr></p:bg><p:spTree/></p:cSld></p:sldLayout>"#.to_vec(),
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/background.png"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                br#"<p:sldMaster xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldMaster>"#.to_vec(),
            ),
            ("ppt/media/background.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"background\""));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("width=\"720\" height=\"540\""));
}

#[test]
fn resolves_pptx_effect_refs_from_each_slide_master_theme() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("multi-theme-effects.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Theme shadow"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></p:spPr><p:style><a:effectRef idx="1"><a:schemeClr val="accent1"/></a:effectRef></p:style></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="No theme effect"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="1905000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></p:spPr><p:style><a:effectRef idx="0"><a:schemeClr val="accent1"/></a:effectRef></p:style></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Explicitly cleared effect"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="3683000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst/></p:spPr><p:style><a:effectRef idx="1"><a:schemeClr val="accent1"/></a:effectRef></p:style></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/slide2.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="1" name="Theme glow"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="2540000" cy="635000"/></a:xfrm><a:prstGeom prst="line"/><a:ln w="25400"><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln></p:spPr><p:style><a:effectRef idx="2"><a:schemeClr val="accent2"/></a:effectRef></p:style></p:cxnSp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/_rels/slide2.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout2.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                r#"<p:sldLayout xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout2.xml",
                r#"<p:sldLayout xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout2.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster2.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                r#"<p:sldMaster xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldMaster>"#,
            ),
            (
                "ppt/slideMasters/slideMaster2.xml",
                r#"<p:sldMaster xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldMaster>"#,
            ),
            (
                "ppt/slideMasters/_rels/slideMaster1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/_rels/slideMaster2.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme2.xml"/></Relationships>"#,
            ),
            (
                "ppt/theme/theme1.xml",
                r#"<a:theme xmlns:a="a"><a:themeElements><a:fmtScheme><a:effectStyleLst><a:effectStyle><a:effectLst><a:outerShdw blurRad="25400" dist="12700" dir="0" rotWithShape="0"><a:srgbClr val="AA0000"><a:alpha val="50000"/></a:srgbClr></a:outerShdw></a:effectLst></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst></a:fmtScheme></a:themeElements></a:theme>"#,
            ),
            (
                "ppt/theme/theme2.xml",
                r#"<a:theme xmlns:a="a"><a:themeElements><a:fmtScheme><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst><a:glow rad="38100"><a:srgbClr val="0000AA"><a:alpha val="60000"/></a:srgbClr></a:glow></a:effectLst></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst></a:fmtScheme></a:themeElements></a:theme>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(first.matches("filter=\"url(#outer-shadow-").count(), 1);
    assert!(first.contains("id=\"outer-shadow-pptx-slide-1-1\""));
    assert!(first.contains("flood-color=\"#AA0000\" flood-opacity=\"0.5\""));
    assert!(!first.contains("outer-shadow-pptx-slide-1-2"));
    assert!(!first.contains("outer-shadow-pptx-slide-1-3"));
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert_eq!(second.matches("filter=\"url(#glow-").count(), 1);
    assert!(second.contains("id=\"glow-pptx-slide-2-1\""));
    assert!(second.contains("flood-color=\"#0000AA\" flood-opacity=\"0.6\""));
}

#[test]
fn recovers_pptx_placeholder_geometry_and_ignores_empty_zero_size_artifacts() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("placeholder-fallbacks.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Inherited title"/><p:cNvSpPr/><p:nvPr><p:ph type="title" idx="99"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>Inherited title text</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Footer fallback"/><p:cNvSpPr/><p:nvPr><p:ph type="ftr" idx="77"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>Footer text</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Slide number fallback"/><p:cNvSpPr/><p:nvPr><p:ph type="sldNum" idx="78"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:fld id="1" type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="4" name="Empty compatibility line"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="10" y="20"/><a:ext cx="0" cy="100"/></a:xfrm><a:prstGeom prst="line"/><a:noFill/><a:ln w="0"><a:noFill/></a:ln></p:spPr><p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="5" name="Malformed body"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="500"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>Missing geometry</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                r#"<p:sldLayout xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Title layout"/><p:cNvSpPr/><p:nvPr><p:ph type="title" idx="5"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr/><a:p/></p:txBody></p:sp></p:spTree></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                r#"<p:sldMaster xmlns:p="p"><p:cSld><p:spTree/></p:cSld></p:sldMaster>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("Malformed body"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Inherited title\""));
    assert!(svg.contains("Inherited title text"));
    assert!(svg.contains("data-source-id=\"Footer fallback\""));
    assert!(svg.contains("Footer text"));
    assert!(svg.contains("data-source-id=\"Slide number fallback\""));
    assert!(svg.contains(">1</tspan>"));
    assert!(!svg.contains("Empty compatibility line"));
    assert!(!svg.contains("Missing geometry"));
}

#[test]
fn converts_pptx_gradient_and_arrow_geometry() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("gradient-arrow.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Gradient triangle"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="1270000"/></a:xfrm><a:prstGeom prst="triangle"/><a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="FF0000"/></a:gs><a:gs pos="100000"><a:srgbClr val="0000FF"/></a:gs></a:gsLst><a:lin ang="0"/></a:gradFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Arrow"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="1905000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="rightArrow"/><a:solidFill><a:srgbClr val="00AA44"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<linearGradient"));
    assert!(svg.contains("#FF0000"));
    assert!(svg.contains("#0000FF"));
    assert!(svg.contains("data-source-id=\"Arrow\""));
    assert!(svg.contains("#00AA44"));
}

#[test]
fn converts_pptx_svg_blip_and_common_presets() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("svg-blip-presets.pptx");
    let output = temporary.path().join("out");
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r" xmlns:asvg="asvg"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="1" name="SVG logo"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip><a:extLst><a:ext><asvg:svgBlip r:embed="rId2"/></a:ext></a:extLst></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="2" name="Straight connector"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="127000" y="1016000"/><a:ext cx="1270000" cy="254000"/></a:xfrm><a:prstGeom prst="straightConnector1"/><a:ln><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln></p:spPr></p:cxnSp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Input output"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="1905000" y="1016000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:prstGeom prst="flowChartInputOutput"/><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/logo.svg"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/media/logo.svg",
                br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="#336699"/></svg>"##.to_vec(),
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/svg+xml;base64,"));
    assert!(svg.contains("data-source-id=\"SVG logo\""));
    assert!(svg.contains("data-source-id=\"Straight connector\""));
    assert!(svg.contains("d=\"M 10 80 L 110 100\""));
    assert!(svg.contains("data-source-id=\"Input output\""));
    assert!(svg.contains("d=\"M 170 80 L 250 80 L 230 130 L 150 130 Z\""));
}

#[test]
fn selects_svg_choice_without_rendering_alternate_content_fallback() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("hybrid-svg.pptx");
    let output = temporary.path().join("out");
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r" xmlns:mc="mc" xmlns:asvg="asvg"><p:cSld><p:spTree><mc:AlternateContent><mc:Choice Requires="asvg"><p:pic><p:nvPicPr><p:cNvPr id="2" name="Hybrid SVG"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"><a:extLst><a:ext><asvg:svgBlip r:embed="rId1"/></a:ext></a:extLst></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></mc:Choice><mc:Fallback><p:pic><p:nvPicPr><p:cNvPr id="2" name="Hybrid SVG"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></mc:Fallback></mc:AlternateContent></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.svg"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/media/image1.svg",
                br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="5"><rect width="10" height="5" fill="#336699"/></svg>"##.to_vec(),
            ),
            (
                "ppt/media/image1.png",
                base64::engine::general_purpose::STANDARD
                    .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
                    .unwrap(),
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"image\"").count(), 1);
    assert_eq!(svg.matches("data:image/svg+xml;base64,").count(), 2);
    assert!(!svg.contains("data:image/png;base64,"));
}

#[test]
fn warns_and_renders_placeholder_for_unsupported_emf_picture() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("emf-picture.pptx");
    let output = temporary.path().join("out");
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="2" name="Legacy diagram"/></p:nvPicPr><p:blipFill><a:blip r:embed="rId1"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.emf"/></Relationships>"#.to_vec(),
            ),
            ("ppt/media/image1.emf", b"not-a-renderable-emf".to_vec()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("uses EMF"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("EMF preview unavailable"));
    assert!(svg.contains("unsupported-vector-image-placeholder"));
    assert!(!svg.contains("data:image/x-emf"));
}

#[test]
fn converts_pptx_src_rect_crop_with_rotation_group_and_negative_edges() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("picture-crops.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="1" name="Rotated crop"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId1"/><a:srcRect l="25000" t="10000" r="25000" b="10000"/></p:blipFill><p:spPr><a:xfrm rot="1800000"><a:off x="127000" y="254000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:effectLst><a:outerShdw blurRad="12700" dist="12700" dir="0"><a:srgbClr val="000000"><a:alpha val="40000"/></a:srgbClr></a:outerShdw></a:effectLst></p:spPr></p:pic><p:grpSp><p:nvGrpSpPr/><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2540000" cy="1270000"/><a:chOff x="0" y="0"/><a:chExt cx="1270000" cy="1270000"/></a:xfrm></p:grpSpPr><p:pic><p:nvPicPr><p:cNvPr id="2" name="Grouped negative crop"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"/><a:srcRect l="-25000" r="25000"/></p:blipFill><p:spPr><a:xfrm><a:off x="254000" y="381000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:grpSp><p:pic><p:nvPicPr><p:cNvPr id="3" name="Invalid crop"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId3"/><a:srcRect l="90000" r="20000"/></p:blipFill><p:spPr><a:xfrm><a:off x="3810000" y="254000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("ppt/media/image1.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("Invalid crop crop was ignored"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("x=\"-40\" y=\"10\" width=\"200\" height=\"100\""));
    assert!(svg.contains("<g id=\"pptx-slide-1-1\""));
    assert!(svg.contains("id=\"pptx-slide-1-1-source\""));
    assert!(svg.contains("filter=\"url(#outer-shadow-pptx-slide-1-1)\""));
    assert!(svg.contains("d=\"M 10 20 H 110 V 100 H 10 Z\""));
    assert!(svg.contains("x=\"45\" y=\"30\" width=\"100\" height=\"50\""));
    assert!(svg.contains("d=\"M 20 30 H 120 V 80 H 20 Z\""));
    assert!(svg.contains("x=\"300\" y=\"20\" width=\"100\" height=\"80\""));
    assert_eq!(svg.matches("<clipPath id=\"pptx-picture-crop-").count(), 2);
    assert!(svg.contains("transform=\"matrix(2 0 0 1 0 0)\""));
}

#[test]
fn converts_pptx_image_color_effects_and_composes_crop_shadow() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("image-effects.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="1" name="Duotone crop shadow"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId1"><a:duotone><a:schemeClr val="accent1"><a:shade val="50000"/></a:schemeClr><a:prstClr val="white"/></a:duotone></a:blip><a:srcRect l="10000" r="10000"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm><a:effectLst><a:outerShdw blurRad="12700" dist="12700" dir="0"><a:srgbClr val="000000"><a:alpha val="40000"/></a:srgbClr></a:outerShdw></a:effectLst></p:spPr></p:pic><p:pic><p:nvPicPr><p:cNvPr id="2" name="Grayscale"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId2"><a:grayscl/></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="1905000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic><p:pic><p:nvPicPr><p:cNvPr id="3" name="Luminance"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId3"><a:lum bright="12000" contrast="20000"/></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="3683000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic><p:pic><p:nvPicPr><p:cNvPr id="4" name="Transparent white"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId4"><a:clrChange><a:clrFrom><a:srgbClr val="FFFFFF"/></a:clrFrom><a:clrTo><a:srgbClr val="FFFFFF"><a:alpha val="0"/></a:srgbClr></a:clrTo></a:clrChange></a:blip></p:blipFill><p:spPr><a:xfrm><a:off x="5461000" y="127000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("ppt/media/image1.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"drawingml-effects-pptx-slide-1-1\""));
    assert!(svg.contains("filter=\"url(#drawingml-effects-pptx-slide-1-1)\""));
    assert!(svg.contains("feGaussianBlur in=\"image-effect-1\""));
    assert!(svg.contains("id=\"pptx-slide-1-1-source\""));
    assert!(svg.contains("id=\"image-effects-pptx-slide-1-2\""));
    assert!(svg.contains("id=\"image-effects-pptx-slide-1-3\""));
    assert!(svg.contains("slope=\"1.1\" intercept=\"0.01\""));
    assert!(svg.contains("id=\"image-effects-pptx-slide-1-4\""));
    assert!(svg.contains("mode=\"difference\""));
    assert!(svg.contains("flood-color=\"#FFFFFF\" flood-opacity=\"0\""));
    assert!(svg.contains("values=\"0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 255 255 255 0 0\""));
}

#[test]
fn converts_pptx_density_hatch_and_diamond_pattern_fills() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("patterns.pptx");
    let output = temporary.path().join("out");
    let presets = [
        "pct5", "pct90", "narHorz", "narVert", "wdUpDiag", "wdDnDiag", "dkUpDiag", "openDmnd",
    ];
    let mut shapes = String::new();
    for (index, preset) in presets.iter().enumerate() {
        let x = 127_000 + index as i64 * 762_000;
        let foreground = if index == 0 {
            r#"<a:srgbClr val="FF0000"><a:tint val="50000"/><a:alpha val="50000"/></a:srgbClr>"#
        } else if index == 1 {
            r#"<a:sysClr val="windowText" lastClr="00AA00"/>"#
        } else {
            r#"<a:schemeClr val="accent1"/>"#
        };
        let shape = format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="{}" name="Pattern {}"/></p:nvSpPr><p:spPr><a:xfrm{}><a:off x="{}" y="254000"/><a:ext cx="635000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:pattFill prst="{}"><a:fgClr>{}</a:fgClr><a:bgClr><a:schemeClr val="bg1"/></a:bgClr></a:pattFill></p:spPr></p:sp>"#,
            index + 1,
            preset,
            if index == 3 { " rot=\"1800000\"" } else { "" },
            x,
            preset,
            foreground,
        );
        if index == 2 {
            shapes.push_str(
                r#"<p:grpSp><p:nvGrpSpPr/><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="12700000" cy="1270000"/><a:chOff x="0" y="0"/><a:chExt cx="6350000" cy="1270000"/></a:xfrm></p:grpSpPr>"#,
            );
            shapes.push_str(&shape);
            shapes.push_str("</p:grpSp>");
        } else {
            shapes.push_str(&shape);
        }
    }
    let slide = format!(
        r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#
    );
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            ("ppt/slides/slide1.xml", &slide),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(
        svg.matches("<pattern id=\"pptx-pattern-slide-1-").count(),
        8
    );
    assert_eq!(svg.matches("fill=\"url(#pptx-pattern-slide-1-").count(), 8);
    assert!(svg.contains("fill=\"#FF8080\" fill-opacity=\"0.5\""));
    assert!(svg.contains("fill=\"#00AA00\" fill-opacity=\"1\""));
    assert!(svg.contains("data-content-kind=\"drawingml-pattern-cell\""));
    assert!(svg.contains("data-content-kind=\"drawingml-pattern-line\""));
    assert!(svg.contains("stroke-width=\"3\""));
    assert!(svg.contains("M 0 6 L 6 0 L 12 6 L 6 12 Z"));
    assert!(svg.contains("transform=\"matrix(2 0 0 1 0 0)\""));
}

#[test]
fn converts_high_frequency_pptx_preset_geometries() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("preset-geometries.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:grpSp><p:nvGrpSpPr/><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="18288000" cy="6858000"/><a:chOff x="0" y="0"/><a:chExt cx="9144000" cy="6858000"/></a:xfrm></p:grpSpPr><p:sp><p:nvSpPr><p:cNvPr id="1" name="Right brace"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="381000" cy="1270000"/></a:xfrm><a:prstGeom prst="rightBrace"><a:avLst><a:gd name="adj1" fmla="val 25000"/><a:gd name="adj2" fmla="val 40000"/></a:avLst></a:prstGeom><a:noFill/><a:ln><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></p:spPr></p:sp></p:grpSp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Left brace"/></p:nvSpPr><p:spPr><a:xfrm rot="1800000"><a:off x="762000" y="127000"/><a:ext cx="381000" cy="1270000"/></a:xfrm><a:prstGeom prst="leftBrace"><a:avLst><a:gd name="adj1" fmla="val 8333"/><a:gd name="adj2" fmla="val 50000"/></a:avLst></a:prstGeom><a:noFill/><a:ln><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Arc"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="1397000" y="127000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:prstGeom prst="arc"><a:avLst><a:gd name="adj1" fmla="val 16200000"/><a:gd name="adj2" fmla="val 0"/></a:avLst></a:prstGeom><a:noFill/><a:ln><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></a:ln></p:spPr></p:sp><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="4" name="Bent connector"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="2921000" y="127000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:prstGeom prst="bentConnector3"><a:avLst><a:gd name="adj1" fmla="val 25000"/></a:avLst></a:prstGeom></p:spPr></p:cxnSp><p:sp><p:nvSpPr><p:cNvPr id="5" name="Manual input"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="4445000" y="127000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:prstGeom prst="flowChartManualInput"/><a:solidFill><a:srgbClr val="FFFF00"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="6" name="Cylinder"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="5969000" y="127000"/><a:ext cx="1016000" cy="1016000"/></a:xfrm><a:prstGeom prst="can"><a:avLst><a:gd name="adj" fmla="val 25000"/></a:avLst></a:prstGeom><a:solidFill><a:srgbClr val="00AAFF"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="7" name="Decision"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="7366000" y="127000"/><a:ext cx="1016000" cy="1016000"/></a:xfrm><a:prstGeom prst="flowChartDecision"/><a:solidFill><a:srgbClr val="00AA00"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="8" name="Extract"/></p:nvSpPr><p:spPr><a:xfrm rot="10800000"><a:off x="127000" y="1905000"/><a:ext cx="1270000" cy="1016000"/></a:xfrm><a:prstGeom prst="flowChartExtract"/><a:solidFill><a:srgbClr val="FFAA00"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"vector\"").count(), 8);
    assert!(svg.contains("data-source-id=\"Right brace\""));
    assert!(svg.contains("data-source-id=\"Left brace\""));
    assert!(svg.contains("M 160 10 A 50 40 0 0 1 210 50"));
    assert!(svg.contains("M 230 10 L 255 10 L 255 90 L 330 90"));
    assert!(svg.contains("M 372 10 L 450 10 L 450 90 L 350 90 Z"));
    assert!(svg.contains("data-source-id=\"Cylinder\""));
    assert!(svg.contains("M 620 10 L 660 50 L 620 90 L 580 50 Z"));
    assert!(svg.contains("M 60 150 L 110 230 L 10 230 Z"));
    assert!(svg.contains("transform=\"matrix(2 0 0 1 0 0)\""));
}

#[test]
fn converts_second_batch_pptx_preset_geometries() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("preset-geometries-2.pptx");
    let output = temporary.path().join("out");
    let presets = [
        "moon",
        "bentArrow",
        "flowChartDelay",
        "flowChartConnector",
        "flowChartCollate",
        "flowChartProcess",
        "cube",
        "leftBracket",
        "uturnArrow",
        "donut",
        "curvedLeftArrow",
        "curvedUpArrow",
        "curvedDownArrow",
        "circularArrow",
        "upDownArrow",
        "bracketPair",
        "snip2SameRect",
        "homePlate",
        "bentUpArrow",
        "corner",
        "rightBracket",
        "leftRightArrow",
        "wave",
        "actionButtonForwardNext",
        "mathPlus",
        "irregularSeal1",
        "irregularSeal2",
        "wedgeRoundRectCallout",
        "wedgeEllipseCallout",
        "borderCallout1",
        "borderCallout2",
    ];
    let mut shapes = String::new();
    for (index, preset) in presets.iter().enumerate() {
        let column = index % 10;
        let row = index / 10;
        let x = 127_000 + column as i64 * 889_000;
        let y = 127_000 + row as i64 * 1_270_000;
        let adjustments = match *preset {
            "moon" => r#"<a:gd name="adj" fmla="val 37454"/>"#,
            "cube" => r#"<a:gd name="adj" fmla="val 81676"/>"#,
            "snip2SameRect" => {
                r#"<a:gd name="adj1" fmla="val 22750"/><a:gd name="adj2" fmla="val 29251"/>"#
            }
            "corner" => {
                r#"<a:gd name="adj1" fmla="val 30000"/><a:gd name="adj2" fmla="val 40000"/>"#
            }
            "wave" => r#"<a:gd name="adj1" fmla="val 5332"/><a:gd name="adj2" fmla="val 0"/>"#,
            _ => "",
        };
        let open_shape = matches!(
            *preset,
            "moon" | "leftBracket" | "rightBracket" | "bracketPair"
        );
        shapes.push_str(&format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="{}" name="Preset {}"/></p:nvSpPr><p:spPr><a:xfrm{}><a:off x="{}" y="{}"/><a:ext cx="762000" cy="762000"/></a:xfrm><a:prstGeom prst="{}"><a:avLst>{}</a:avLst></a:prstGeom>{}<a:ln><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:ln></p:spPr></p:sp>"#,
            index + 1,
            preset,
            if index % 3 == 0 { " rot=\"1800000\"" } else { "" },
            x,
            y,
            preset,
            adjustments,
            if open_shape {
                "<a:noFill/>"
            } else {
                "<a:solidFill><a:srgbClr val=\"66CCFF\"/></a:solidFill>"
            },
        ));
    }
    let slide = format!(
        r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#
    );
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            ("ppt/slides/slide1.xml", &slide),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(
        svg.matches("data-content-kind=\"vector\"").count(),
        presets.len()
    );
    for preset in presets {
        assert!(
            svg.contains(&format!("data-source-id=\"Preset {preset}\"")),
            "missing {preset}"
        );
    }
    assert!(svg.contains("A 30 30 0 1 0"));
    assert!(svg.contains("data-source-id=\"Preset cube\""));
    assert!(svg.contains("data-source-id=\"Preset donut\""));
    assert!(svg.contains("data-source-id=\"Preset wave\""));
}

#[test]
fn converts_pptx_adjustable_wedge_rect_callout() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("wedge-callout.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Adjusted callout"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="1524000" cy="762000"/></a:xfrm><a:prstGeom prst="wedgeRectCallout"><a:avLst><a:gd name="adj1" fmla="val 0"/><a:gd name="adj2" fmla="val 100000"/></a:avLst></a:prstGeom><a:solidFill><a:srgbClr val="FFFF00"/></a:solidFill></p:spPr><p:style><a:lnRef idx="2"><a:schemeClr val="accent1"><a:shade val="50000"/></a:schemeClr></a:lnRef><a:fillRef idx="1"><a:schemeClr val="accent1"/></a:fillRef></p:style></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Adjusted callout\""));
    assert!(svg.contains("data-content-kind=\"vector\""));
    assert!(svg.contains("L 70 110"));
    assert!(svg.contains("fill=\"#FFFF00\""));
}

#[test]
fn converts_pptx_custom_geometry_to_editable_svg_path() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("custom-geometry.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Custom wave"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="1270000" cy="1270000"/></a:xfrm><a:custGeom><a:avLst/><a:gdLst><a:gd name="xMid" fmla="*/ w 1 2"/></a:gdLst><a:ahLst/><a:cxnLst/><a:rect l="l" t="t" r="r" b="b"/><a:pathLst><a:path w="21600" h="21600"><a:moveTo><a:pt x="0" y="21600"/></a:moveTo><a:lnTo><a:pt x="xMid" y="0"/></a:lnTo><a:cubicBezTo><a:pt x="16200" y="0"/><a:pt x="21600" y="5400"/><a:pt x="21600" y="10800"/></a:cubicBezTo><a:quadBezTo><a:pt x="10800" y="21600"/><a:pt x="0" y="21600"/></a:quadBezTo><a:close/></a:path><a:path w="21600" h="21600"><a:moveTo><a:pt x="10800" y="0"/></a:moveTo><a:arcTo wR="10800" hR="10800" stAng="16200000" swAng="21600000"/><a:close/></a:path></a:pathLst></a:custGeom><a:solidFill><a:srgbClr val="336699"/></a:solidFill></p:spPr></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Custom wave\""));
    assert!(svg.contains("M 10 110 L 60 10 C 85 10 110 35 110 60 Q 60 110 10 110 Z"));
    assert!(svg.matches(" A 50 50").count() >= 2);
}

#[test]
fn converts_pptx_cached_smartart_drawing() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("smartart.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="2540000" cy="1524000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:dgm="dgm" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Process diagram"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="1270000" cy="635000"/></p:xfrm><a:graphic><a:graphicData><dgm:relIds r:dm="rId2"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData" Target="../diagrams/data1.xml"/><Relationship Id="rId6" Type="http://schemas.microsoft.com/office/2007/relationships/diagramDrawing" Target="../diagrams/drawing1.xml"/></Relationships>"#,
            ),
            (
                "ppt/diagrams/data1.xml",
                r#"<dgm:dataModel xmlns:dgm="dgm" xmlns:dsp="dsp"><dgm:extLst><dgm:ext><dsp:dataModelExt relId="rId6"/></dgm:ext></dgm:extLst></dgm:dataModel>"#,
            ),
            (
                "ppt/diagrams/drawing1.xml",
                r#"<dsp:drawing xmlns:dsp="dsp" xmlns:a="a"><dsp:spTree><dsp:sp><dsp:nvSpPr><dsp:cNvPr id="1" name="Node"/></dsp:nvSpPr><dsp:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="508000" cy="508000"/></a:xfrm><a:prstGeom prst="ellipse"/><a:solidFill><a:schemeClr val="accent4"><a:alpha val="50000"/></a:schemeClr></a:solidFill><a:ln><a:solidFill><a:schemeClr val="lt1"/></a:solidFill></a:ln></dsp:spPr><dsp:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="ctr"/><a:r><a:rPr sz="1100"/><a:t>Smart node</a:t></a:r></a:p></dsp:txBody><dsp:txXfrm><a:off x="190500" y="279400"/><a:ext cx="381000" cy="127000"/></dsp:txXfrm></dsp:sp></dsp:spTree></dsp:drawing>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"smartart\""));
    assert!(svg.contains("data-semantic-role=\"diagram\""));
    assert!(svg.contains("fill=\"#FFC000\" fill-opacity=\"0.5\""));
    assert!(svg.contains("stroke=\"#FFFFFF\""));
    assert!(svg.contains(">Smart</tspan>"));
    assert!(svg.contains(">node</tspan>"));
}

#[test]
fn converts_pptx_native_table_cells_text_and_merges() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("table.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Native table"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/table"><a:tbl><a:tblPr firstRow="1" bandRow="1"/><a:tblGrid><a:gridCol w="1016000"/><a:gridCol w="1524000"/></a:tblGrid><a:tr h="508000"><a:tc><a:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="ctr"/><a:r><a:rPr b="1" sz="1200"><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill><a:effectLst><a:outerShdw blurRad="12700" dist="12700" dir="0"><a:srgbClr val="000000"><a:alpha val="40000"/></a:srgbClr></a:outerShdw><a:glow rad="12700"><a:srgbClr val="00AAFF"><a:alpha val="30000"/></a:srgbClr></a:glow></a:effectLst></a:rPr><a:t>Alpha Beta Gamma</a:t></a:r></a:p></a:txBody><a:tcPr><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:tcPr></a:tc><a:tc><a:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="ctr"/><a:r><a:rPr b="1" sz="1200"><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a:rPr><a:t>Head B</a:t></a:r></a:p></a:txBody><a:tcPr><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:tcPr></a:tc></a:tr><a:tr h="762000"><a:tc gridSpan="2"><a:txBody><a:bodyPr anchor="ctr"/><a:p><a:pPr algn="l"/><a:r><a:rPr sz="1100"><a:solidFill><a:srgbClr val="223344"/></a:solidFill></a:rPr><a:t>Merged body</a:t></a:r></a:p></a:txBody><a:tcPr><a:gridSpan val="2"/><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill><a:lnL><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:lnL></a:tcPr></a:tc><a:tc hMerge="1"><a:txBody><a:bodyPr/><a:p/></a:txBody><a:tcPr><a:hMerge/></a:tcPr></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"table-cell\"").count(), 3);
    assert_eq!(
        svg.matches("data-content-kind=\"table-cell-text\"").count(),
        4
    );
    assert!(svg.contains("data-source-id=\"Native table!R1C1\""));
    assert!(svg.contains("fill=\"#112233\""));
    assert!(svg.contains("fill=\"#DDEEFF\""));
    assert!(svg.contains("font-weight=\"700\""));
    assert!(svg.contains("Alpha Beta"));
    assert!(svg.contains("Gamma"));
    assert!(svg.contains("Merged body"));
    assert!(svg.contains("d=\"M 10 60 H 210 V 120 H 10 Z\""));
    assert!(svg.contains("clip-path=\"url(#pptx-table-1-1-clip-2-1)\""));
    assert_eq!(svg.matches("filter=\"url(#drawingml-effects-").count(), 2);
    assert!(svg.contains("stdDeviation=\"0.5\""));
    assert!(svg.contains("flood-opacity=\"0.4\""));
    assert!(svg.contains("flood-color=\"#00AAFF\" flood-opacity=\"0.3\""));
}

#[test]
fn recovers_pptx_zero_height_table_rows_from_frame_extent() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("zero-row-table.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Zero row table"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData><a:tbl><a:tblPr/><a:tblGrid><a:gridCol w="1270000"/><a:gridCol w="1270000"/></a:tblGrid><a:tr h="0"><a:tc><a:txBody><a:p><a:r><a:t>A</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc><a:tc><a:txBody><a:p><a:r><a:t>B</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr><a:tr h="0"><a:tc><a:txBody><a:p><a:r><a:t>C</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc><a:tc><a:txBody><a:p><a:r><a:t>D</a:t></a:r></a:p></a:txBody><a:tcPr/></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"table-cell\"").count(), 4);
    assert!(svg.contains("d=\"M 10 20 H 110 V 70 H 10 Z\""));
    assert!(svg.contains("d=\"M 10 70 H 110 V 120 H 10 Z\""));
    assert!(svg.contains(">A</tspan>"));
    assert!(svg.contains(">D</tspan>"));
}

#[test]
fn applies_pptx_group_coordinate_transform() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("group-transform.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:grpSp><p:nvGrpSpPr/><p:grpSpPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/><a:chOff x="0" y="0"/><a:chExt cx="1270000" cy="1270000"/></a:xfrm></p:grpSpPr><p:sp><p:nvSpPr><p:cNvPr id="1" name="Grouped rectangle"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="254000" cy="254000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="123456"/></a:solidFill></p:spPr></p:sp></p:grpSp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Grouped rectangle\""));
    assert!(svg.contains("d=\"M 10 10 H 30 V 30 H 10 Z\""));
    assert!(svg.contains("transform=\"matrix(2 0 0 1 10 20)\""));
}

#[test]
fn renders_pptx_embedded_video_as_static_poster() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("video.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r" xmlns:p14="p14"><p:cSld><p:spTree><p:pic><p:nvPicPr><p:cNvPr id="1" name="Demo video" descr="Video preview"/><p:cNvPicPr/><p:nvPr><a:videoFile r:link="rId2"/><p14:media r:embed="rId2"/></p:nvPr></p:nvPicPr><p:blipFill><a:blip r:embed="rId1"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="1270000" cy="635000"/></a:xfrm></p:spPr></p:pic></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/poster.png"/><Relationship Id="rId2" Type="http://schemas.microsoft.com/office/2007/relationships/media" Target="../media/video1.mp4"/></Relationships>"#.to_vec(),
            ),
            ("ppt/media/poster.png", png),
            ("ppt/media/video1.mp4", b"not-decoded-by-static-svg".to_vec()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("rendered as a static poster"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"video-poster\""));
    assert!(svg.contains("data-semantic-role=\"video\""));
    assert!(svg.contains("data-source-id=\"Demo video\""));
    assert!(svg.contains("data:image/png;base64,"));
}

#[test]
fn preserves_pptx_ole_frame_as_static_placeholder_without_preview() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ole.pptx");
    let output = temporary.path().join("out");
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Budget workbook"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData><p:oleObj r:id="rId2" name="Budget workbook" progId="Excel.Sheet.12"><p:embed/></p:oleObj></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject" Target="../embeddings/workbook1.xlsx"/></Relationships>"#.to_vec(),
            ),
            ("ppt/embeddings/workbook1.xlsx", b"opaque-ole-payload".to_vec()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("rendered as a static placeholder"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"embedded-object-placeholder\""));
    assert!(svg.contains("data-semantic-role=\"embedded-object\""));
    assert!(svg.contains("data-source-id=\"Budget workbook\""));
    assert!(svg.contains("d=\"M 10 20 H 210 V 120 H 10 Z\""));
    assert!(svg.contains(">Budget workbook</tspan>"));
}

#[test]
fn preserves_pptx_ole_static_preview_without_duplicate_placeholder() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ole-preview.pptx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "ppt/presentation.xml",
                br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#.to_vec(),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "ppt/slides/slide1.xml",
                br#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Workbook frame"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></p:xfrm><a:graphic><a:graphicData><p:oleObj r:id="rId2" name="Workbook preview" progId="Excel.Sheet.12"><p:embed/><p:pic><p:nvPicPr><p:cNvPr id="2" name="Workbook poster"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed="rId3"/></p:blipFill><p:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="2540000" cy="1270000"/></a:xfrm></p:spPr></p:pic></p:oleObj></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#.to_vec(),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                br#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject" Target="../embeddings/workbook1.xlsx"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/ole-preview.png"/></Relationships>"#.to_vec(),
            ),
            ("ppt/embeddings/workbook1.xlsx", b"opaque-ole-payload".to_vec()),
            ("ppt/media/ole-preview.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("rendered as a static preview"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"embedded-object-preview\""));
    assert!(svg.contains("data-semantic-role=\"embedded-object\""));
    assert!(svg.contains("data-source-id=\"Workbook poster\""));
    assert!(svg.contains("data:image/png;base64,"));
    assert!(!svg.contains("embedded-object-placeholder"));
}

#[test]
fn converts_pptx_cached_line_chart() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("chart.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:c="c" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Trend chart"/></p:nvGraphicFramePr><p:xfrm><a:off x="127000" y="127000"/><a:ext cx="3810000" cy="2540000"/></p:xfrm><a:graphic><a:graphicData><c:chart r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Trend</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:lineChart><c:ser><c:tx><c:v>Series 1</c:v></c:tx><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>1</c:v></c:pt><c:pt idx="1"><c:v>3</c:v></c:pt><c:pt idx="2"><c:v>2</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">Trend</tspan>"));
    assert!(svg.contains("data-content-kind=\"chart-line\""));
}

#[test]
fn converts_minimal_xlsx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Hello XLSX</t></is></c><c r="B1"><v>42</v></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Xlsx);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello XLSX"));
    assert!(svg.contains(">42</tspan>"));
}

#[test]
fn formats_xlsx_dates_currency_and_percent_values() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("formatted.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><workbookPr date1904="1"/><sheets><sheet name="Formats" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/styles.xml",
                r#"<styleSheet><numFmts count="1"><numFmt numFmtId="200" formatCode="¥#,##0.00"/></numFmts><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellXfs count="3"><xf numFmtId="14"/><xf numFmtId="200"/><xf numFmtId="10"/></cellXfs></styleSheet>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" s="0"><v>0</v></c><c r="B1" s="1"><v>12345.6</v></c><c r="C1" s="2"><v>0.125</v></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">1/1/04</tspan>"));
    assert!(svg.contains(">¥12,345.60</tspan>"));
    assert!(svg.contains(">12.50%</tspan>"));
}

#[test]
fn renders_xlsx_conditional_formats() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("conditional.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Conditional" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/styles.xml",
                r#"<styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellXfs count="1"><xf/></cellXfs><dxfs count="1"><dxf><font><color rgb="FFFF0000"/></font><fill><patternFill patternType="solid"><fgColor rgb="FFFFFF00"/></patternFill></fill></dxf></dxfs></styleSheet>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>1</v></c><c r="C1"><v>1</v></c></row><row r="2"><c r="A2"><v>5</v></c><c r="B2"><v>5</v></c><c r="C2"><v>5</v></c></row><row r="3"><c r="A3"><v>10</v></c><c r="B3"><v>10</v></c><c r="C3"><v>10</v></c></row></sheetData><conditionalFormatting sqref="A1:A3"><cfRule type="cellIs" dxfId="0" priority="1" stopIfTrue="1" operator="greaterThan"><formula>4</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="B1:B3"><cfRule type="colorScale" priority="2"><colorScale><cfvo type="min"/><cfvo type="percent" val="50"/><cfvo type="max"/><color rgb="FFFF0000"/><color rgb="FFFFFF00"/><color rgb="FF00FF00"/></colorScale></cfRule></conditionalFormatting><conditionalFormatting sqref="C1:C3"><cfRule type="dataBar" priority="3"><dataBar><cfvo type="min"/><cfvo type="max"/><color rgb="FF638EC6"/></dataBar><extLst><ext><x14:id>duplicate</x14:id></ext></extLst></cfRule></conditionalFormatting><extLst><ext><x14:conditionalFormattings><x14:conditionalFormatting><x14:cfRule type="dataBar" priority="3"><x14:dataBar><x14:cfvo type="min"/><x14:cfvo type="max"/><x14:fillColor rgb="FF638EC6"/></x14:dataBar></x14:cfRule><xm:sqref>C1:C3</xm:sqref></x14:conditionalFormatting></x14:conditionalFormattings></ext></extLst></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"cell-fill-2-1\""));
    assert!(svg.contains("fill=\"#FFFF00\""));
    assert!(svg.contains("id=\"cell-text-2-1\""));
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("id=\"cell-fill-1-2\""));
    assert!(svg.contains("id=\"cell-fill-3-2\""));
    assert_eq!(
        svg.matches("data-content-kind=\"conditional-data-bar\"")
            .count(),
        3
    );
    assert!(svg.contains("fill=\"#638EC6\" fill-opacity=\"0.65\""));
}

#[test]
fn renders_xlsx_blank_and_text_conditional_formats() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("text-conditional.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Text rules" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/styles.xml",
                r#"<styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellXfs count="1"><xf/></cellXfs><dxfs count="1"><dxf><font><color rgb="FFFF0000"/></font><fill><patternFill patternType="solid"><fgColor rgb="FFFFFF00"/></patternFill></fill></dxf></dxfs></styleSheet>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1"><v></v></c><c r="B1" t="inlineStr"><is><t>Needs Review</t></is></c><c r="C1" t="inlineStr"><is><t>yes</t></is></c></row><row r="2"><c r="C2" t="inlineStr"><is><t>no</t></is></c></row></sheetData><conditionalFormatting sqref="A1"><cfRule type="containsBlanks" dxfId="0" priority="1"><formula>LEN(TRIM(A1))=0</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="B1"><cfRule type="containsText" operator="containsText" text="review" dxfId="0" priority="2"><formula>NOT(ISERROR(SEARCH("review",B1)))</formula></cfRule></conditionalFormatting><conditionalFormatting sqref="C1:C2"><cfRule type="expression" dxfId="0" priority="3"><formula>OR($C$1:$C$2="yes")</formula></cfRule></conditionalFormatting></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"cell-fill-1-1\""));
    assert!(svg.contains("id=\"cell-fill-1-2\""));
    assert!(svg.contains("id=\"cell-fill-1-3\""));
    assert!(svg.contains("id=\"cell-fill-2-3\""));
    assert_eq!(svg.matches("fill=\"#FFFF00\"").count(), 4);
    assert!(svg.contains("id=\"cell-text-1-2\""));
    assert!(svg.contains("fill=\"#FF0000\""));
}

#[test]
fn evaluates_xlsx_formulas_when_cached_values_are_missing() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("formulas.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Formula" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1"><v>2</v></c><c r="B1"><f>SUM(A1:A2)</f></c><c r="C1" t="inlineStr"><is><t>Visible</t></is></c><c r="E1" t="inlineStr"><is><t>Key A</t></is></c><c r="F1" t="inlineStr"><is><t>Result A</t></is></c></row><row r="2"><c r="A2"><v>3</v></c><c r="B2"><f>A1*A2+1</f></c><c r="C2"><f>IF(C1&lt;&gt;"",C1,"")</f></c><c r="E2" t="inlineStr"><is><t>Key B</t></is></c><c r="F2" t="inlineStr"><is><t>Result B</t></is></c></row><row r="3"><c r="B3"><f>IF(6&lt;B2,ROUND(9.6,0),0)</f></c><c r="C3"><f>COUNTA(C1:C2)</f></c></row><row r="4"><c r="C4"><f>IF(D1="","",D1)</f></c></row><row r="5"><c r="C5"><f>VLOOKUP("Key B",E1:F2,2,0)</f></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Formula!B1\""));
    assert!(svg.contains(">5</tspan>"));
    assert!(svg.contains("data-source-id=\"Formula!B2\""));
    assert!(svg.contains(">7</tspan>"));
    assert!(svg.contains("data-source-id=\"Formula!B3\""));
    assert!(svg.contains(">10</tspan>"));
    assert!(svg.contains("data-source-id=\"Formula!C2\""));
    assert!(svg.contains(">Visible</tspan>"));
    assert!(svg.contains("data-source-id=\"Formula!C3\""));
    assert!(svg.contains(">2</tspan>"));
    assert!(!svg.contains("data-source-id=\"Formula!C4\""));
    assert!(svg.contains("data-source-id=\"Formula!C5\""));
    assert!(svg.contains(">Result B</tspan>"));
}

#[test]
fn evaluates_xlsx_cross_sheet_and_defined_name_formulas() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("cross-sheet-formulas.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Main" sheetId="1" r:id="rId1"/><sheet name="Other Sheet" sheetId="2" r:id="rId2"/></sheets><definedNames><definedName name="LocalName">'Main'!$A$1</definedName><definedName name="ExternalName">'Other Sheet'!$A$1</definedName></definedNames></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Local</t></is></c><c r="B1"><f>IF('Other Sheet'!A1&lt;&gt;"",'Other Sheet'!A1,"")</f></c><c r="C1"><f>IF(LocalName="Local","Named","")</f></c><c r="D1"><f>IF(ExternalName="External","External named","")</f></c></row></sheetData></worksheet>"#,
            ),
            (
                "xl/worksheets/sheet2.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>External</t></is></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 2);
    let main = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(main.contains("data-source-id=\"Main!B1\""));
    assert!(main.contains(">External</tspan>"));
    assert!(main.contains("data-source-id=\"Main!C1\""));
    assert!(main.contains(">Named</tspan>"));
    assert!(main.contains("data-source-id=\"Main!D1\""));
    assert!(main.contains(">External named</tspan>"));
}

#[test]
fn evaluates_xlsx_conditional_aggregate_formulas_without_cached_values() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("conditional-formulas.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Criteria" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>East</t></is></c><c r="B1"><v>10</v></c><c r="C1"><f>COUNTIF(A1:A3,"e*")</f></c><c r="D1"><f>COUNTIFS(A1:A3,"east",B1:B3,"&gt;20")</f></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>West</t></is></c><c r="B2"><v>20</v></c><c r="C2"><f>SUMIF(A1:A3,"east",B1:B3)</f></c><c r="D2"><f>SUMIFS(B1:B3,A1:A3,"east",B1:B3,"&gt;10")</f></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>East</t></is></c><c r="B3"><v>30</v></c><c r="C3"><f>AVERAGEIF(B1:B3,"&gt;10")</f></c><c r="D3"><f>AVERAGEIFS(B1:B3,A1:A3,"east")</f></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Criteria!C1\""));
    assert!(svg.contains(">2</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!C2\""));
    assert!(svg.contains(">40</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!C3\""));
    assert!(svg.contains(">25</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!D1\""));
    assert!(svg.contains(">1</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!D2\""));
    assert!(svg.contains(">30</tspan>"));
    assert!(svg.contains("data-source-id=\"Criteria!D3\""));
    assert!(svg.contains(">20</tspan>"));
}

#[test]
fn paginates_xlsx_print_area_and_manual_breaks() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("print-area.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Print" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">'Print'!$B$2:$D$6</definedName></definedNames></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="2"><c r="B2" t="inlineStr"><is><t>B2</t></is></c><c r="C2" t="inlineStr"><is><t>C2</t></is></c><c r="D2" t="inlineStr"><is><t>D2</t></is></c></row><row r="4"><c r="B4" t="inlineStr"><is><t>B4</t></is></c><c r="C4" t="inlineStr"><is><t>C4</t></is></c><c r="D4" t="inlineStr"><is><t>D4</t></is></c></row><row r="6"><c r="B6" t="inlineStr"><is><t>B6</t></is></c><c r="C6" t="inlineStr"><is><t>C6</t></is></c><c r="D6" t="inlineStr"><is><t>D6</t></is></c></row></sheetData><rowBreaks count="1" manualBreakCount="1"><brk id="3" min="1" max="3" man="1"/></rowBreaks><colBreaks count="1" manualBreakCount="1"><brk id="2" min="1" max="5" man="1"/></colBreaks></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.page_count, 4);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let third = fs::read_to_string(output.join("page-0003.svg")).unwrap();
    assert!(first.contains("<title>Print (1/4)</title>"));
    assert!(first.contains("width=\"48pt\" height=\"30pt\""));
    assert!(first.contains("Print!B2"));
    assert!(!first.contains("Print!C2"));
    assert!(third.contains("width=\"96pt\" height=\"30pt\""));
    assert!(third.contains("Print!C2"));
    assert!(third.contains("Print!D2"));
}

#[test]
fn repeats_xlsx_print_title_rows_and_columns_on_later_pages() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("print-titles.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Titles" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">'Titles'!$A$1:$D$6</definedName><definedName name="_xlnm.Print_Titles" localSheetId="0">'Titles'!$1:$1,'Titles'!$A:$A</definedName></definedNames></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>A1 title</t></is></c><c r="B1" t="inlineStr"><is><t>B1 title</t></is></c><c r="C1" t="inlineStr"><is><t>C1 title</t></is></c><c r="D1" t="inlineStr"><is><t>D1 title</t></is></c></row><row r="2"><c r="B2" t="inlineStr"><is><t>B2 omitted</t></is></c></row><row r="4"><c r="A4" t="inlineStr"><is><t>A4 title</t></is></c><c r="C4" t="inlineStr"><is><t>C4 body</t></is></c><c r="D4" t="inlineStr"><is><t>D4 body</t></is></c></row><row r="6"><c r="A6" t="inlineStr"><is><t>A6 title</t></is></c><c r="C6" t="inlineStr"><is><t>C6 body</t></is></c><c r="D6" t="inlineStr"><is><t>D6 body</t></is></c></row></sheetData><rowBreaks count="1" manualBreakCount="1"><brk id="3" min="0" max="3" man="1"/></rowBreaks><colBreaks count="1" manualBreakCount="1"><brk id="2" min="0" max="5" man="1"/></colBreaks></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 4);
    let fourth = fs::read_to_string(output.join("page-0004.svg")).unwrap();
    assert!(fourth.contains("<title>Titles (4/4)</title>"));
    assert!(fourth.contains("width=\"144pt\" height=\"60pt\""));
    assert!(fourth.contains("Titles!A1"));
    assert!(fourth.contains("Titles!C1"));
    assert!(fourth.contains("Titles!D1"));
    assert!(fourth.contains("Titles!A4"));
    assert!(fourth.contains("Titles!A6"));
    assert!(fourth.contains("Titles!C4"));
    assert!(fourth.contains("Titles!D6"));
    assert!(!fourth.contains("Titles!B1"));
    assert!(!fourth.contains("Titles!B2"));
    assert!(fourth.contains("transform=\"matrix(1 0 0 1 -48 0)\""));
    assert!(fourth.contains("transform=\"matrix(1 0 0 1 0 -30)\""));
    assert!(fourth.contains("transform=\"matrix(1 0 0 1 -48 -30)\""));
}

#[test]
fn auto_tiles_large_xlsx_without_a_print_area() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("large-sheet.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Large" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>First tile</t></is></c></row><row r="1100"><c r="A1100" t="inlineStr"><is><t>Second tile</t></is></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("<title>Large (1/2)</title>"));
    assert!(first.contains("width=\"48pt\" height=\"16380pt\""));
    assert!(first.contains("data-source-id=\"Large!A1\""));
    assert!(!first.contains("Large!A1100"));
    assert!(second.contains("<title>Large (2/2)</title>"));
    assert!(second.contains("width=\"48pt\" height=\"120pt\""));
    assert!(!second.contains("data-source-id=\"Large!A1\""));
    assert!(second.contains("Large!A1100"));
}

#[test]
fn auto_tiles_xlsx_by_the_grid_cell_budget() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("wide-grid.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Grid" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>First grid tile</t></is></c></row><row r="50"><c r="AX50" t="inlineStr"><is><t>Second grid tile</t></is></c></row></sheetData></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("width=\"2400pt\" height=\"600pt\""));
    assert!(first.contains("Grid!A1"));
    assert!(!first.contains("Grid!AX50"));
    assert!(second.contains("width=\"2400pt\" height=\"150pt\""));
    assert!(!second.contains("Grid!A1"));
    assert!(second.contains("Grid!AX50"));
}

#[test]
fn fits_xlsx_print_region_to_landscape_paper() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("fit-to-page.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Fit" sheetId="1" r:id="rId1"/></sheets><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">Fit!$A$1:$T$10</definedName></definedNames></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetPr><pageSetUpPr fitToPage="1"/></sheetPr><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Fit content</t></is></c></row></sheetData><printOptions horizontalCentered="1" verticalCentered="1"/><pageMargins left="0.5" right="0.5" top="0.5" bottom="0.5" header="0.2" footer="0.2"/><pageSetup paperSize="9" orientation="landscape" fitToWidth="1" fitToHeight="1"/></worksheet>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("width=\"841.89pt\" height=\"595.28pt\""));
    assert!(svg.contains("transform=\"matrix(0.80196875 0 0 0.80196875 36"));
    assert!(svg.contains("Fit!A1"));
}

#[test]
fn converts_xlsx_anchored_drawing_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("drawing.xlsx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "xl/workbook.xml",
                br#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_vec(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                br#"<worksheet xmlns:r="r"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Image</t></is></c></row></sheetData><drawing r:id="rId2"/></worksheet>"#.to_vec(),
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                br#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#.to_vec(),
            ),
            (
                "xl/drawings/drawing1.xml",
                br#"<xdr:wsDr xmlns:xdr="xdr" xmlns:a="a" xmlns:r="r"><xdr:oneCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>12700</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>12700</xdr:rowOff></xdr:from><xdr:ext cx="127000" cy="254000"/><xdr:pic><xdr:nvPicPr><xdr:cNvPr id="1" name="Logo" descr="One pixel logo"><a:extLst><a:ext uri="metadata"/></a:extLst></xdr:cNvPr></xdr:nvPicPr><xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic></xdr:oneCellAnchor><xdr:oneCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:ext cx="0" cy="0"/><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="2" name="Hidden control" hidden="1"/></xdr:nvSpPr><xdr:spPr><a:prstGeom prst="rect"/></xdr:spPr></xdr:sp></xdr:oneCellAnchor></xdr:wsDr>"#.to_vec(),
            ),
            (
                "xl/drawings/_rels/drawing1.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("xl/media/image1.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/png;base64,"));
    assert!(svg.contains("data-source-id=\"Logo\""));
    assert!(svg.contains("x=\"49\" y=\"16\" width=\"10\" height=\"20\""));
    assert!(!svg.contains("Hidden control"));
}

#[test]
fn recovers_xlsx_shape_local_geometry_when_cell_anchors_are_zero() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shape-fallback.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Shapes" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns:r="r"><sheetData><row r="5"><c r="G5"><v>1</v></c></row></sheetData><drawing r:id="rId2"/></worksheet>"#,
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#,
            ),
            (
                "xl/drawings/drawing1.xml",
                r#"<xdr:wsDr xmlns:xdr="xdr" xmlns:a="a"><xdr:twoCellAnchor><xdr:from><xdr:col>6</xdr:col><xdr:colOff>1000</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>6</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="1" name="Fallback arrow"/></xdr:nvSpPr><xdr:spPr><a:xfrm><a:off x="127000" y="254000"/><a:ext cx="254000" cy="127000"/></a:xfrm><a:prstGeom prst="rightArrow"/><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></xdr:spPr></xdr:sp></xdr:twoCellAnchor><xdr:twoCellAnchor><xdr:from><xdr:col>2</xdr:col><xdr:row>2</xdr:row></xdr:from><xdr:to><xdr:col>2</xdr:col><xdr:row>2</xdr:row></xdr:to><xdr:sp><xdr:nvSpPr><xdr:cNvPr id="2" name="Auto text box"/></xdr:nvSpPr><xdr:spPr><a:xfrm><a:off x="508000" y="508000"/><a:ext cx="0" cy="0"/></a:xfrm><a:prstGeom prst="rect"/><a:noFill/></xdr:spPr><xdr:txBody><a:p><a:r><a:rPr sz="1000"/><a:t>Auto box</a:t></a:r></a:p></xdr:txBody></xdr:sp></xdr:twoCellAnchor></xdr:wsDr>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-source-id=\"Fallback arrow\""));
    assert!(svg.contains("fill=\"#FF0000\""));
    assert!(svg.contains("M 10 22.5 H 22 V 20 L 30 25"));
    assert!(svg.contains("data-source-id=\"Auto text box\""));
    assert!(svg.contains(">Auto box</tspan>"));
}

#[test]
fn converts_xlsx_cached_bar_chart() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("chart.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns:r="r"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Chart</t></is></c></row></sheetData><drawing r:id="rId2"/></worksheet>"#,
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#,
            ),
            (
                "xl/drawings/drawing1.xml",
                r#"<xdr:wsDr xmlns:xdr="xdr" xmlns:a="a" xmlns:c="c" xmlns:r="r"><xdr:oneCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:ext cx="2540000" cy="1524000"/><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="1" name="Sales chart"/></xdr:nvGraphicFramePr><a:graphic><a:graphicData><c:chart r:id="rId1"/></a:graphicData></a:graphic></xdr:graphicFrame></xdr:oneCellAnchor></xdr:wsDr>"#,
            ),
            (
                "xl/drawings/_rels/drawing1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "xl/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Quarterly sales</a:t></a:r></a:p></c:rich></c:tx></c:title><c:plotArea><c:barChart><c:ser><c:tx><c:v>Sales</c:v></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Q1</c:v></c:pt><c:pt idx="1"><c:v>Q2</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>10</c:v></c:pt><c:pt idx="1"><c:v>20</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Quarterly sales"));
    assert!(svg.contains("data-content-kind=\"chart-bar\""));
    assert_eq!(svg.matches("data-content-kind=\"chart-bar\"").count(), 2);
}

#[test]
fn converts_minimal_docx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sample.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:rPr><w:b/><w:sz w:val="28"/></w:rPr><w:t>Hello DOCX</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Docx);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Hello DOCX"));
    assert!(svg.contains("font-weight=\"700\""));
}

#[test]
fn preserves_docx_words_and_script_font_fallbacks_when_wrapping() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("word-wrap.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:rPr><w:rFonts w:ascii="Times New Roman" w:eastAsia="SimSun"/><w:sz w:val="40"/></w:rPr><w:t>Alpha Beta Gamma</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="4000" w:h="6000"/><w:pgMar w:top="400" w:right="400" w:bottom="400" w:left="400"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("font-family=\"Times New Roman, SimSun\""));
    assert!(svg.contains(">Alpha Beta</tspan>"));
    assert!(svg.contains(">Gamma</tspan>"));
    assert!(!svg.contains(">Alpha Beta G"));
}

#[test]
fn resolves_docx_based_on_style_chains() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("style-chain.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/styles.xml",
                r#"<w:styles xmlns:w="w"><w:style w:type="paragraph" w:styleId="Base"><w:rPr><w:rFonts w:ascii="Arial"/><w:b/><w:sz w:val="28"/></w:rPr></w:style><w:style w:type="paragraph" w:styleId="Derived"><w:basedOn w:val="Base"/><w:pPr><w:jc w:val="center"/></w:pPr><w:rPr><w:i/></w:rPr></w:style></w:styles>"#,
            ),
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:pStyle w:val="Derived"/></w:pPr><w:r><w:t>Inherited style</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="4000" w:h="6000"/><w:pgMar w:top="400" w:right="400" w:bottom="400" w:left="400"/></w:sectPr></w:body></w:document>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("x=\"100\""));
    assert!(svg.contains("font-family=\"Arial\""));
    assert!(svg.contains("font-size=\"14\""));
    assert!(svg.contains("font-weight=\"700\""));
    assert!(svg.contains("font-style=\"italic\""));
}

#[test]
fn renders_docx_superscript_and_subscript_runs() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("vertical-runs.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:rPr><w:sz w:val="20"/></w:rPr><w:t>x</w:t></w:r><w:r><w:rPr><w:sz w:val="20"/><w:vertAlign w:val="superscript"/></w:rPr><w:t>2</w:t></w:r><w:r><w:rPr><w:sz w:val="20"/><w:vertAlign w:val="subscript"/></w:rPr><w:t>3</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="4000" w:h="6000"/><w:pgMar w:top="400" w:right="400" w:bottom="400" w:left="400"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">x</tspan>"));
    assert!(svg.contains("font-size=\"7.5\""));
    assert!(svg.contains("baseline-shift=\"3.5\""));
    assert!(svg.contains("baseline-shift=\"-2\""));
}

#[test]
fn renders_docx_omml_fraction_scripts_and_radical() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("math.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w" xmlns:m="m"><w:body><w:p><m:oMath><m:f><m:num><m:r><m:t>a</m:t></m:r></m:num><m:den><m:r><m:t>b</m:t></m:r></m:den></m:f><m:r><m:t>+</m:t></m:r><m:sSub><m:e><m:r><m:t>x</m:t></m:r></m:e><m:sub><m:r><m:t>i</m:t></m:r></m:sub></m:sSub><m:sSup><m:e><m:r><m:t>y</m:t></m:r></m:e><m:sup><m:r><m:t>2</m:t></m:r></m:sup></m:sSup><m:rad><m:radPr><m:degHide m:val="1"/></m:radPr><m:deg/><m:e><m:r><m:t>z</m:t></m:r></m:e></m:rad></m:oMath></w:p><w:sectPr><w:pgSz w:w="4000" w:h="6000"/><w:pgMar w:top="400" w:right="400" w:bottom="400" w:left="400"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("(a)/(b)+x"));
    assert!(svg.contains("baseline-shift=\"3.85\""));
    assert!(svg.contains("baseline-shift=\"-2.2\""));
    assert!(svg.contains("√(z)"));
}

#[test]
fn splits_docx_inline_page_breaks_inside_a_paragraph() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline-page-break.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Before break</w:t><w:br w:type="page"/><w:t>After break, page </w:t><w:fldSimple w:instr="PAGE"/></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("Before break"));
    assert!(!first.contains("After break"));
    assert!(!second.contains("Before break"));
    assert!(second.contains("After break, page 2"));
}

#[test]
fn repeats_docx_default_header_and_footer() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("header-footer.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w" xmlns:r="r"><w:body><w:p><w:r><w:t>Page one</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/><w:t>Page two</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rId1"/><w:footerReference w:type="default" r:id="rId2"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#,
            ),
            (
                "word/header1.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER TEXT</w:t></w:r></w:p><w:tbl><w:tblGrid><w:gridCol w:w="3600"/><w:gridCol w:w="3600"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>H1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>H2</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:hdr>"#,
            ),
            (
                "word/footer1.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>FOOTER TEXT</w:t></w:r></w:p></w:ftr>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 2);
    assert!(report.warnings.is_empty());
    for page in ["page-0001.svg", "page-0002.svg"] {
        let svg = fs::read_to_string(output.join(page)).unwrap();
        assert!(svg.contains("HEADER TEXT"));
        assert!(svg.contains("FOOTER TEXT"));
        assert!(svg.contains("data-content-kind=\"header\""));
        assert!(svg.contains("data-content-kind=\"footer\""));
        assert_eq!(
            svg.matches("data-content-kind=\"header-table-cell\"")
                .count(),
            2
        );
        assert!(svg.contains("H1"));
        assert!(svg.contains("H2"));
    }
}

#[test]
fn selects_docx_first_even_and_section_specific_stories() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("section-stories.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w" xmlns:r="r"><w:body><w:p><w:r><w:t>Section one first</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/><w:t>Section one even</w:t></w:r></w:p><w:p><w:pPr><w:sectPr><w:headerReference w:type="default" r:id="rH1D"/><w:headerReference w:type="first" r:id="rH1F"/><w:headerReference w:type="even" r:id="rH1E"/><w:footerReference w:type="default" r:id="rF1D"/><w:footerReference w:type="first" r:id="rF1F"/><w:footerReference w:type="even" r:id="rF1E"/><w:titlePg/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:pPr></w:p><w:p><w:r><w:t>Section two</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rH2"/><w:footerReference w:type="default" r:id="rF2"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rH1D" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header-default.xml"/><Relationship Id="rH1F" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header-first.xml"/><Relationship Id="rH1E" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header-even.xml"/><Relationship Id="rF1D" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer-default.xml"/><Relationship Id="rF1F" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer-first.xml"/><Relationship Id="rF1E" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer-even.xml"/><Relationship Id="rH2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header-section2.xml"/><Relationship Id="rF2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer-section2.xml"/></Relationships>"#,
            ),
            (
                "word/header-default.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER DEFAULT ONE</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/header-first.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER FIRST ONE</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/header-even.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER EVEN ONE</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/footer-default.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>FOOTER DEFAULT ONE</w:t></w:r></w:p></w:ftr>"#,
            ),
            (
                "word/footer-first.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>FOOTER FIRST ONE</w:t></w:r></w:p></w:ftr>"#,
            ),
            (
                "word/footer-even.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>FOOTER EVEN ONE</w:t></w:r></w:p></w:ftr>"#,
            ),
            (
                "word/header-section2.xml",
                r#"<w:hdr xmlns:w="w"><w:p><w:r><w:t>HEADER SECTION TWO</w:t></w:r></w:p></w:hdr>"#,
            ),
            (
                "word/footer-section2.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t>FOOTER SECTION TWO</w:t></w:r></w:p></w:ftr>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.page_count, 3);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    let third = fs::read_to_string(output.join("page-0003.svg")).unwrap();
    assert!(first.contains("HEADER FIRST ONE"));
    assert!(first.contains("FOOTER FIRST ONE"));
    assert!(!first.contains("HEADER DEFAULT ONE"));
    assert!(second.contains("HEADER EVEN ONE"));
    assert!(second.contains("FOOTER EVEN ONE"));
    assert!(!second.contains("HEADER FIRST ONE"));
    assert!(third.contains("HEADER SECTION TWO"));
    assert!(third.contains("FOOTER SECTION TWO"));
    assert!(!third.contains("HEADER EVEN ONE"));
}

#[test]
fn renders_docx_numbered_and_bulleted_lists() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("lists.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="10"/></w:numPr></w:pPr><w:r><w:t>First</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="10"/></w:numPr></w:pPr><w:r><w:t>Nested</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="10"/></w:numPr></w:pPr><w:r><w:t>Second</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="11"/></w:numPr></w:pPr><w:r><w:t>Bullet</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/numbering.xml",
                r#"<w:numbering xmlns:w="w"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1.%2)"/><w:pPr><w:ind w:left="1440" w:hanging="360"/></w:pPr></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/></w:lvl></w:abstractNum><w:num w:numId="10"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="11"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">1. First</tspan>"));
    assert!(svg.contains(">1.a) Nested</tspan>"));
    assert!(svg.contains(">2. Second</tspan>"));
    assert!(svg.contains(">• Bullet</tspan>"));
}

#[test]
fn renders_docx_footnotes_and_endnotes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("notes.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Body</w:t><w:footnoteReference w:id="2"/><w:t> and end</w:t><w:endnoteReference w:id="3"/><w:t> comment</w:t><w:commentReference w:id="4"/></w:r><w:del><w:r><w:delText>Deleted revision</w:delText></w:r></w:del><w:ins><w:r><w:t> Inserted revision</w:t></w:r></w:ins></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/footnotes.xml",
                r#"<w:footnotes xmlns:w="w"><w:footnote w:id="-1"><w:p/></w:footnote><w:footnote w:id="2"><w:p><w:r><w:t>Footnote detail</w:t></w:r></w:p></w:footnote></w:footnotes>"#,
            ),
            (
                "word/endnotes.xml",
                r#"<w:endnotes xmlns:w="w"><w:endnote w:id="3"><w:p><w:r><w:t>Endnote detail</w:t></w:r></w:p></w:endnote></w:endnotes>"#,
            ),
            (
                "word/comments.xml",
                r#"<w:comments xmlns:w="w"><w:comment w:id="4" w:author="Reviewer"><w:p><w:r><w:t>Comment detail</w:t></w:r></w:p></w:comment></w:comments>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("baseline-shift=\"3.85\""));
    assert!(svg.contains("data-content-kind=\"note-separator\""));
    assert!(svg.contains("data-content-kind=\"footnote\""));
    assert!(svg.contains("Footnote detail"));
    assert!(svg.contains("data-content-kind=\"endnote\""));
    assert!(svg.contains("Endnote detail"));
    assert!(svg.contains("data-content-kind=\"comment\""));
    assert!(svg.contains("Comment detail"));
    assert!(svg.contains("Inserted revision"));
    assert!(!svg.contains("Deleted revision"));
}

#[test]
fn renders_docx_drawingml_and_vml_text_boxes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("text-boxes.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r##"<w:document xmlns:w="w" xmlns:wp="wp" xmlns:a="a" xmlns:wps="wps" xmlns:v="v"><w:body><w:p><w:r><w:t>Anchor</w:t><w:drawing><wp:anchor><wp:positionH relativeFrom="page"><wp:posOffset>127000</wp:posOffset></wp:positionH><wp:positionV relativeFrom="page"><wp:posOffset>254000</wp:posOffset></wp:positionV><wp:extent cx="1270000" cy="635000"/><wp:docPr id="1" name="Modern box" descr="Modern text box"/><a:graphic><a:graphicData><wps:wsp><wps:spPr><a:solidFill><a:srgbClr val="DDEEFF"/></a:solidFill><a:ln w="12700"><a:solidFill><a:srgbClr val="112233"/></a:solidFill></a:ln></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>Modern textbox</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p><w:p><w:r><w:t>VML anchor</w:t><w:pict><v:shape style="position:absolute;margin-left:30pt;margin-top:40pt;width:120pt;height:55pt" fillcolor="#FFF2CC" strokecolor="#806000" strokeweight="1pt"><v:textbox><w:txbxContent><w:p><w:r><w:t>VML textbox</w:t></w:r></w:p></w:txbxContent></v:textbox></v:shape></w:pict></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"##,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(
        svg.matches("data-content-kind=\"floating-text-box\"")
            .count(),
        2
    );
    assert!(svg.contains("fill=\"#DDEEFF\""));
    assert!(svg.contains("stroke=\"#112233\""));
    assert!(svg.contains("Modern textbox"));
    assert!(svg.contains("fill=\"#FFF2CC\""));
    assert!(svg.contains("stroke=\"#806000\""));
    assert!(svg.contains("VML textbox"));
}

#[test]
fn renders_docx_multi_section_page_setups() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sections.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Portrait section</w:t></w:r></w:p><w:p><w:pPr><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:pPr></w:p><w:p><w:r><w:t>Landscape section</w:t></w:r></w:p><w:sectPr><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.page_count, 2);
    let portrait = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let landscape = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(portrait.contains("width=\"612pt\" height=\"792pt\""));
    assert!(portrait.contains("Portrait section"));
    assert!(landscape.contains("width=\"792pt\" height=\"612pt\""));
    assert!(landscape.contains("Landscape section"));
}

#[test]
fn positions_docx_floating_image() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("floating.docx");
    let output = temporary.path().join("out");
    let png = base64::engine::general_purpose::STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=")
        .unwrap();
    make_zip_bytes(
        &input,
        vec![
            (
                "word/document.xml",
                br#"<w:document xmlns:w="w" xmlns:wp="wp" xmlns:a="a" xmlns:r="r"><w:body><w:p><w:r><w:t>Body text</w:t></w:r><w:r><w:drawing><wp:anchor><wp:positionH relativeFrom="page"><wp:posOffset>127000</wp:posOffset></wp:positionH><wp:positionV relativeFrom="page"><wp:posOffset>254000</wp:posOffset></wp:positionV><wp:extent cx="254000" cy="127000"/><wp:docPr id="1" name="Floating logo" descr="Floating image"/><a:graphic><a:graphicData><a:blip r:embed="rId1"/></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#.to_vec(),
            ),
            (
                "word/_rels/document.xml.rels",
                br#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("word/media/image1.png", png),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty());
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"floating-image\""));
    assert!(svg.contains("x=\"10\" y=\"20\" width=\"20\" height=\"10\""));
}

#[test]
fn does_not_expose_internal_page_break_markers_from_docx_tables() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("table-break.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Before</w:t><w:br w:type="page"/><w:t>After</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(report.warnings[0].contains("table pagination may differ"));
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Before After"));
    assert!(!svg.contains("DOCSVG_BREAK"));
}

#[test]
fn renders_docx_grid_span_as_one_merged_cell() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("grid-span.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:tbl><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:tcPr><w:gridSpan w:val="2"/><w:shd w:fill="CCDDEE"/></w:tcPr><w:p><w:r><w:t>Merged heading</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(svg.matches("data-content-kind=\"table-cell\"").count(), 1);
    assert!(svg.contains("data-source-id=\"R1C1\""));
    assert!(svg.contains("Merged heading"));
}

#[test]
fn tiles_xlsx_sheets_that_outgrow_the_configured_paper() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("long.xlsx");
    let output = temporary.path().join("out");
    // 200 default-height rows are far taller than one sheet of letter paper.
    let rows = (1..=200)
        .map(|row| {
            format!(
                r#"<row r="{row}"><c r="A{row}" t="inlineStr"><is><t>row {row}</t></is></c></row>"#
            )
        })
        .collect::<String>();
    let sheet = format!(
        r#"<worksheet><sheetData>{rows}</sheetData><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75"/><pageSetup orientation="portrait"/></worksheet>"#
    );
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            ("xl/worksheets/sheet1.xml", sheet.as_str()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    // A sheet with a print setup must be split across pages rather than drawn
    // past the edge of the first one.
    assert!(
        report.page_count > 1,
        "expected the sheet to be paginated, got {} page(s)",
        report.page_count
    );
    let last_row = ">row 200</tspan>";
    let last_page = output.join(format!("page-{:04}.svg", report.page_count));
    assert!(fs::read_to_string(last_page).unwrap().contains(last_row));
    for page in 1..=report.page_count {
        let svg = fs::read_to_string(output.join(format!("page-{page:04}.svg"))).unwrap();
        assert!(svg.contains(r#"viewBox="0 0 612 792""#));
    }
}

#[test]
fn keeps_fit_to_page_worksheets_on_a_single_page() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("fitted.xlsx");
    let output = temporary.path().join("out");
    let rows = (1..=200)
        .map(|row| {
            format!(
                r#"<row r="{row}"><c r="A{row}" t="inlineStr"><is><t>row {row}</t></is></c></row>"#
            )
        })
        .collect::<String>();
    let sheet = format!(
        r#"<worksheet><sheetData>{rows}</sheetData><pageSetup orientation="portrait" fitToWidth="1" fitToHeight="1"/></worksheet>"#
    );
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns:r="r"><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            ("xl/worksheets/sheet1.xml", sheet.as_str()),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    // Fit-to-page asks for one sheet of paper, so scaling replaces tiling.
    assert_eq!(report.page_count, 1);
}

#[test]
fn keeps_bullet_and_paragraph_end_colors_out_of_the_shape_fill() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bullet-color.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<presentation xmlns:r="r"><sldSz cx="9144000" cy="6858000"/><sldIdLst><sldId id="256" r:id="rId1"/></sldIdLst></presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Box"/><p:cNvSpPr txBox="1"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="4000000" cy="1000000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom><a:solidFill><a:srgbClr val="304FFE"/></a:solidFill></p:spPr><p:txBody><a:bodyPr/><a:p><a:pPr><a:buClr><a:srgbClr val="000000"/></a:buClr><a:buNone/></a:pPr><a:r><a:rPr lang="en"/><a:t>Legible</a:t></a:r><a:endParaRPr><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a:endParaRPr></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The bullet color and the end-of-paragraph run color describe text, so
    // the shape keeps the fill its own spPr asked for.
    assert!(svg.contains("#304FFE"), "shape fill was overwritten: {svg}");
    assert!(svg.contains("Legible"));
}

#[test]
fn rejects_encrypted_office_documents_as_unsupported() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("locked.xlsx");
    let output = temporary.path().join("out");
    // Office stores a password-protected document in a compound file, not a ZIP.
    let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    bytes.extend_from_slice(&[0u8; 512]);
    fs::write(&input, bytes).unwrap();

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(
        error.to_string().contains("encrypted"),
        "unhelpful error: {error}"
    );
}

#[test]
fn rebuilds_a_cross_reference_table_with_short_entries() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("short-xref.pdf");
    let output = temporary.path().join("out");
    make_pdf(&input);
    // Some producers end each cross-reference entry after 19 bytes instead of
    // the 20 the specification requires; mainstream viewers still open those.
    let bytes = fs::read(&input).unwrap();
    let damaged = String::from_utf8_lossy(&bytes)
        .replace(" n \r\n", " n\n")
        .replace(" f \r\n", " f\n")
        .replace(" n \n", " n\n")
        .replace(" f \n", " f\n");
    fs::write(&input, damaged.as_bytes()).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("cross-reference")),
        "recovery was not reported: {:?}",
        report.warnings
    );
    assert!(
        fs::read_to_string(output.join("page-0001.svg"))
            .unwrap()
            .contains("Hello PDF")
    );
}

#[test]
fn advances_standard_font_text_without_a_widths_array() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("standard-font.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    // Helvetica without /Widths: every reader is expected to know its metrics.
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![20.into(), 100.into()]),
            Operation::new("Tj", vec![Object::string_literal("iiii")]),
            Operation::new("ET", vec![]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 12.into()]),
            Operation::new("Td", vec![20.into(), 60.into()]),
            Operation::new("Tj", vec![Object::string_literal("WWWW")]),
            Operation::new("ET", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A narrow letter must not advance as far as a wide one.
    let xs = |needle: &str| -> Vec<f64> {
        let start = svg.find(needle).expect("run not rendered");
        let tail = &svg[start..];
        let end = tail.find("</text>").unwrap_or(tail.len());
        tail[..end]
            .match_indices("x=\"")
            .filter_map(|(index, _)| {
                let rest = &tail[index + 3..];
                let stop = rest.find('"')?;
                rest[..stop].parse::<f64>().ok()
            })
            .collect()
    };
    let narrow = xs("iiii");
    let wide = xs("WWWW");
    assert!(
        narrow.len() > 1 && wide.len() > 1,
        "expected per-glyph positions, got {narrow:?} and {wide:?}"
    );
    let narrow_step = narrow[1] - narrow[0];
    let wide_step = wide[1] - wide[0];
    assert!(
        wide_step > narrow_step * 2.0,
        "standard font metrics were not applied: 'i' advanced {narrow_step}, 'W' advanced {wide_step}"
    );
}

fn make_zip(path: &Path, entries: &[(&str, &str)]) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

fn make_zip_bytes(path: &Path, entries: Vec<(&str, Vec<u8>)>) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        zip.start_file(name, options).unwrap();
        zip.write_all(&content).unwrap();
    }
    zip.finish().unwrap();
}

fn make_master_layout_pptx(path: &Path) {
    make_zip(
        path,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst><p:sldSz cx="9144000" cy="6858000"/></p:presentation>"#,
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                r#"<p:sldMaster xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Master decoration"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="127000"/><a:ext cx="635000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="112233"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Master title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="914400" y="457200"/><a:ext cx="4572000" cy="914400"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>MASTER SAMPLE</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sldMaster>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                r#"<p:sldLayout xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Layout decoration"/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x="127000" y="889000"/><a:ext cx="635000" cy="635000"/></a:xfrm><a:prstGeom prst="rect"/><a:solidFill><a:srgbClr val="445566"/></a:solidFill></p:spPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="2" name="Layout title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr><a:xfrm><a:off x="1828800" y="457200"/><a:ext cx="4572000" cy="914400"/></a:xfrm></p:spPr><p:txBody><a:p><a:r><a:t>LAYOUT SAMPLE</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slides/slide1.xml",
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Slide title"/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:p><a:r><a:rPr sz="2400"/><a:t>Inherited title</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );
}

fn make_pdf(path: &Path) {
    make_pdf_with_text(path, "Hello PDF");
}

fn make_pdf_with_text(path: &Path, text: &str) {
    let mut document = Document::with_version("1.7");
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => Object::Reference(font_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 24.into()]),
            Operation::new(
                "Tm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    720.into(),
                ],
            ),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    document.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "Contents" => Object::Reference(content_id),
            "Resources" => Object::Reference(resources_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    document.trailer.set("Root", Object::Reference(catalog_id));
    document.compress();
    document.save(path).unwrap();
}

fn make_pdf_compatibility_section(path: &Path) {
    let mut document = Document::with_version("1.7");
    let resources_id = document.add_object(dictionary! {});
    let content = Content {
        operations: vec![
            Operation::new("BX", vec![]),
            Operation::new("FutureOperator", vec![1.into()]),
            Operation::new("EX", vec![]),
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![10.into(), 10.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_type3_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let glyph = Content {
        operations: vec![
            Operation::new(
                "d1",
                vec![
                    600.into(),
                    0.into(),
                    0.into(),
                    0.into(),
                    600.into(),
                    700.into(),
                ],
            ),
            Operation::new("m", vec![0.into(), 0.into()]),
            Operation::new("l", vec![300.into(), 700.into()]),
            Operation::new("l", vec![600.into(), 0.into()]),
            Operation::new("h", vec![]),
            Operation::new("f", vec![]),
        ],
    };
    let glyph_id = document.add_object(Stream::new(dictionary! {}, glyph.encode().unwrap()));
    let font_id = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type3",
        "Name" => "F3",
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 700.into()],
        "FontMatrix" => vec![Object::Real(0.001), 0.into(), 0.into(), Object::Real(0.001), 0.into(), 0.into()],
        "CharProcs" => dictionary! { "A" => Object::Reference(glyph_id) },
        "Encoding" => dictionary! { "Type" => "Encoding", "Differences" => vec![65.into(), Object::Name(b"A".to_vec())] },
        "FirstChar" => 65,
        "LastChar" => 65,
        "Widths" => vec![600.into()],
        "Resources" => dictionary! {},
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! { "F3" => Object::Reference(font_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F3".to_vec()), 100.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 20.into()],
            ),
            Operation::new("Tj", vec![Object::string_literal("A")]),
            Operation::new("ET", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_packed_images_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let grayscale_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 3,
            "Height" => 2,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 4,
            "Decode" => vec![0.into(), 1.into()],
        },
        vec![0x08, 0xF0, 0xF8, 0x00],
    ));
    let stencil_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 8,
            "Height" => 1,
            "ImageMask" => true,
            "BitsPerComponent" => 1,
            "Decode" => vec![0.into(), 1.into()],
        },
        vec![0b1010_1010],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! {
            "Gray4" => Object::Reference(grayscale_id),
            "Stencil" => Object::Reference(stencil_id),
        },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    60.into(),
                    0.into(),
                    0.into(),
                    40.into(),
                    20.into(),
                    20.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Gray4".to_vec())]),
            Operation::new("Q", vec![]),
            Operation::new("q", vec![]),
            Operation::new("rg", vec![0.into(), 1.into(), 0.into()]),
            Operation::new(
                "cm",
                vec![
                    80.into(),
                    0.into(),
                    0.into(),
                    20.into(),
                    100.into(),
                    20.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Stencil".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_jpeg_soft_mask_pdf(path: &Path) {
    let mut mask_jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut mask_jpeg, 100)
        .encode(&[0, 255], 2, 1, jpeg_encoder::ColorType::Luma)
        .unwrap();
    let mut document = Document::with_version("1.7");
    let mask_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 1,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        },
        mask_jpeg,
    ));
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 1,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "SMask" => Object::Reference(mask_id),
        },
        vec![255, 0, 0, 0, 0, 255],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Masked" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    100.into(),
                    0.into(),
                    0.into(),
                    50.into(),
                    20.into(),
                    20.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Masked".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_shading_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let shading_id = document.add_object(dictionary! {
        "ShadingType" => 2,
        "ColorSpace" => "DeviceRGB",
        "Coords" => vec![0.into(), 0.into(), 200.into(), 0.into()],
        "Domain" => vec![0.into(), 1.into()],
        "Function" => dictionary! {
            "FunctionType" => 2,
            "Domain" => vec![0.into(), 1.into()],
            "C0" => vec![1.into(), 0.into(), 0.into()],
            "C1" => vec![0.into(), 0.into(), 1.into()],
            "N" => 1,
        },
        "Extend" => vec![true.into(), true.into()],
    });
    let resources_id = document.add_object(dictionary! {
        "Shading" => dictionary! { "Sh1" => Object::Reference(shading_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    1.into(),
                    0.into(),
                    0.into(),
                    1.into(),
                    72.into(),
                    500.into(),
                ],
            ),
            Operation::new("sh", vec![Object::Name(b"Sh1".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_function_shading_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let function_id = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        b"{ pop dup 1 exch sub 0.25 }".to_vec(),
    ));
    let shading_id = document.add_object(dictionary! {
        "ShadingType" => 1,
        "ColorSpace" => "DeviceRGB",
        "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
        "Matrix" => vec![180.into(), 0.into(), 0.into(), 80.into(), 10.into(), 10.into()],
        "Function" => Object::Reference(function_id),
    });
    let resources_id = document.add_object(dictionary! {
        "Shading" => dictionary! { "Sh1" => Object::Reference(shading_id) },
    });
    let content = Content {
        operations: vec![Operation::new("sh", vec![Object::Name(b"Sh1".to_vec())])],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_soft_mask_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let mask_content = Content {
        operations: vec![
            Operation::new("g", vec![0.5.into()]),
            Operation::new("re", vec![72.into(), 600.into(), 200.into(), 100.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let mask_form_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            "Resources" => dictionary! {},
            "Group" => dictionary! {
                "S" => "Transparency",
                "CS" => "DeviceGray",
                "I" => true,
            },
        },
        mask_content.encode().unwrap(),
    ));
    let graphics_state_id = document.add_object(dictionary! {
        "Type" => "ExtGState",
        "SMask" => dictionary! {
            "S" => "Luminosity",
            "G" => Object::Reference(mask_form_id),
        },
    });
    let resources_id = document.add_object(dictionary! {
        "ExtGState" => dictionary! { "GS1" => Object::Reference(graphics_state_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![72.into(), 600.into(), 200.into(), 100.into()]),
            Operation::new("f", vec![]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_soft_mask_transfer_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let mask_content = Content {
        operations: vec![
            Operation::new("g", vec![Object::Real(0.5)]),
            Operation::new("re", vec![20.into(), 20.into(), 100.into(), 100.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let mask_form_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 144.into(), 144.into()],
            "Resources" => dictionary! {},
            "Group" => dictionary! { "S" => "Transparency", "CS" => "DeviceGray", "I" => true },
        },
        mask_content.encode().unwrap(),
    ));
    let graphics_state_id = document.add_object(dictionary! {
        "Type" => "ExtGState",
        "SMask" => dictionary! {
            "S" => "Luminosity",
            "G" => Object::Reference(mask_form_id),
            "TR" => dictionary! {
                "FunctionType" => 2,
                "Domain" => vec![0.into(), 1.into()],
                "C0" => vec![0.into()],
                "C1" => vec![1.into()],
                "N" => 2,
                "Range" => vec![0.into(), 1.into()],
            },
        },
    });
    let resources_id = document.add_object(dictionary! {
        "ExtGState" => dictionary! { "GS1" => Object::Reference(graphics_state_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![20.into(), 20.into(), 100.into(), 100.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_nonisolated_group_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let form_content = Content {
        operations: vec![
            Operation::new("rg", vec![0.into(), 0.into(), 1.into()]),
            Operation::new("re", vec![20.into(), 20.into(), 80.into(), 80.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let form_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), 144.into(), 144.into()],
            "Resources" => dictionary! {},
            "Group" => dictionary! {
                "Type" => "Group",
                "S" => "Transparency",
                "CS" => "DeviceRGB",
                "I" => false,
                "K" => false,
            },
        },
        form_content.encode().unwrap(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Fm1" => Object::Reference(form_id) },
    });
    let content = Content {
        operations: vec![Operation::new("Do", vec![Object::Name(b"Fm1".to_vec())])],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_special_color_space_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let separation_id = document.add_object(Object::Array(vec![
        Object::Name(b"Separation".to_vec()),
        Object::Name(b"LogoGreen".to_vec()),
        Object::Name(b"DeviceRGB".to_vec()),
        Object::Dictionary(dictionary! {
            "FunctionType" => 2,
            "Domain" => vec![0.into(), 1.into()],
            "C0" => vec![1.into(), 1.into(), 1.into()],
            "C1" => vec![Object::Real(0.2), Object::Real(0.8), Object::Real(0.3)],
            "N" => 1,
        }),
    ]));
    let device_n_function_id = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        b"{ 0.5 }".to_vec(),
    ));
    let device_n_id = document.add_object(Object::Array(vec![
        Object::Name(b"DeviceN".to_vec()),
        Object::Array(vec![
            Object::Name(b"Spot1".to_vec()),
            Object::Name(b"Spot2".to_vec()),
        ]),
        Object::Name(b"DeviceRGB".to_vec()),
        Object::Reference(device_n_function_id),
    ]));
    let lab_id = document.add_object(Object::Array(vec![
        Object::Name(b"Lab".to_vec()),
        Object::Dictionary(dictionary! {
            "WhitePoint" => vec![Object::Real(0.95047), 1.into(), Object::Real(1.08883)],
            "Range" => vec![(-128).into(), 127.into(), (-128).into(), 127.into()],
        }),
    ]));
    let profile_id = document.add_object(Stream::new(dictionary! { "N" => 3 }, Vec::new()));
    let icc_id = document.add_object(Object::Array(vec![
        Object::Name(b"ICCBased".to_vec()),
        Object::Reference(profile_id),
    ]));
    let resources_id = document.add_object(dictionary! {
        "ColorSpace" => dictionary! {
            "CSsep" => Object::Reference(separation_id),
            "CSdevn" => Object::Reference(device_n_id),
            "CSlab" => Object::Reference(lab_id),
            "CSicc" => Object::Reference(icc_id),
        },
    });
    let content = Content {
        operations: vec![
            Operation::new("cs", vec![Object::Name(b"CSsep".to_vec())]),
            Operation::new("scn", vec![1.into()]),
            Operation::new("re", vec![0.into(), 0.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
            Operation::new("cs", vec![Object::Name(b"CSdevn".to_vec())]),
            Operation::new("scn", vec![Object::Real(0.2), Object::Real(0.8)]),
            Operation::new("re", vec![40.into(), 0.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
            Operation::new("cs", vec![Object::Name(b"CSlab".to_vec())]),
            Operation::new("scn", vec![50.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![80.into(), 0.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
            Operation::new("cs", vec![Object::Name(b"CSicc".to_vec())]),
            Operation::new("scn", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![120.into(), 0.into(), 40.into(), 30.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_shading_pattern_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let shading_id = document.add_object(dictionary! {
        "ShadingType" => 2,
        "ColorSpace" => "DeviceRGB",
        "Coords" => vec![50.into(), 0.into(), 150.into(), 0.into()],
        "Domain" => vec![0.into(), 1.into()],
        "Function" => dictionary! {
            "FunctionType" => 2,
            "Domain" => vec![0.into(), 1.into()],
            "C0" => vec![1.into(), 0.into(), 0.into()],
            "C1" => vec![0.into(), 0.into(), 1.into()],
            "N" => 1,
        },
        "Extend" => vec![false.into(), false.into()],
        "Background" => vec![0.into(), 1.into(), 0.into()],
        "BBox" => vec![25.into(), 20.into(), 175.into(), 80.into()],
    });
    let pattern_id = document.add_object(dictionary! {
        "Type" => "Pattern",
        "PatternType" => 2,
        "Shading" => Object::Reference(shading_id),
        "Matrix" => vec![1.into(), Object::Real(0.1), Object::Real(0.2), Object::Real(0.8), 0.into(), 10.into()],
    });
    let resources_id = document.add_object(dictionary! {
        "Pattern" => dictionary! { "P1" => Object::Reference(pattern_id) },
        "ExtGState" => dictionary! { "GS1" => dictionary! { "ca" => Object::Real(0.75) } },
    });
    let content = Content {
        operations: vec![
            Operation::new("gs", vec![Object::Name(b"GS1".to_vec())]),
            Operation::new("cs", vec![Object::Name(b"Pattern".to_vec())]),
            Operation::new("scn", vec![Object::Name(b"P1".to_vec())]),
            Operation::new("re", vec![10.into(), 10.into(), 180.into(), 80.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn make_tiling_pattern_pdf(path: &Path) {
    let mut document = Document::with_version("1.7");
    let colored_content = Content {
        operations: vec![
            Operation::new("rg", vec![1.into(), 0.into(), 0.into()]),
            Operation::new("re", vec![0.into(), 0.into(), 5.into(), 5.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let colored_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "Pattern",
            "PatternType" => 1,
            "PaintType" => 1,
            "TilingType" => 1,
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "XStep" => 10,
            "YStep" => 10,
            "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 0.into(), 0.into()],
            "Resources" => dictionary! {},
        },
        colored_content.encode().unwrap(),
    ));
    let uncolored_content = Content {
        operations: vec![
            Operation::new("re", vec![0.into(), 0.into(), 5.into(), 10.into()]),
            Operation::new("f", vec![]),
        ],
    };
    let uncolored_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "Pattern",
            "PatternType" => 1,
            "PaintType" => 2,
            "TilingType" => 1,
            "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()],
            "XStep" => 10,
            "YStep" => 10,
            "Resources" => dictionary! {},
        },
        uncolored_content.encode().unwrap(),
    ));
    let resources_id = document.add_object(dictionary! {
        "Pattern" => dictionary! {
            "P1" => Object::Reference(colored_id),
            "P2" => Object::Reference(uncolored_id),
        },
        "ColorSpace" => dictionary! {
            "PCS" => Object::Array(vec![
                Object::Name(b"Pattern".to_vec()),
                Object::Name(b"DeviceRGB".to_vec()),
            ]),
        },
    });
    let content = Content {
        operations: vec![
            Operation::new("cs", vec![Object::Name(b"Pattern".to_vec())]),
            Operation::new("scn", vec![Object::Name(b"P1".to_vec())]),
            Operation::new("re", vec![10.into(), 10.into(), 80.into(), 80.into()]),
            Operation::new("f", vec![]),
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    1.into(),
                    Object::Real(0.2),
                    0.into(),
                    1.into(),
                    280.into(),
                    0.into(),
                ],
            ),
            Operation::new("re", vec![0.into(), 10.into(), 60.into(), 60.into()]),
            Operation::new("f", vec![]),
            Operation::new("Q", vec![]),
            Operation::new("cs", vec![Object::Name(b"PCS".to_vec())]),
            Operation::new(
                "scn",
                vec![0.into(), 0.into(), 1.into(), Object::Name(b"P2".to_vec())],
            ),
            Operation::new("re", vec![100.into(), 10.into(), 80.into(), 80.into()]),
            Operation::new("f", vec![]),
            Operation::new(
                "scn",
                vec![0.into(), 1.into(), 0.into(), Object::Name(b"P2".to_vec())],
            ),
            Operation::new("re", vec![190.into(), 10.into(), 80.into(), 80.into()]),
            Operation::new("f", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, path, resources_id, content);
}

fn save_single_page_pdf(
    document: &mut Document,
    path: &Path,
    resources_id: (u32, u16),
    content: Content,
) {
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    document.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "Contents" => Object::Reference(content_id),
            "Resources" => Object::Reference(resources_id),
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
    });
    document.trailer.set("Root", Object::Reference(catalog_id));
    document.compress();
    document.save(path).unwrap();
}
