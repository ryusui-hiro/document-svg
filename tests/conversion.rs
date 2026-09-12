use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use base64::Engine;
use document_svg::{ConvertOptions, ReverseOptions, SourceFormat, convert_path};
use lopdf::content::{Content, Operation};
use lopdf::{Document, Object, Stream, dictionary};
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

#[path = "../src/pdf/font_test_data.rs"]
mod font_test_data;

#[test]
fn outlines_an_embedded_symbol_font_instead_of_trusting_a_system_symbol_font() {
    // A word processor's own subsetted font named "Symbol" is common for
    // dingbat-style bullets, and its ToUnicode map commonly points at
    // Private Use Area code points that mean nothing outside that specific
    // embedded font. Treating the family name as a sign that any system's
    // real Symbol font would render it left such text as a missing-glyph
    // box wherever the literal PUA text was not backed by that exact font.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("embedded-symbol-font.pdf");
    let mut document = Document::with_version("1.7");
    let bytes = font_test_data::font(None, false, false);
    let font_file = document.add_object(Stream::new(
        dictionary! { "Length1" => bytes.len() as i64 },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "Symbol", "Flags" => 4,
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 900.into()],
        "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80,
        "FontFile2" => font_file,
    });
    let to_unicode = document.add_object(Stream::new(
        dictionary! {},
        b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n1 beginbfchar\n<41> <F0B7>\nendbfchar\nendcmap\nend\nend".to_vec(),
    ));
    let font = document.add_object(dictionary! {
        // No /Encoding: this is a symbolic simple font per the PDF spec's
        // own default, matching a real embedded "Symbol" subset.
        "Type" => "Font", "Subtype" => "TrueType", "BaseFont" => "Symbol",
        "FirstChar" => 32, "LastChar" => 66,
        "Widths" => (32..=66).map(|code| Object::Integer(if code == 32 { 250 } else { 500 })).collect::<Vec<_>>(),
        "FontDescriptor" => descriptor, "ToUnicode" => to_unicode,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 20 80 Td (A) Tj ET".to_vec(),
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
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"text-outline\""), "{svg}");
    assert!(svg.contains("L 0.5 0"), "{svg}");
}

fn build_type0_pdf(input: &Path, encoding: &str) {
    let mut document = Document::with_version("1.7");
    let bytes = font_test_data::font(None, false, false);
    let font_file = document.add_object(Stream::new(
        dictionary! { "Length1" => bytes.len() as i64 },
        bytes,
    ));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor", "FontName" => "TestCID", "Flags" => 4,
        "FontBBox" => vec![0.into(), 0.into(), 600.into(), 900.into()],
        "Ascent" => 800, "Descent" => -200, "CapHeight" => 700, "StemV" => 80,
        "FontFile2" => font_file,
    });
    let descendant = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "CIDFontType2", "BaseFont" => "TestCID",
        "CIDSystemInfo" => dictionary! { "Registry" => Object::string_literal("Adobe"), "Ordering" => Object::string_literal("Identity"), "Supplement" => 0 },
        "FontDescriptor" => descriptor, "DW" => 1000,
    });
    let font = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "TestCID",
        "Encoding" => Object::Name(encoding.as_bytes().to_vec()),
        "DescendantFonts" => vec![descendant.into()],
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 20 Tf 20 80 Td <0041> Tj ET".to_vec(),
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
    document.save(input).unwrap();
}

#[test]
fn warns_about_a_type0_font_that_uses_a_non_identity_encoding() {
    // Character code only equals CID under Identity-H/V. A predefined
    // CMap such as UniJIS-UCS2-H (common for non-embedded standard Asian
    // fonts) maps code to CID through a table this converter does not
    // resolve, silently giving every /W width lookup and glyph selection
    // the wrong key -- which showed up as visibly wrong letter spacing in
    // Latin runs mixed into CJK text. Say so in a warning instead of
    // shipping mispositioned text with no diagnostic.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("non-identity.pdf");
    build_type0_pdf(&input, "UniJIS-UCS2-H");
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let warnings = &report.pages[0].warnings;
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("non-Identity encoding UniJIS-UCS2-H")),
        "{warnings:?}"
    );
}

#[test]
fn does_not_warn_about_a_type0_font_that_uses_identity_h_encoding() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("identity.pdf");
    build_type0_pdf(&input, "Identity-H");
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let warnings = &report.pages[0].warnings;
    assert!(
        !warnings
            .iter()
            .any(|warning| warning.contains("non-Identity")),
        "{warnings:?}"
    );
}

fn add_appearance_annotation(
    document: &mut Document,
    subtype: &str,
    rect: [i64; 4],
    fill_rgb: [f64; 3],
    flags: Option<i64>,
) -> Object {
    let appearance_content = format!(
        "{} {} {} rg 0 0 50 20 re f",
        fill_rgb[0], fill_rgb[1], fill_rgb[2]
    );
    let appearance = document.add_object(Stream::new(
        dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 50.into(), 20.into()] },
        appearance_content.into_bytes(),
    ));
    let mut annotation = dictionary! {
        "Type" => "Annot", "Subtype" => subtype,
        "Rect" => rect.into_iter().map(Object::Integer).collect::<Vec<_>>(),
        "AP" => dictionary! { "N" => appearance },
    };
    if let Some(flags) = flags {
        annotation.set("F", flags);
    }
    document.add_object(annotation).into()
}

#[test]
fn renders_annotation_appearance_streams_but_skips_link_and_hidden() {
    // A filled-in form field's value, and a sticky note's icon, live only
    // in an annotation's /AP appearance stream -- not in the page content
    // stream -- so a converter that reads only page content silently drops
    // them. Link annotations are conventionally just an invisible active
    // area (never rendered even when they happen to carry an /AP), and
    // anything flagged Hidden must stay invisible either way.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("annotations.pdf");
    let mut document = Document::with_version("1.7");
    let visible_widget = add_appearance_annotation(
        &mut document,
        "Widget",
        [100, 100, 150, 120],
        [0.0, 0.0, 1.0],
        None,
    );
    let visible_text = add_appearance_annotation(
        &mut document,
        "Text",
        [100, 200, 150, 220],
        [0.0, 1.0, 0.0],
        None,
    );
    let hidden_widget = add_appearance_annotation(
        &mut document,
        "Widget",
        [100, 300, 150, 320],
        [1.0, 0.0, 0.0],
        Some(2),
    );
    let link = add_appearance_annotation(
        &mut document,
        "Link",
        [100, 400, 150, 420],
        [1.0, 1.0, 0.0],
        None,
    );
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 400.into(), 500.into()],
        "Resources" => dictionary! {},
        "Contents" => content,
        "Annots" => vec![visible_widget, visible_text, hidden_widget, link],
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
    let output = temporary.path().join("out");
    convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#0000FF"), "{svg}");
    assert!(svg.contains("#00FF00"), "{svg}");
    assert!(!svg.contains("#FF0000"), "{svg}");
    assert!(!svg.contains("#FFFF00"), "{svg}");
}

#[test]
fn hides_content_in_an_optional_content_group_that_is_off_by_default() {
    // A PDF layer (Optional Content Group) that the document's own default
    // configuration turns off must not appear, the same way no viewer would
    // show it -- CAD- and GIS-exported PDFs commonly ship several such
    // layers, on by default or not.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ocg.pdf");
    let mut document = Document::with_version("1.7");
    let visible_ocg = document
        .add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Visible") });
    let hidden_ocg = document
        .add_object(dictionary! { "Type" => "OCG", "Name" => Object::string_literal("Hidden") });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"q /OC /VisG BDC 0 1 0 rg 20 20 100 100 re f EMC Q\nq /OC /HidG BDC 1 0 0 rg 150 20 100 100 re f EMC Q\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 200.into()],
        "Resources" => dictionary! {
            "Properties" => dictionary! { "VisG" => visible_ocg, "HidG" => hidden_ocg },
        },
        "Contents" => content,
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages,
        "OCProperties" => dictionary! {
            "OCGs" => vec![Object::Reference(visible_ocg), Object::Reference(hidden_ocg)],
            "D" => dictionary! { "OFF" => vec![Object::Reference(hidden_ocg)] },
        },
    });
    document.trailer.set("Root", catalog);
    document.save(&input).unwrap();
    let output = temporary.path().join("out");
    convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#00FF00"), "{svg}");
    assert!(!svg.contains("#FF0000"), "{svg}");
}

#[test]
fn fills_a_path_with_a_function_based_shading_pattern() {
    // A shading pattern (PatternType 2) has no SVG gradient equivalent once
    // its ShadingType is 1 (function-based) rather than 2/3 (axial/radial):
    // the fill silently disappeared. Tessellate it into an SVG <pattern>
    // instead, the same way the sh operator already tessellates one.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("function-pattern.pdf");
    let mut document = Document::with_version("1.7");
    let function = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 200.into(), 0.into(), 200.into()],
            "Range" => vec![0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        b"{ 200 div exch 200 div exch 0.5 }".to_vec(),
    ));
    let shading = dictionary! {
        "ShadingType" => 1, "ColorSpace" => "DeviceRGB",
        "Domain" => vec![0.into(), 200.into(), 0.into(), 200.into()],
        "Function" => function,
    };
    let pattern = document.add_object(dictionary! {
        "Type" => "Pattern", "PatternType" => 2, "Shading" => shading,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"/Pattern cs /P0 scn 20 20 160 160 re f\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Resources" => dictionary! { "Pattern" => dictionary! { "P0" => pattern } },
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
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(report.pages[0].warnings.is_empty(), "{:?}", report.pages[0].warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<pattern"), "{svg}");
    assert!(svg.contains("data-content-kind=\"function-shading-cell\""), "{svg}");
    assert!(svg.contains("fill=\"url(#"), "{svg}");
}

#[test]
fn fills_a_path_with_a_mesh_shading_pattern() {
    // A mesh shading pattern (PatternType 2, ShadingType 4-7) has no SVG
    // gradient equivalent either: the fill silently disappeared. Tessellate
    // it into an SVG <pattern> the same way the sh operator already does,
    // clipped to the mesh's own vertex bounds so the pattern's necessary
    // tiling does not repeat visibly beyond the shape the mesh actually
    // covers.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mesh-pattern.pdf");
    let mut document = Document::with_version("1.7");
    // ShadingType 4: free-form Gouraud-shaded triangle mesh. Each vertex is
    // flag(8 bits) + x,y(16 bits each) + r,g,b(8 bits each), decoded through
    // /Decode into a 0..200 coordinate range and 0..1 color range.
    fn vertex(flag: u8, x: u16, y: u16, r: u8, g: u8, b: u8) -> [u8; 8] {
        let xb = x.to_be_bytes();
        let yb = y.to_be_bytes();
        [flag, xb[0], xb[1], yb[0], yb[1], r, g, b]
    }
    let scale = |value: f64| -> u16 { (value / 200.0 * 65535.0) as u16 };
    let mut data = Vec::new();
    data.extend(vertex(0, scale(20.0), scale(20.0), 255, 0, 0));
    data.extend(vertex(0, scale(180.0), scale(20.0), 0, 255, 0));
    data.extend(vertex(0, scale(100.0), scale(180.0), 0, 0, 255));
    let shading = document.add_object(Stream::new(
        dictionary! {
            "ShadingType" => 4, "ColorSpace" => "DeviceRGB",
            "BitsPerCoordinate" => 16, "BitsPerComponent" => 8, "BitsPerFlag" => 8,
            "Decode" => vec![0.into(), 200.into(), 0.into(), 200.into(), 0.into(), 1.into(), 0.into(), 1.into(), 0.into(), 1.into()],
        },
        data,
    ));
    let pattern = document.add_object(dictionary! {
        "Type" => "Pattern", "PatternType" => 2, "Shading" => shading,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"/Pattern cs /P0 scn 10 10 180 180 re f\n".to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Resources" => dictionary! { "Pattern" => dictionary! { "P0" => pattern } },
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
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(report.pages[0].warnings.is_empty(), "{:?}", report.pages[0].warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<pattern"), "{svg}");
    assert!(svg.contains("data-content-kind=\"mesh-triangle\""), "{svg}");
    assert!(svg.contains("fill=\"url(#"), "{svg}");
}

#[test]
fn keeps_invisible_text_selectable_instead_of_dropping_it() {
    // Rendering mode 3 (invisible) is how a searchable scanned PDF hides its
    // OCR text layer behind the scanned image: nothing should paint, but
    // the text itself is the entire point of that layer, and dropping it
    // turned a searchable document into one that is not.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("invisible-text.pdf");
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
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new("Tr", vec![3.into()]),
            Operation::new(
                "Tm",
                vec![1.into(), 0.into(), 0.into(), 1.into(), 20.into(), 60.into()],
            ),
            Operation::new(
                "Tj",
                vec![Object::String(
                    b"Hidden OCR text".to_vec(),
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
    assert!(svg.contains("aria-label=\"Hidden OCR text\""), "{svg}");
    assert!(svg.contains("Hidden"), "{svg}");
    let hidden = svg.split("aria-label=\"Hidden OCR text\"").next().unwrap();
    let text_tag = hidden.rsplit("<text").next().unwrap();
    assert!(text_tag.contains("stroke=\"none\""), "{svg}");
    let tspan = svg.split_once("aria-label=\"Hidden OCR text\"").unwrap().1;
    assert!(tspan.contains("fill=\"none\""), "{svg}");
}

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
    // One data URI per image: `href` only, no duplicate `xlink:href`.
    assert_eq!(svg.matches("data:image/png;base64,").count(), 2);
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

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    // The guard runs on the declared dimensions, so the 30 GB buffer is never
    // allocated. The rest of the document still converts: one unusable image
    // is a warning, not a reason to discard every other page.
    assert_eq!(report.page_count, 1);
    assert!(
        report.warnings.iter().any(
            |warning| warning.contains("expands to 100000x100000 pixels")
                && warning.contains("was skipped")
        ),
        "{:?}",
        report.warnings
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
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid packed samples and was skipped")),
        "{:?}",
        report.warnings
    );
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
    assert_eq!(svg.matches("data:image/svg+xml;base64,").count(), 1);
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

/// A 600 DPI bilevel page scan is one byte per pixel once unpacked, so the
/// expansion budget must not charge it four. Before this was fixed an ordinary
/// scanned A4 page failed the whole document with an "RGBA limit" error.
#[test]
fn accepts_grayscale_images_that_would_exceed_the_rgba_budget() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("large-gray-scan.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    // 4910 x 6962 is a real 600 DPI A4 scan: 34.2 M pixels, which is 137 MB as
    // RGBA (over the 128 MiB default) but 34 MB as the grayscale it decodes to.
    let width = 4910usize;
    let height = 6962usize;
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width as i64,
            "Height" => height as i64,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0x80; width * height],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Scan" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new(
                "cm",
                vec![
                    612.into(),
                    0.into(),
                    0.into(),
                    792.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Scan".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert_eq!(report.pages[0].node_count, 1, "{:?}", report.warnings);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

/// JBIG2 is common in scanned manuals and has no decoder here. Naming the
/// filter beats relaying the decoder's "missing feature" text, and skipping
/// one image beats discarding the other 35 pages.
#[test]
fn skips_jbig2_images_by_name_without_failing_the_document() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("jbig2.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 16,
            "Height" => 16,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 1,
            "Filter" => "JBIG2Decode",
        },
        vec![0u8; 32],
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Scan" => Object::Reference(image_id) },
    });
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new("0 0 1 rg", vec![]),
            Operation::new("re", vec![0.into(), 0.into(), 10.into(), 10.into()]),
            Operation::new("f", vec![]),
            Operation::new("Do", vec![Object::Name(b"Scan".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning
                .contains("uses the unsupported JBIG2Decode filter and was skipped")),
        "{:?}",
        report.warnings
    );
}

/// A page that consumed real content operators but drew nothing looks exactly
/// like a genuinely empty page in the SVG. Say which one it was.
#[test]
fn warns_when_a_page_consumes_content_but_draws_nothing() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("silently-blank.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let resources_id = document.add_object(dictionary! {});
    // Referring to an XObject that no resource dictionary defines leaves the
    // page with operators to run and nothing to show for them.
    let content = Content {
        operations: vec![Operation::new(
            "Do",
            vec![Object::Name(b"Missing".to_vec())],
        )],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.pages[0].node_count, 0);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("drew nothing from")),
        "{:?}",
        report.warnings
    );
}

/// A page whose content stream is only whitespace is legitimately empty and
/// must not be reported as a conversion problem.
#[test]
fn does_not_warn_about_a_genuinely_empty_page() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("empty-page.pdf");
    let output = temporary.path().join("out");
    let mut document = Document::with_version("1.7");
    let resources_id = document.add_object(dictionary! {});
    save_single_page_pdf(
        &mut document,
        &input,
        resources_id,
        Content { operations: vec![] },
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.pages[0].node_count, 0);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("drew nothing from")),
        "{:?}",
        report.warnings
    );
}

/// Slide-less decks are a real export failure mode. "input contains no
/// renderable pages" does not tell the user which part of their file is empty.
#[test]
fn names_the_empty_slide_list_in_a_slideless_pptx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("no-slides.pptx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "ppt/presentation.xml",
                r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst/><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#,
            ),
            ("ppt/_rels/presentation.xml.rels", r#"<Relationships/>"#),
        ],
    );

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(
        error.to_string().contains("PPTX declares no slides"),
        "{error}"
    );
}

/// The XLSX counterpart of [`names_the_empty_slide_list_in_a_slideless_pptx`].
#[test]
fn names_the_empty_sheet_list_in_a_sheetless_xlsx() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("no-sheets.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="w" xmlns:r="r"><sheets/></workbook>"#,
            ),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships/>"#),
        ],
    );

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(
        error.to_string().contains("XLSX declares no worksheets"),
        "{error}"
    );
}

/// PowerPoint stores `<#>` as the cached text of a slide-number field and
/// substitutes the real number when it renders. Emitting the cached text puts a
/// literal `<#>` on every slide of the roughly one deck in three that numbers
/// its slides.
#[test]
fn resolves_pptx_slide_number_fields_to_the_page_number() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("slide-numbers.pptx");
    let output = temporary.path().join("out");
    let slide = r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Number"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="6000000" y="6000000"/><a:ext cx="800000" cy="300000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:fld id="{X}" type="slidenum"><a:rPr sz="1200"/><a:t>&#8249;#&#8250;</a:t></a:fld></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;
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
            ("ppt/slides/slide1.xml", slide),
            ("ppt/slides/slide2.xml", slide),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 2);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains(">1</tspan>"), "{first}");
    assert!(second.contains(">2</tspan>"), "{second}");
    assert!(
        !first.contains('\u{2039}') && !first.contains('\u{203a}'),
        "cached placeholder text leaked: {first}"
    );
}

/// Word keeps whatever name a picture arrived with, so `media/image1.png`
/// holding JPEG bytes is ordinary. Declaring the wrong type in the data URI
/// makes strict SVG renderers drop the image and the document silently loses
/// its figures.
#[test]
fn labels_docx_images_from_their_signature_not_their_file_name() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mislabelled-image.docx");
    let output = temporary.path().join("out");
    // A minimal but real JPEG: SOI, APP0/JFIF, EOI.
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
    jpeg.extend_from_slice(b"JFIF\0");
    jpeg.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    jpeg.extend_from_slice(&[0xff, 0xd9]);
    make_zip_bytes(
        &input,
        vec![
            (
                "word/document.xml",
                br#"<w:document xmlns:w="w" xmlns:r="r" xmlns:a="a" xmlns:wp="wp"><w:body><w:p><w:r><w:drawing><wp:inline><wp:extent cx="1270000" cy="1270000"/><a:graphic><a:graphicData><pic:pic xmlns:pic="pic"><pic:blipFill><a:blip r:embed="rId9"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p></w:body></w:document>"#.to_vec(),
            ),
            (
                "word/_rels/document.xml.rels",
                br#"<Relationships><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image1.png"/></Relationships>"#.to_vec(),
            ),
            ("word/media/image1.png", jpeg),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data:image/jpeg;base64,"), "{svg}");
    assert!(!svg.contains("data:image/png;base64,"), "{svg}");
}

/// `fldCharType="separate"` is optional. A `PAGE` field Word has never
/// calculated runs begin -> instrText -> end, and skipping it leaves the footer
/// without its page number.
#[test]
fn materializes_a_docx_page_field_that_has_no_separate_marker() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("page-field.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "word/document.xml",
                r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Body</w:t></w:r></w:p><w:p><w:r><w:br w:type="page"/></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p><w:sectPr><w:footerReference w:type="default" r:id="rId5" xmlns:r="r"/></w:sectPr></w:body></w:document>"#,
            ),
            (
                "word/_rels/document.xml.rels",
                r#"<Relationships><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer" Target="footer1.xml"/></Relationships>"#,
            ),
            (
                "word/footer1.xml",
                r#"<w:ftr xmlns:w="w"><w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/><w:instrText xml:space="preserve"> PAGE </w:instrText><w:fldChar w:fldCharType="end"/></w:r></w:p></w:ftr>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.page_count >= 2, "{}", report.page_count);
    let first = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let second = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(first.contains("Page 1"), "{first}");
    assert!(second.contains("Page 2"), "{second}");
}

/// quick-xml reports `&amp;` as `Event::GeneralRef`, not as text. A parser that
/// only handles `Event::Text` drops every ampersand, so "R&D" reached the SVG
/// as "RD". This held for PPTX shape text, PPTX table text, DOCX body text and
/// chart labels; 12.7% of the documents in the review corpus were affected.
#[test]
fn keeps_xml_entities_in_pptx_shape_and_table_text() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("entities.pptx");
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
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="T"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="5000000" cy="1000000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:rPr sz="2400"/><a:t>R&amp;D &#183; Q&amp;A</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The SVG re-escapes the ampersand, so the round trip is `&amp;`.
    assert!(svg.contains("R&amp;D \u{b7} Q&amp;A"), "{svg}");
}

/// The DOCX half of [`keeps_xml_entities_in_pptx_shape_and_table_text`].
#[test]
fn keeps_xml_entities_in_docx_text() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("entities.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t xml:space="preserve">Research &amp; Development &#8212; Q&amp;A</w:t></w:r></w:p></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("Research &amp; Development \u{2014} Q&amp;A"),
        "{svg}"
    );
}

/// `a14:hiddenFill` and `a14:hiddenLine` hold the colours Word would restore if
/// the shape's real `noFill` were removed. Reading them as the shape's own paint
/// filled every such text box with `hiddenLine`'s black, hiding its text.
#[test]
fn ignores_compatibility_paint_on_docx_text_boxes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("hidden-fill.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w" xmlns:a="a" xmlns:wps="wps" xmlns:wp="wp" xmlns:a14="a14"><w:body><w:p><w:r><w:drawing><wp:anchor><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="3630295" cy="361950"/><a:graphic><a:graphicData><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="3630295" cy="361950"/></a:xfrm><a:prstGeom prst="rect"/><a:noFill/><a:ln><a:noFill/></a:ln><a:extLst><a:ext uri="{909E8E84}"><a14:hiddenFill><a:solidFill><a:srgbClr val="FFFFFF"/></a:solidFill></a14:hiddenFill></a:ext><a:ext uri="{91240B29}"><a14:hiddenLine w="9525"><a:solidFill><a:srgbClr val="000000"/></a:solidFill></a14:hiddenLine></a:ext></a:extLst></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>Visible caption</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Visible caption"), "{svg}");
    let box_path = svg
        .lines()
        .find(|line| line.contains("docx-floating-text-box"))
        .unwrap_or_default();
    assert!(
        box_path.contains("fill=\"none\""),
        "compatibility paint became the shape fill: {box_path}"
    );
}

/// Word's default text box is `noAutofit`: text longer than the frame spills
/// out rather than vanishing. Cutting the layout off at the declared height
/// dropped every paragraph after the first in a short box.
#[test]
fn keeps_docx_text_box_paragraphs_that_overflow_the_frame() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("overflowing-box.docx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[(
            "word/document.xml",
            r#"<w:document xmlns:w="w" xmlns:a="a" xmlns:wps="wps" xmlns:wp="wp"><w:body><w:p><w:r><w:drawing><wp:anchor><wp:positionH relativeFrom="column"><wp:posOffset>0</wp:posOffset></wp:positionH><wp:positionV relativeFrom="paragraph"><wp:posOffset>0</wp:posOffset></wp:positionV><wp:extent cx="3630295" cy="180000"/><a:graphic><a:graphicData><wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="3630295" cy="180000"/></a:xfrm><a:prstGeom prst="rect"/><a:noFill/><a:noAutofit/></wps:spPr><wps:txbx><w:txbxContent><w:p><w:r><w:t>First line of the box</w:t></w:r></w:p><w:p><w:r><w:t>Second line of the box</w:t></w:r></w:p><w:p><w:r><w:t>Third line of the box</w:t></w:r></w:p></w:txbxContent></wps:txbx></wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p></w:body></w:document>"#,
        )],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    for line in [
        "First line of the box",
        "Second line of the box",
        "Third line of the box",
    ] {
        assert!(svg.contains(line), "missing {line:?} in {svg}");
    }
}

/// A Flate image written with a PNG predictor decoded to noise: `/DecodeParms`
/// given as an indirect reference was not read at all, and the Average row
/// filter reconstructs `(left + above) / 2`, not `left + above / 2`. Both are
/// silent — the page simply comes out as static.
#[test]
fn undoes_png_predictors_including_the_average_row_filter() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("predicted-image.pdf");
    let output = temporary.path().join("out");
    let width = 4usize;
    let height = 4usize;
    let bpp = 3usize;
    let stride = width * bpp;

    // A gradient whose Average rows differ under the two reconstructions.
    let mut expected = vec![0u8; stride * height];
    for y in 0..height {
        for x in 0..width {
            for c in 0..bpp {
                expected[y * stride + x * bpp + c] = ((x * 40 + y * 30 + c * 20) % 256) as u8;
            }
        }
    }
    // Filter each row: None, Sub, Up, then Average.
    let mut filtered = Vec::new();
    for y in 0..height {
        let row = &expected[y * stride..(y + 1) * stride];
        let previous: &[u8] = if y == 0 {
            &[0u8; 12]
        } else {
            &expected[(y - 1) * stride..y * stride]
        };
        let filter = y as u8; // 0 None, 1 Sub, 2 Up, 3 Average
        filtered.push(filter);
        for i in 0..stride {
            let left = if i >= bpp { row[i - bpp] } else { 0 };
            let above = previous[i];
            let predicted = match filter {
                1 => left,
                2 => above,
                3 => ((u16::from(left) + u16::from(above)) / 2) as u8,
                _ => 0,
            };
            filtered.push(row[i].wrapping_sub(predicted));
        }
    }

    let mut document = Document::with_version("1.7");
    let parameters = document.add_object(dictionary! {
        "Predictor" => 15,
        "Colors" => 3,
        "BitsPerComponent" => 8,
        "Columns" => width as i64,
    });
    let mut image = Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width as i64,
            "Height" => height as i64,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            // Referenced, not inline: the form that was being ignored.
            "DecodeParms" => Object::Reference(parameters),
        },
        filtered,
    );
    image.compress().unwrap();
    let image_id = document.add_object(image);
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "Predicted" => Object::Reference(image_id) },
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
                    100.into(),
                    0.into(),
                    0.into(),
                ],
            ),
            Operation::new("Do", vec![Object::Name(b"Predicted".to_vec())]),
            Operation::new("Q", vec![]),
        ],
    };
    save_single_page_pdf(&mut document, &input, resources_id, content);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.pages[0].node_count, 1, "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let encoded = svg
        .split("base64,")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("image data URI");
    let png = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let mut samples = vec![0u8; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut samples).unwrap();
    assert_eq!(&samples[..info.buffer_size()], &expected[..]);
}

/// `<p:bg>` accepts the same fill grammar as a shape, but the background was
/// tracked as a single colour, so a gradient collapsed to whichever stop the
/// parser read last. A title slide meant to fade from bright to dark came out
/// flat in the dark stop. 6% of the decks in the review corpus use one.
#[test]
fn renders_a_pptx_gradient_slide_background_as_a_gradient() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("gradient-background.pptx");
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
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:bg><p:bgPr><a:gradFill><a:gsLst><a:gs pos="0"><a:srgbClr val="FFC000"/></a:gs><a:gs pos="100000"><a:srgbClr val="C00000"/></a:gs></a:gsLst><a:lin ang="2700000"/></a:gradFill></p:bgPr></p:bg><p:spTree/></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<linearGradient"), "{svg}");
    // Both stops must survive, not just the last one read.
    assert!(svg.contains("#FFC000"), "{svg}");
    assert!(svg.contains("#C00000"), "{svg}");
}

/// `showMasterSp="0"` on a layout means the master's own shapes are not drawn.
/// Ignoring it doubles anything a template repeats on both the master and the
/// layout — a footer, a logo. 27 of the 259 decks in the review corpus set it.
#[test]
fn honours_show_master_sp_when_a_layout_hides_master_shapes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("hidden-master.pptx");
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
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Body"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="4000000" cy="900000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:t>Slide body</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml",
                r#"<p:sldLayout xmlns:p="p" xmlns:a="a" showMasterSp="0"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="LayoutFooter"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="5000000"/><a:ext cx="4000000" cy="400000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:t>Repeated footer</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sldLayout>"#,
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#,
            ),
            (
                "ppt/slideMasters/slideMaster1.xml",
                r#"<p:sldMaster xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="3" name="MasterFooter"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="5000000"/><a:ext cx="4000000" cy="400000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:t>Repeated footer</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sldMaster>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("Slide body"), "{svg}");
    assert_eq!(
        svg.matches("Repeated footer").count(),
        1,
        "the master copy should be suppressed: {svg}"
    );
}

/// `<a:tr h="...">` is a minimum row height: PowerPoint grows a row when its
/// cells wrap to more lines than fit. Treating it as final clipped the second
/// line off every wrapped cell, cutting the text mid-glyph.
#[test]
fn grows_pptx_table_rows_to_fit_wrapped_cell_text() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("wrapped-table.pptx");
    let output = temporary.path().join("out");
    // One narrow column, a short declared row height, and text that needs
    // several lines at that width.
    let slide = r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Grid"/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="400000"/></p:xfrm><a:graphic><a:graphicData><a:tbl><a:tblGrid><a:gridCol w="2000000"/></a:tblGrid><a:tr h="200000"><a:tc><a:txBody><a:p><a:r><a:rPr sz="1200"/><a:t>alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#;
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
            ("ppt/slides/slide1.xml", slide),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Every wrapped word survives, and the last line sits below the declared
    // 200000 EMU (about 15.7pt) row height rather than being clipped away.
    for word in ["alpha", "juliet", "kilo", "lima"] {
        assert!(svg.contains(word), "missing {word:?} in {svg}");
    }
    let last_baseline = svg
        .split("<text")
        .filter_map(|chunk| chunk.split("y=\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .filter_map(|value| value.parse::<f64>().ok())
        .fold(0.0_f64, f64::max);
    assert!(
        last_baseline > 15.7,
        "the row did not grow past its declared height: {last_baseline}"
    );
}

/// `Attribute::value` holds raw, still-escaped bytes. Taking them verbatim and
/// then escaping again on the way out turned an alt text of `R&D` into
/// `R&amp;D` on screen. 58 of the 364 Office files in the review corpus carry
/// an entity in a `descr`, `name` or `title` attribute.
#[test]
fn resolves_xml_entities_in_pptx_attribute_values() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("entity-attributes.pptx");
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
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Chart" descr="Revenue for R&amp;D and Q&amp;A"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="4000000" cy="1000000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:p><a:r><a:t>body</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // One level of escaping, so a reader sees "R&D", not "R&amp;D".
    assert!(
        svg.contains(r#"aria-label="Revenue for R&amp;D and Q&amp;A""#),
        "{svg}"
    );
}

/// `/Interpolate false` asks for no smoothing when an image is enlarged. CSS
/// `pixelated` also disables the area averaging every PDF viewer applies when
/// an image is reduced, which speckles a scan placed at a fraction of its pixel
/// size, so it belongs only on images actually drawn larger than their own grid.
#[test]
fn asks_for_pixelated_rendering_only_where_the_image_is_magnified() {
    fn convert_at(scale: i64, directory: &Path) -> String {
        let input = directory.join(format!("image-{scale}.pdf"));
        let output = directory.join(format!("out-{scale}"));
        let mut document = Document::with_version("1.7");
        // A 64x64 image, drawn either much smaller or much larger.
        let image_id = document.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => 64,
                "Height" => 64,
                "ColorSpace" => "DeviceGray",
                "BitsPerComponent" => 8,
            },
            vec![0x40; 64 * 64],
        ));
        let resources_id = document.add_object(dictionary! {
            "XObject" => dictionary! { "Pic" => Object::Reference(image_id) },
        });
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new(
                    "cm",
                    vec![
                        scale.into(),
                        0.into(),
                        0.into(),
                        scale.into(),
                        0.into(),
                        0.into(),
                    ],
                ),
                Operation::new("Do", vec![Object::Name(b"Pic".to_vec())]),
                Operation::new("Q", vec![]),
            ],
        };
        save_single_page_pdf(&mut document, &input, resources_id, content);
        convert_path(&input, &output, &ConvertOptions::default()).unwrap();
        fs::read_to_string(output.join("page-0001.svg")).unwrap()
    }

    let temporary = TempDir::new().unwrap();
    let reduced = convert_at(16, temporary.path());
    let enlarged = convert_at(400, temporary.path());

    assert!(
        reduced.contains("image-rendering=\"auto\""),
        "a 64px image drawn at 16pt is being reduced: {reduced}"
    );
    assert!(
        enlarged.contains("image-rendering=\"pixelated\""),
        "a 64px image drawn at 400pt is being enlarged: {enlarged}"
    );
}

/// `<c:barDir val="bar"/>` is a horizontal bar chart. It was never read, so a
/// bar chart came out as a column chart — the wrong shape for the data — and
/// the category names, which the parser already collects, were never drawn, so
/// the bars had nothing identifying them.
#[test]
fn draws_a_horizontal_bar_chart_with_its_category_names() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bar-chart.pptx");
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
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Chart"/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="5000000" cy="3000000"/></p:xfrm><a:graphic><a:graphicData><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="bar"/><c:ser><c:tx><c:v>Count</c:v></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Moulding</c:v></c:pt><c:pt idx="1"><c:v>Assembly</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>47</c:v></c:pt><c:pt idx="1"><c:v>12</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("Moulding"),
        "category names are missing: {svg}"
    );
    assert!(
        svg.contains("Assembly"),
        "category names are missing: {svg}"
    );
    // Horizontal bars share a left edge and differ in length; column bars would
    // instead share a bottom edge and differ in height.
    let bars: Vec<&str> = svg
        .split("data-content-kind=\"chart-bar\"")
        .take(3)
        .collect();
    assert!(bars.len() >= 3, "expected two bars: {svg}");
    let widths: Vec<f64> = svg
        .split("<path")
        .filter(|chunk| chunk.contains("chart-bar"))
        .filter_map(|chunk| chunk.split(" H ").nth(1))
        .filter_map(|rest| rest.split(' ').next())
        .filter_map(|value| value.parse::<f64>().ok())
        .collect();
    assert_eq!(widths.len(), 2, "expected two bar rectangles: {svg}");
    assert!(
        (widths[0] - widths[1]).abs() > 1.0,
        "horizontal bars must differ in length: {widths:?}"
    );
}

/// `<c:grouping val="stacked"/>` puts the series on top of one another, so the
/// category total is what the reader sees. Drawing them side by side instead
/// turns a column of 146 into two columns of 64 and 82 — a different claim
/// about the data.
#[test]
fn stacks_a_stacked_bar_chart_instead_of_clustering_it() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("stacked-chart.xlsx");
    let output = temporary.path().join("out");
    make_zip(
        &input,
        &[
            (
                "xl/workbook.xml",
                r#"<workbook xmlns="w" xmlns:r="r"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet xmlns="w" xmlns:r="r"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Grid</t></is></c></row></sheetData><drawing r:id="rId2"/></worksheet>"#,
            ),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/></Relationships>"#,
            ),
            (
                "xl/drawings/drawing1.xml",
                r#"<xdr:wsDr xmlns:xdr="xdr" xmlns:a="a" xmlns:r="r"><xdr:twoCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>1</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from><xdr:to><xdr:col>8</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>18</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to><xdr:graphicFrame><xdr:nvGraphicFramePr><xdr:cNvPr id="1" name="Chart"/></xdr:nvGraphicFramePr><a:graphic><a:graphicData><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId3"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor></xdr:wsDr>"#,
            ),
            (
                "xl/drawings/_rels/drawing1.xml.rels",
                r#"<Relationships><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "xl/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:barChart><c:barDir val="col"/><c:grouping val="stacked"/><c:ser><c:tx><c:v>Inside</c:v></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>G1</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>64</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser><c:ser><c:tx><c:v>Outside</c:v></c:tx><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>82</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:barChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert_eq!(report.page_count, 1);
    // Two bars sharing one category slot: the same left edge, stacked so that
    // together they fill the plot height.
    let lefts: Vec<f64> = svg
        .split("<path")
        .filter(|chunk| chunk.contains("chart-bar"))
        .filter_map(|chunk| chunk.split("M ").nth(1))
        .filter_map(|rest| rest.split(' ').next())
        .filter_map(|value| value.parse::<f64>().ok())
        .collect();
    assert_eq!(lefts.len(), 2, "expected two stacked segments: {svg}");
    assert!(
        (lefts[0] - lefts[1]).abs() < 0.01,
        "stacked segments must share a left edge, got {lefts:?}"
    );
}

/// A line chart shares the bar chart's category axis, and its points sit on the
/// plot edges rather than in slots, so its labels belong under the points.
#[test]
fn labels_line_chart_categories_under_their_points() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("line-chart.pptx");
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
                r#"<p:sld xmlns:p="p" xmlns:a="a" xmlns:r="r"><p:cSld><p:spTree><p:graphicFrame><p:nvGraphicFramePr><p:cNvPr id="1" name="Chart"/></p:nvGraphicFramePr><p:xfrm><a:off x="0" y="0"/><a:ext cx="5000000" cy="3000000"/></p:xfrm><a:graphic><a:graphicData><c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId2"/></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#,
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                r#"<Relationships><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/></Relationships>"#,
            ),
            (
                "ppt/charts/chart1.xml",
                r#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:plotArea><c:lineChart><c:ser><c:tx><c:v>Cost</c:v></c:tx><c:cat><c:strRef><c:strCache><c:pt idx="0"><c:v>Jan</c:v></c:pt><c:pt idx="1"><c:v>Feb</c:v></c:pt><c:pt idx="2"><c:v>Mar</c:v></c:pt></c:strCache></c:strRef></c:cat><c:val><c:numRef><c:numCache><c:pt idx="0"><c:v>3</c:v></c:pt><c:pt idx="1"><c:v>6</c:v></c:pt><c:pt idx="2"><c:v>9</c:v></c:pt></c:numCache></c:numRef></c:val></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    for month in ["Jan", "Feb", "Mar"] {
        assert!(svg.contains(month), "missing {month:?} in {svg}");
    }
    // The first and last labels sit on the plot edges, where the line starts
    // and ends, rather than inset by half a slot.
    let label_xs: Vec<f64> = svg
        .split("<text")
        .filter(|chunk| chunk.contains("chart-category-label"))
        .filter_map(|chunk| chunk.split("x=\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .filter_map(|value| value.parse::<f64>().ok())
        .collect();
    assert_eq!(label_xs.len(), 3, "{svg}");
    let middle = (label_xs[0] + label_xs[2]) / 2.0;
    assert!(
        (label_xs[1] - middle).abs() < 0.5,
        "the middle label should sit midway between the outer two: {label_xs:?}"
    );
}

/// `<a:buChar>` was only read from a master's text styles, so a paragraph that
/// names its own bullet — the usual way a deck writes a bulleted list — lost
/// it. `marL` and `indent` were not read from the paragraph either, so the
/// bullet sat hard against the shape inset instead of hanging beside its text.
#[test]
fn draws_a_pptx_paragraphs_own_bullet_at_its_hanging_indent() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bulleted.pptx");
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
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="1" name="Body"/></p:nvSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="5000000" cy="2000000"/></a:xfrm><a:prstGeom prst="rect"/></p:spPr><p:txBody><a:bodyPr lIns="0"/><a:p><a:pPr marL="228600" indent="-228600"><a:buChar char="&#8226;"/></a:pPr><a:r><a:rPr sz="1400"/><a:t>First item</a:t></a:r></a:p><a:p><a:pPr marL="228600" indent="-228600"><a:buNone/></a:pPr><a:r><a:rPr sz="1400"/><a:t>Unbulleted item</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Exactly one bullet: the second paragraph asked for none.
    assert_eq!(
        svg.matches(r#"data-content-kind="bullet""#).count(),
        1,
        "{svg}"
    );
    // marL is 228600 EMU (18pt) and indent cancels it, so the bullet sits at 0
    // and the text at 18pt.
    let bullet_x: f64 = svg
        .split("<text")
        .find(|chunk| chunk.contains(r#"data-content-kind="bullet""#))
        .and_then(|chunk| chunk.split("x=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse().ok())
        .expect("bullet x");
    let text_x: f64 = svg
        .split("<text")
        .find(|chunk| chunk.contains("First item"))
        .and_then(|chunk| chunk.split("x=\"").nth(1))
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse().ok())
        .expect("text x");
    assert!(
        bullet_x < 0.01,
        "bullet should hang at the margin: {bullet_x}"
    );
    assert!(
        (text_x - 18.0).abs() < 0.5,
        "text should start at marL: {text_x}"
    );
}

/// Curved connectors fell through to the bounding-box fallback, drawing a
/// stroked rectangle where a line should curve between two shapes.
#[test]
fn draws_curved_connectors_as_curves_not_boxes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("curved-connector.pptx");
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
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:cxnSp><p:nvCxnSpPr><p:cNvPr id="1" name="Curve"/></p:nvCxnSpPr><p:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="2000000" cy="1000000"/></a:xfrm><a:prstGeom prst="curvedConnector3"><a:avLst/></a:prstGeom><a:ln w="12700"><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill></a:ln></p:spPr></p:cxnSp></p:spTree></p:cSld></p:sld>"#,
            ),
        ],
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains(" C "),
        "the connector should be a cubic curve: {svg}"
    );
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("curvedConnector3")),
        "{:?}",
        report.warnings
    );
}

fn drawio_file(path: &Path, body: &str) {
    let mut file = File::create(path).unwrap();
    file.write_all(body.as_bytes()).unwrap();
}

/// draw.io stores a diagram as `encodeURIComponent` output, raw-deflated and
/// base64-encoded. This builds the same thing so the reader is exercised the
/// way the editor writes it.
fn packed_diagram(xml: &str) -> String {
    let mut encoded = String::new();
    for byte in xml.as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(byte) {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    let mut deflater =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    deflater.write_all(encoded.as_bytes()).unwrap();
    base64::engine::general_purpose::STANDARD.encode(deflater.finish().unwrap())
}

const DRAWIO_FLOW: &str = r##"<mxGraphModel dx="800" dy="600" background="#FAFAFA"><root>
<mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="start" value="&lt;b&gt;Start&lt;/b&gt;&lt;br&gt;here" style="ellipse;whiteSpace=wrap;html=1;fillColor=#D5E8D4;strokeColor=#82B366;" vertex="1" parent="1"><mxGeometry x="40" y="40" width="120" height="60" as="geometry"/></mxCell>
<mxCell id="dec" value="Is it OK?" style="rhombus;whiteSpace=wrap;html=1;" vertex="1" parent="1"><mxGeometry x="280" y="140" width="140" height="90" as="geometry"/></mxCell>
<mxCell id="edge" value="yes" style="edgeStyle=orthogonalEdgeStyle;rounded=0;html=1;" edge="1" parent="1" source="start" target="dec"><mxGeometry relative="1" as="geometry"/></mxCell>
</root></mxGraphModel>"##;

#[test]
fn converts_drawio_shapes_labels_and_edges() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("flow.drawio");
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    drawio_file(
        &input,
        &format!(
            r##"<mxfile host="test"><diagram id="p1" name="Flow">{DRAWIO_FLOW}</diagram></mxfile>"##
        ),
    );

    let report = convert_path(&input, &first, &ConvertOptions::default()).unwrap();

    assert_eq!(report.source_format, SourceFormat::Drawio);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(first.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<title>Flow</title>"), "{svg}");
    // Model pixels reach the page as points: the drawing spans 40..420 across
    // and 40..230 down, plus the 10 pixel crop margin on each side.
    assert!(svg.contains(r#"width="300pt" height="157.5pt""#), "{svg}");
    assert!(svg.contains("#D5E8D4"), "{svg}");
    assert!(
        svg.contains(">Start</tspan>") && svg.contains(">here</tspan>"),
        "{svg}"
    );
    assert!(svg.contains("font-weight=\"700\""), "{svg}");
    assert!(svg.contains(">Is it OK?</tspan>"), "{svg}");
    assert!(svg.contains(">yes</tspan>"), "{svg}");
    assert!(
        svg.contains("data-content-kind=\"drawio-edge-marker\""),
        "{svg}"
    );
    convert_path(&input, &second, &ConvertOptions::default()).unwrap();
    assert_eq!(
        svg,
        fs::read_to_string(second.join("page-0001.svg")).unwrap()
    );
}

#[test]
fn expands_compressed_drawio_diagrams_into_one_page_each() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("packed.drawio");
    let output = temporary.path().join("out");
    let second = r##"<mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="note" value="Second &amp; last" style="rounded=1;whiteSpace=wrap;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="200" height="60" as="geometry"/></mxCell>
</root></mxGraphModel>"##;
    drawio_file(
        &input,
        &format!(
            r##"<mxfile host="test"><diagram id="p1" name="Plain">{DRAWIO_FLOW}</diagram><diagram id="p2" name="Packed">{}</diagram></mxfile>"##,
            packed_diagram(second)
        ),
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 2);
    let svg = fs::read_to_string(output.join("page-0002.svg")).unwrap();
    assert!(svg.contains("<title>Packed</title>"), "{svg}");
    assert!(svg.contains(">Second &amp; last</tspan>"), "{svg}");
}

#[test]
fn routes_orthogonal_drawio_edges_square_on_to_the_shapes_they_join() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("route.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        &format!(r##"<mxfile><diagram name="Flow">{DRAWIO_FLOW}</diagram></mxfile>"##),
    );

    convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let path = svg
        .lines()
        .find(|line| line.contains(r#"id="drawio-edge""#))
        .unwrap_or_default();
    // The route leaves the ellipse's right side and enters the rhombus's left
    // side, turning halfway between the two shapes rather than at either one.
    assert!(
        path.contains(r#"d="M 160 70 L 220 70 L 220 185 L "#),
        "{path}"
    );
}

#[test]
fn keeps_a_drawio_shape_it_cannot_draw_as_a_labelled_placeholder() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("library.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Icons"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="fn" value="Lambda" style="sketch=0;shape=mxgraph.aws4.lambda_function;fillColor=#F58534;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="78" height="78" as="geometry"/></mxCell>
<mxCell id="fn2" value="Also Lambda" style="shape=mxgraph.aws4.lambda_function;" vertex="1" parent="1"><mxGeometry x="140" y="20" width="78" height="78" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("mxgraph.aws4.lambda_function")
            && report.warnings[0].contains("shape library"),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains(">Lambda</tspan>"), "{svg}");
    assert!(svg.contains("#F58534"), "{svg}");
}

#[test]
fn rejects_xml_that_is_not_a_drawio_document() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("notes.xml");
    let output = temporary.path().join("out");
    drawio_file(&input, "<notes><note>Not a diagram</note></notes>");

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(error.to_string().contains("mxGraphModel"), "{}", error);
}

#[test]
fn embeds_drawio_pictures_and_refuses_to_fetch_remote_ones() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("pictures.drawio");
    let output = temporary.path().join("out");
    // A 2x2 PNG, stored the way draw.io writes one: no ";base64" marker.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAFElEQVR4nGP8z8Dwn4GBgYmBgYEBAB0EAwFgYUqTAAAAAElFTkSuQmCC";
    drawio_file(
        &input,
        &format!(
            r##"<mxfile><diagram name="Pictures"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="pic" value="Photo" style="shape=image;html=1;image=data:image/png,{png}" vertex="1" parent="1"><mxGeometry x="20" y="20" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="remote" value="Remote" style="shape=image;html=1;image=https://example.com/logo.png" vertex="1" parent="1"><mxGeometry x="20" y="140" width="60" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##
        ),
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("not fetched"),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains(&format!("data:image/png;base64,{png}")),
        "{svg}"
    );
    assert!(!svg.contains("example.com"), "{svg}");
}

#[test]
fn round_trips_a_drawio_file_through_svg_without_losing_the_diagram() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("flow.drawio");
    let pages = temporary.path().join("pages");
    let restored = temporary.path().join("restored.drawio");
    let again = temporary.path().join("again");
    drawio_file(
        &input,
        &format!(
            r##"<mxfile host="test"><diagram id="p1" name="Flow">{DRAWIO_FLOW}</diagram></mxfile>"##
        ),
    );
    let options = ConvertOptions {
        embed_drawio_source: true,
        ..Default::default()
    };

    convert_path(&input, &pages, &options).unwrap();
    let svg = fs::read_to_string(pages.join("page-0001.svg")).unwrap();
    assert!(svg.contains(" content=\"&lt;mxfile"), "{svg}");

    let report =
        document_svg::svg_to_document(&pages, &restored, &ReverseOptions::default()).unwrap();
    assert_eq!(report.output_format, document_svg::ReverseFormat::Drawio);
    assert!(
        report.warnings[0].contains("restored"),
        "{:?}",
        report.warnings
    );
    let diagram = fs::read_to_string(&restored).unwrap();
    assert!(
        diagram.contains(r#"<diagram id="p1" name="Flow">"#),
        "{diagram}"
    );

    // The restored diagram converts to the same SVG the original did, embedded
    // source and all: a diagram body is written across several lines, and a
    // reader turns a literal newline inside an attribute into a space, so the
    // source only survives the trip if it is escaped as a character reference.
    convert_path(&restored, &again, &options).unwrap();
    assert_eq!(
        svg,
        fs::read_to_string(again.join("page-0001.svg")).unwrap()
    );
    let plain = temporary.path().join("plain");
    convert_path(&input, &plain, &ConvertOptions::default()).unwrap();
    let plain_again = temporary.path().join("plain-again");
    convert_path(&restored, &plain_again, &ConvertOptions::default()).unwrap();
    assert_eq!(
        fs::read_to_string(plain.join("page-0001.svg")).unwrap(),
        fs::read_to_string(plain_again.join("page-0001.svg")).unwrap()
    );
}

/// Inputs that are malformed, hostile or simply strange must come back as an
/// error or a page, never as a panic and never as an unbounded run. Written as
/// a test so it runs in a debug build, where arithmetic overflow aborts.
#[test]
fn survives_malformed_and_hostile_drawio_input() {
    let temporary = TempDir::new().unwrap();
    let cases: [(&str, String); 17] = [
        ("empty", String::new()),
        ("not-xml", "this is not xml at all".into()),
        ("truncated", "<mxfile><diagram><mxGraphModel><root>".into()),
        (
            "cyclic-parents",
            r##"<mxGraphModel><root><mxCell id="a" parent="b" vertex="1"><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell><mxCell id="b" parent="a" vertex="1"><mxGeometry x="0" y="0" width="10" height="10" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "self-parent",
            r##"<mxGraphModel><root><mxCell id="a" parent="a" vertex="1"><mxGeometry width="10" height="10" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "self-loop-edge",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1"><mxGeometry width="80" height="40" as="geometry"/></mxCell><mxCell id="e" edge="1" parent="1" source="a" target="a" style="edgeStyle=orthogonalEdgeStyle;"><mxGeometry relative="1" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "edge-to-nothing",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="e" edge="1" parent="1" source="missing" target="gone"><mxGeometry relative="1" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "not-a-number",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1" style="rounded=1;strokeWidth=NaN;opacity=inf;rotation=1e400;"><mxGeometry x="NaN" y="-inf" width="1e400" height="abc" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "enormous-geometry",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1"><mxGeometry x="-1e300" y="-1e300" width="1e308" height="1e308" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "zero-size",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" value="x" parent="1" vertex="1"><mxGeometry width="0" height="0" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "negative-size",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" value="x" parent="1" vertex="1"><mxGeometry width="-40" height="-40" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "duplicate-ids",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1"><mxGeometry width="10" height="10" as="geometry"/></mxCell><mxCell id="a" parent="1" vertex="1"><mxGeometry width="10" height="10" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "unclosed-html-label",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" value="&lt;b&gt;&lt;font color=&quot;#fff&quot;&gt;never closed" style="html=1;whiteSpace=wrap;" parent="1" vertex="1"><mxGeometry width="80" height="40" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
        (
            "bare-ampersand-and-controls",
            "<mxGraphModel><root><mxCell id=\"1\"/><mxCell id=\"a\" value=\"A &amp; B &amp;#x; &amp;nope; \u{7}\" style=\"html=1;\" parent=\"1\" vertex=\"1\"><mxGeometry width=\"80\" height=\"40\" as=\"geometry\"/></mxCell></root></mxGraphModel>".into(),
        ),
        (
            "deep-groups",
            {
                let mut xml = String::from("<mxGraphModel><root><mxCell id=\"1\"/>");
                for level in 0..400 {
                    let parent = if level == 0 { "1".to_owned() } else { format!("g{}", level - 1) };
                    xml.push_str(&format!(
                        "<mxCell id=\"g{level}\" parent=\"{parent}\" vertex=\"1\" style=\"group\"><mxGeometry x=\"1\" y=\"1\" width=\"50\" height=\"50\" as=\"geometry\"/></mxCell>"
                    ));
                }
                xml.push_str("</root></mxGraphModel>");
                xml
            },
        ),
        (
            "many-waypoints",
            {
                let mut xml = String::from("<mxGraphModel><root><mxCell id=\"1\"/><mxCell id=\"e\" edge=\"1\" parent=\"1\" style=\"edgeStyle=orthogonalEdgeStyle;rounded=1;\"><mxGeometry relative=\"1\" as=\"geometry\"><mxPoint x=\"0\" y=\"0\" as=\"sourcePoint\"/><mxPoint x=\"900\" y=\"900\" as=\"targetPoint\"/><Array as=\"points\">");
                for index in 0..2_000 {
                    xml.push_str(&format!("<mxPoint x=\"{}\" y=\"{}\"/>", index % 97, index % 89));
                }
                xml.push_str("</Array></mxGeometry></mxCell></root></mxGraphModel>");
                xml
            },
        ),
        (
            "image-payload-garbage",
            r##"<mxGraphModel><root><mxCell id="1"/><mxCell id="a" parent="1" vertex="1" style="shape=image;image=data:image/png,!!!not base64!!!"><mxGeometry width="40" height="40" as="geometry"/></mxCell></root></mxGraphModel>"##.into(),
        ),
    ];
    for (name, body) in cases {
        let input = temporary.path().join(format!("{name}.drawio"));
        let output = temporary.path().join(name);
        fs::write(&input, &body).unwrap();

        // Either outcome is fine; hanging, panicking or overflowing is not.
        match convert_path(&input, &output, &ConvertOptions::default()) {
            Ok(report) => {
                assert!(report.page_count >= 1, "{name}");
                for page in &report.pages {
                    // A page has to stay a page: positive, finite and small
                    // enough that a renderer can open it.
                    assert!(
                        page.width_points.is_finite() && page.width_points > 0.0,
                        "{name}"
                    );
                    assert!(
                        page.height_points.is_finite() && page.height_points > 0.0,
                        "{name}"
                    );
                    assert!(
                        page.width_points <= 1_000_000.0,
                        "{name}: {}",
                        page.width_points
                    );
                    assert!(
                        page.height_points <= 1_000_000.0,
                        "{name}: {}",
                        page.height_points
                    );
                    let svg = fs::read_to_string(output.join(&page.svg)).unwrap();
                    assert!(svg.starts_with("<?xml"), "{name}");
                    assert!(!svg.contains("NaN"), "{name}: {svg:.400}");
                }
            }
            Err(error) => {
                let text = error.to_string();
                assert!(!text.is_empty(), "{name}");
            }
        }
    }
}

#[test]
fn refuses_a_drawio_diagram_that_expands_past_the_entry_limit() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bomb.drawio");
    let output = temporary.path().join("out");
    // 8 MiB of one repeated byte compresses to a few kilobytes.
    let payload = "A".repeat(8 * 1024 * 1024);
    drawio_file(
        &input,
        &format!(
            r##"<mxfile><diagram name="Bomb">{}</diagram></mxfile>"##,
            packed_diagram(&payload)
        ),
    );
    let options = ConvertOptions {
        max_zip_entry_bytes: 64 * 1024,
        ..Default::default()
    };

    let error = convert_path(&input, &output, &options).unwrap_err();

    assert!(error.to_string().contains("expands past"), "{error}");
    assert!(!output.join("page-0001.svg").exists());
}

#[test]
fn refuses_more_drawio_diagrams_than_the_page_limit_allows() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("many.drawio");
    let output = temporary.path().join("out");
    let mut body = String::from("<mxfile>");
    for index in 0..12 {
        body.push_str(&format!(
            r##"<diagram id="p{index}" name="P{index}"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/><mxCell id="a{index}" value="{index}" style="rounded=1;" vertex="1" parent="1"><mxGeometry width="40" height="20" as="geometry"/></mxCell></root></mxGraphModel></diagram>"##
        ));
    }
    body.push_str("</mxfile>");
    drawio_file(&input, &body);
    let options = ConvertOptions {
        max_pages: 4,
        ..Default::default()
    };

    let error = convert_path(&input, &output, &options).unwrap_err();

    assert!(error.to_string().contains("maximum is 4"), "{error}");
}

#[test]
fn refuses_a_drawio_model_that_exceeds_the_xml_event_budget() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("chatty.drawio");
    let output = temporary.path().join("out");
    let mut body =
        String::from(r#"<mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>"#);
    for index in 0..2_000 {
        body.push_str(&format!(
            r##"<mxCell id="c{index}" vertex="1" parent="1"><mxGeometry width="10" height="10" as="geometry"/></mxCell>"##
        ));
    }
    body.push_str("</root></mxGraphModel>");
    drawio_file(&input, &body);
    let options = ConvertOptions {
        max_xml_events: 500,
        ..Default::default()
    };

    let error = convert_path(&input, &output, &options).unwrap_err();

    assert!(error.to_string().contains("events"), "{error}");
}

/// mxGraph looks a style token without `=` up in the stylesheet's named styles
/// and ignores what it does not find. Treating any such token as a shape turned
/// the stray `9E9E9E` left in draw.io's own GCP templates into an unknown shape
/// warning on 54 of the project's example diagrams.
#[test]
fn ignores_a_style_token_that_names_no_shape() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("stray.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Stray"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="stray" value="Card" style="fillColor=#F6F6F6;fontColor=#717171;9E9E9E;verticalAlign=top;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="200" height="60" as="geometry"/></mxCell>
<mxCell id="named" value="Oval" style="ellipse;whiteSpace=wrap;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="120" width="120" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The stray token leaves a plain rectangle; the leading `ellipse` still
    // selects a shape, because that one does name a style draw.io defines.
    assert!(svg.contains("data-semantic-role=\"rectangle\""), "{svg}");
    assert!(svg.contains("data-semantic-role=\"ellipse\""), "{svg}");
}

#[test]
fn draws_the_container_shapes_a_real_diagram_uses() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("containers.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Containers"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="grp" style="group" vertex="1" connectable="0" parent="1"><mxGeometry x="20" y="20" width="200" height="100" as="geometry"/></mxCell>
<mxCell id="inner" value="In group" style="rounded=1;html=1;" vertex="1" parent="grp"><mxGeometry x="10" y="10" width="100" height="40" as="geometry"/></mxCell>
<mxCell id="cell" value="Cell" style="shape=partialRectangle;top=0;left=0;html=1;fillColor=#EEEEEE;" vertex="1" parent="1"><mxGeometry x="20" y="160" width="120" height="40" as="geometry"/></mxCell>
<mxCell id="point" style="shape=waypoint;" vertex="1" parent="1"><mxGeometry x="300" y="160" width="20" height="20" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A group and a waypoint position things without drawing anything.
    assert!(!svg.contains("id=\"drawio-grp\""), "{svg}");
    assert!(!svg.contains("id=\"drawio-point\""), "{svg}");
    // The child is placed relative to the group it sits in: 20 + 10, with the
    // rounded corner the style asks for starting the path 6 further along.
    assert!(svg.contains(r#"d="M 36 30 H 124 A 6 6"#), "{svg}");
    // A partial rectangle fills, and strokes only the sides it switches on.
    assert!(svg.contains("#EEEEEE"), "{svg}");
    assert!(svg.contains("id=\"drawio-cell-detail-0\""), "{svg}");
    assert!(!svg.contains("id=\"drawio-cell-detail-2\""), "{svg}");
}

#[test]
fn says_what_a_drawio_shape_library_is_instead_of_calling_it_empty() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shapes.xml");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r#"<mxlibrary>[{"xml":"...","w":100,"h":100}]</mxlibrary>"#,
    );

    let error = convert_path(&input, &output, &ConvertOptions::default()).unwrap_err();

    assert!(error.to_string().contains("shape library"), "{error}");
}

/// A shape library is a drawing program per shape, in the shape's own
/// coordinate space. Reading it is what puts the real icon on the page instead
/// of a labelled box.
#[test]
fn draws_a_shape_from_a_library_the_caller_supplies() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    fs::write(
        stencils.join("demo.xml"),
        r##"<shapes name="mxgraph.demo">
<shape name="Round Badge" h="20" w="20" aspect="fixed" strokewidth="inherit">
  <connections/>
  <background><ellipse x="0" y="0" w="20" h="20"/></background>
  <foreground>
    <fillstroke/>
    <fillcolor color="#123456"/>
    <path><move x="4" y="10"/><line x="16" y="10"/><close/></path>
    <fill/>
  </foreground>
</shape>
</shapes>"##,
    )
    .unwrap();
    let input = temporary.path().join("library.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Library"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="badge" value="Badge" style="shape=mxgraph.demo.round_badge;fillColor=#FFEEDD;strokeColor=#884400;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="40" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils],
        ..Default::default()
    };

    let report = convert_path(&input, &output, &options).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("data-content-kind=\"drawio-stencil\""),
        "{svg}"
    );
    // The shape's own 20x20 space is scaled onto the cell's 40x40 box, and the
    // cell's colours are what the stencil paints with.
    assert!(svg.contains("#FFEEDD") && svg.contains("#884400"), "{svg}");
    // The stencil's own colour overrides the cell's for what follows it.
    assert!(svg.contains("#123456"), "{svg}");
    assert!(svg.contains(">Badge</tspan>"), "{svg}");

    // Without the library the same diagram says so, and still shows the label.
    let plain = temporary.path().join("plain");
    let bare = convert_path(&input, &plain, &ConvertOptions::default()).unwrap();
    assert!(
        bare.warnings[0].contains("mxgraph.demo.round_badge"),
        "{:?}",
        bare.warnings
    );
    assert!(
        fs::read_to_string(plain.join("page-0001.svg"))
            .unwrap()
            .contains(">Badge</tspan>")
    );
}

#[test]
fn draws_a_shape_the_diagram_carries_inline() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("inline.drawio");
    let output = temporary.path().join("out");
    let shape = r#"<shape h="10" w="10" aspect="variable" strokewidth="inherit"><connections/><background><rect x="0" y="0" w="10" h="10"/></background><foreground><fillstroke/></foreground></shape>"#;
    drawio_file(
        &input,
        &format!(
            r##"<mxfile><diagram name="Inline"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="own" value="Mine" style="shape=stencil({});fillColor=#AABBCC;" vertex="1" parent="1"><mxGeometry x="10" y="10" width="60" height="30" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
            packed_diagram(shape)
        ),
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("data-content-kind=\"drawio-stencil\""),
        "{svg}"
    );
    assert!(svg.contains("#AABBCC"), "{svg}");
    // The shape's 10x10 space is stretched over the whole 60x30 cell.
    assert!(svg.contains("M 10 10 H 70 V 40 H 10 Z"), "{svg}");
}

#[test]
fn draws_the_glyph_a_cloud_tile_names_in_its_style() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    fs::write(
        stencils.join("cloud.xml"),
        r##"<shapes name="mxgraph.cloud">
<shape name="Bucket" h="10" w="10" aspect="fixed" strokewidth="inherit">
  <connections/><background><rect x="0" y="0" w="10" h="10"/></background>
  <foreground><fillstroke/></foreground>
</shape>
</shapes>"##,
    )
    .unwrap();
    let input = temporary.path().join("tile.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Tile"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="tile" value="Storage" style="shape=mxgraph.aws4.resourceIcon;resIcon=mxgraph.cloud.bucket;fillColor=#3334B9;strokeColor=#ffffff;" vertex="1" parent="1"><mxGeometry x="0" y="0" width="100" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils],
        ..Default::default()
    };

    let report = convert_path(&input, &output, &options).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The tile keeps the style's colour, and the glyph sits at seven tenths of
    // it in white.
    assert!(svg.contains("#3334B9"), "{svg}");
    assert!(
        svg.contains("data-content-kind=\"drawio-stencil-icon\""),
        "{svg}"
    );
    assert!(svg.contains("M 15 15 H 85 V 85 H 15 Z"), "{svg}");
}

/// Entity-relationship ends are the notation an ER diagram is read by: a tick
/// for one, a crow's foot for many, and a ring for zero.
#[test]
fn draws_entity_relationship_connector_ends() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("er.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="ER"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" value="Order" style="shape=table;startSize=30;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="160" height="90" as="geometry"/></mxCell>
<mxCell id="b" value="Line" style="shape=table;startSize=30;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="20" width="160" height="90" as="geometry"/></mxCell>
<mxCell id="e" style="edgeStyle=entityRelationEdgeStyle;html=1;startArrow=ERmandOne;endArrow=ERzeroToMany;" edge="1" parent="1" source="a" target="b"><mxGeometry relative="1" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Two ticks at the mandatory-one end, and a ring plus a crow's foot at the
    // other. The ring is filled white so the line does not show through it.
    assert_eq!(
        svg.matches("data-content-kind=\"drawio-edge-marker\"")
            .count(),
        3,
        "{svg}"
    );
    assert!(svg.contains("fill=\"#FFFFFF\""), "{svg}");
    // A table is a swimlane: its name sits in the band above the rows.
    assert!(svg.contains("data-semantic-role=\"swimlane\""), "{svg}");
}

#[test]
fn draws_bpmn_events_gateways_and_their_symbols() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bpmn.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="BPMN"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="start" value="Order" style="shape=mxgraph.bpmn.shape;outline=standard;symbol=message;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="gate" value="Split" style="shape=mxgraph.bpmn.shape;background=gateway;outline=none;symbol=parallelGw;html=1;" vertex="1" parent="1"><mxGeometry x="120" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="end" value="Done" style="shape=mxgraph.bpmn.shape;outline=end;symbol=terminate;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="task" value="Pack" style="shape=mxgraph.bpmn.task;rectStyle=rounded;size=10;html=1;" vertex="1" parent="1"><mxGeometry x="320" y="20" width="100" height="50" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"drawio-bpmn\""), "{svg}");
    // A gateway is a diamond with the symbol in its middle half.
    assert!(
        svg.contains("M 145 20 L 170 45 L 145 70 L 120 45 Z"),
        "{svg}"
    );
    // An end event's ring is three times the stroke width, and its terminate
    // symbol takes the line colour because the event throws.
    assert!(svg.contains("stroke-width=\"3\""), "{svg}");
    // The activity keeps the corner radius its own style asks for.
    assert!(svg.contains("A 10 10 0 0 1"), "{svg}");
}

#[test]
fn draws_uml_components_and_packages() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("uml.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="UML"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="c" value="Service" style="shape=component;align=left;spacingLeft=36;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="180" height="90" as="geometry"/></mxCell>
<mxCell id="p" value="Domain" style="shape=folder;tabWidth=80;tabHeight=20;tabPosition=left;labelInHeader=1;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="20" width="200" height="120" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The component keeps its two sockets on the left edge.
    assert_eq!(svg.matches("drawio-c-detail-").count(), 2, "{svg}");
    // The package's name sits in the tab, not in the middle of the box.
    assert!(svg.contains(">Domain</tspan>"), "{svg}");
    let label = svg
        .lines()
        .find(|line| line.contains("drawio-p-label-0"))
        .unwrap();
    let baseline = label
        .split("y=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap();
    // The tab runs from 20 to 40 in the model's own pixels.
    assert!(
        (20.0..40.0).contains(&baseline),
        "the name is in the tab: {label}"
    );
}

/// The shapes draw.io implements in code rather than as stencils still have to
/// be drawn: a ring segment keeps its angles, a mock-up grid keeps its cells,
/// and an AWS 3D service keeps the isometric body it stands on.
#[test]
fn draws_the_library_shapes_that_have_no_stencil() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("coded.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Coded"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="ring" style="shape=mxgraph.basic.partConcEllipse;startAngle=0;endAngle=0.25;arcWidth=0.5;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="grid" style="shape=mxgraph.mockup.graphics.iconGrid;gridSize=2,2;html=1;" vertex="1" parent="1"><mxGeometry x="140" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="ribbon" value="Step" style="shape=mxgraph.infographic.ribbonSimple;notch1=20;notch2=20;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="20" width="160" height="40" as="geometry"/></mxCell>
<mxCell id="box" value="Server" style="shape=mxgraph.aws3d.application_server;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="140" width="100" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    // Only the AWS service says something, and it says exactly what is missing.
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("service glyph it carries is not drawn"),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A quarter ring, drawn from the top round to the right and back.
    assert!(
        svg.contains("M 60 20 A 40 40 0 0 1 100 60 L 80 60 A 20 20 0 0 0 60 40 Z"),
        "{svg}"
    );
    // Two by two cells, each two thirds of the box with a gap between them.
    assert_eq!(svg.matches("id=\"drawio-grid\"").count(), 1, "{svg}");
    assert!(svg.contains("M 140 20 H 160 V 40 H 140 Z"), "{svg}");
    // The ribbon keeps its notches.
    assert!(
        svg.contains("M 220 60 L 240 40 L 220 20 L 360 20 L 380 40 L 360 60 Z"),
        "{svg}"
    );
    // The isometric body and its two shaded faces.
    assert!(
        svg.contains("data-content-kind=\"drawio-isometric\""),
        "{svg}"
    );
    assert_eq!(
        svg.matches("data-content-kind=\"drawio-isometric-face\"")
            .count(),
        2,
        "{svg}"
    );
}

/// The stencil loader reads files the caller points it at, so it has to hold up
/// against whatever is in that directory: files that are not stencils, shapes
/// with no geometry, and numbers that are not numbers.
#[test]
fn survives_stencil_libraries_that_are_malformed_or_hostile() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    fs::write(stencils.join("not-xml.xml"), "this is not xml at all").unwrap();
    fs::write(stencils.join("empty.xml"), "").unwrap();
    fs::write(stencils.join("notes.txt"), "ignored: not an xml file").unwrap();
    fs::write(
        stencils.join("odd.xml"),
        concat!(
            r##"<shapes name="mxgraph.odd">"##,
            // No geometry at all.
            r##"<shape name="Bare" w="0" h="0"/>"##,
            // Numbers that are not numbers, and a shape that never closes its
            // path before painting.
            r##"<shape name="Broken" w="NaN" h="-1e400" aspect="fixed" strokewidth="wide">"##,
            r##"<foreground><fill/><stroke/><restore/><restore/>"##,
            r##"<path><line x="1e400" y="NaN"/><arc rx="-5" ry="0" x="10" y="10"/></path>"##,
            r##"<fillcolor color="not a colour"/><strokewidth width="1e400"/><alpha alpha="5"/>"##,
            r##"<fillstroke/></foreground></shape>"##,
            // A deeply nested body, which must not be followed into recursion.
            r##"<shape name="Nested" w="10" h="10"><foreground>"##,
            r##"<save/><save/><save/><path><move x="0" y="0"/><line x="10" y="10"/></path><fillstroke/>"##,
            r##"</foreground></shape></shapes>"##,
        ),
    )
    .unwrap();
    let input = temporary.path().join("odd.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Odd"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" value="Bare" style="shape=mxgraph.odd.bare;html=1;" vertex="1" parent="1"><mxGeometry x="10" y="10" width="40" height="40" as="geometry"/></mxCell>
<mxCell id="b" value="Broken" style="shape=mxgraph.odd.broken;html=1;" vertex="1" parent="1"><mxGeometry x="70" y="10" width="40" height="40" as="geometry"/></mxCell>
<mxCell id="c" value="Nested" style="shape=mxgraph.odd.nested;html=1;" vertex="1" parent="1"><mxGeometry x="130" y="10" width="40" height="40" as="geometry"/></mxCell>
<mxCell id="d" value="Absent" style="shape=mxgraph.odd.absent;html=1;" vertex="1" parent="1"><mxGeometry x="190" y="10" width="40" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils, temporary.path().join("missing-directory")],
        ..Default::default()
    };

    // A directory that is not there is an error the caller can act on; nothing
    // in the files themselves may panic or run away.
    let error = convert_path(&input, &output, &options).unwrap_err();
    assert!(error.to_string().contains("No such file"), "{error}");

    let options = ConvertOptions {
        stencil_paths: options.stencil_paths[..1].to_vec(),
        ..Default::default()
    };
    let report = convert_path(&input, &output, &options).unwrap();

    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(!svg.contains("NaN"), "{svg}");
    // Every label survives, whatever happened to the shape around it.
    for label in ["Bare", "Broken", "Nested", "Absent"] {
        assert!(svg.contains(&format!(">{label}</tspan>")), "{label}: {svg}");
    }
    // The shape that was not in any library is the only one reported.
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("mxgraph.odd.absent"),
        "{:?}",
        report.warnings
    );
}

#[test]
fn refuses_a_stencil_library_that_exceeds_the_event_budget() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    let mut library = String::from(
        r##"<shapes name="mxgraph.big"><shape name="Long" w="10" h="10"><foreground><path>"##,
    );
    for index in 0..4_000 {
        library.push_str(&format!(
            r##"<line x="{}" y="{}"/>"##,
            index % 10,
            index % 7
        ));
    }
    library.push_str("</path><fillstroke/></foreground></shape></shapes>");
    fs::write(stencils.join("big.xml"), library).unwrap();
    let input = temporary.path().join("big.drawio");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Big"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" style="shape=mxgraph.big.long;html=1;" vertex="1" parent="1"><mxGeometry width="40" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils],
        max_xml_events: 500,
        ..Default::default()
    };

    let error = convert_path(&input, temporary.path().join("out"), &options).unwrap_err();

    assert!(error.to_string().contains("events"), "{error}");
}

#[test]
fn draws_the_shapes_that_need_more_than_one_colour() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("shaded.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Shaded"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="pie" style="shape=mxgraph.mockup.graphics.pieChart;parts=25,25,50;partColors=#FF0000,#00FF00,#0000FF;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="cube" style="shape=mxgraph.infographic.shadedCube;isoAngle=15;html=1;fillColor=#DDDDDD;" vertex="1" parent="1"><mxGeometry x="140" y="20" width="80" height="80" as="geometry"/></mxCell>
<mxCell id="brace" style="shape=mxgraph.mockup.markup.curlyBrace;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="140" width="120" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Three slices, each in the colour the style lists for it.
    for colour in ["#FF0000", "#00FF00", "#0000FF"] {
        assert!(svg.contains(colour), "{colour}: {svg}");
    }
    // The cube's body plus its two shaded faces.
    assert_eq!(
        svg.matches("data-content-kind=\"drawio-shaded\"").count(),
        7,
        "{svg}"
    );
    assert!(svg.contains("fill-opacity=\"0.2\""), "{svg}");
    // The brace is a line, so it carries no fill of its own.
    let brace = svg
        .lines()
        .find(|line| line.contains("drawio-brace-detail-0"))
        .unwrap();
    assert!(brace.contains("fill=\"none\""), "{brace}");
}

/// A connector has to meet the outline it points at, not the box around it.
#[test]
fn attaches_connectors_to_the_outline_the_perimeter_names() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("perimeter.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Perimeter"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="tri" style="triangle;perimeter=trianglePerimeter;html=1;" vertex="1" parent="1"><mxGeometry x="0" y="0" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="far" style="rounded=0;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="40" width="40" height="20" as="geometry"/></mxCell>
<mxCell id="e" style="html=1;" edge="1" parent="1" source="tri" target="far"><mxGeometry relative="1" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let edge = svg
        .lines()
        .find(|line| line.contains(r#"id="drawio-e""#))
        .unwrap();
    // The triangle points right, so the line starts at its tip, not at the
    // right edge of its bounding box at the centre height.
    assert!(edge.contains("M 100 50 L"), "{edge}");
}

#[test]
fn keeps_a_hidden_overflow_label_inside_its_shape() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("overflow.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Overflow"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="clipped" value="A label far longer than the box it belongs to" style="rounded=0;html=1;overflow=hidden;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="60" height="30" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("<clipPath id=\"drawio-clipped-label-clip\""),
        "{svg}"
    );
    assert!(
        svg.contains("clip-path=\"url(#drawio-clipped-label-clip)\""),
        "{svg}"
    );
    // The page is the shape plus the margin: the label does not stretch it.
    assert_eq!(report.pages[0].width_points, 60.0);
}

/// draw.io's stylesheet gives a bare style token its own settings, which is how
/// `plain-yellow` colours a shape, and `glass=1` lays a sheen over the top of
/// it. Ignoring either left the shape white.
#[test]
fn applies_the_named_styles_drawio_ships_with() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("named.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Named"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="a" value="Yellow" style="ellipse;whiteSpace=wrap;html=1;plain-yellow;strokeWidth=2;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="120" height="60" as="geometry"/></mxCell>
<mxCell id="b" value="Override" style="rounded=1;html=1;plain-orange;glass=1;fillColor=#123456;" vertex="1" parent="1"><mxGeometry x="180" y="20" width="120" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The named style brings a fill, a border and a gradient with it.
    assert!(svg.contains("#FFF2CC") && svg.contains("#D6B656"), "{svg}");
    assert!(svg.contains("#FFD966"), "the gradient stop: {svg}");
    // A setting after the name still wins over it.
    assert!(svg.contains("#123456"), "{svg}");
    assert!(
        !svg.contains("#FFCD28"),
        "the overridden fill is gone: {svg}"
    );
    // The sheen is a white gradient over the top of the shape.
    assert!(svg.contains("data-content-kind=\"drawio-glass\""), "{svg}");
    assert!(svg.contains("stop-opacity=\"0.9\""), "{svg}");
}

/// A value stream map is drawn almost entirely out of lean-mapping shapes: the
/// process boxes with a band for their name, the data boxes ruled underneath
/// them, and the striped push and hooked pull arrows that join them.
#[test]
fn draws_the_lean_mapping_shapes_a_value_stream_map_is_made_of() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("vsm.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="VSM"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="proc" value="Wafer" style="shape=mxgraph.lean_mapping.manufacturing_process;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="120" height="60" as="geometry"/></mxCell>
<mxCell id="data" value="C/T= 2 min" style="shape=mxgraph.lean_mapping.data_box;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="100" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="push" style="shape=mxgraph.lean_mapping.push_arrow;html=1;" vertex="1" parent="1"><mxGeometry x="160" y="20" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="pull" style="shape=mxgraph.lean_mapping.physical_pull;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="20" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="plan" value="Schedule" style="shape=mxgraph.lean_mapping.schedule;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="140" width="100" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The process box wears a band the height of one and a half lines of text.
    assert!(svg.contains("M 20 20 H 140 V 80 H 20 Z"), "{svg}");
    assert!(svg.contains("M 20 32 L 140 32"), "{svg}");
    // The data box is open at the top and ruled into five rows.
    assert!(
        svg.contains("M 20 200 L 20 100 L 120 100 L 120 200"),
        "{svg}"
    );
    for rule in ["M 20 120 L 120 120", "M 20 180 L 120 180"] {
        assert!(svg.contains(rule), "{rule} missing from {svg}");
    }
    // The push arrow keeps its three stripes.
    assert!(
        svg.contains("M 160 37 L 235 37 L 235 20 L 260 70 L 235 120 L 235 103 L 160 103 Z"),
        "{svg}"
    );
    for stripe in [
        "M 160 37 H 172 V 103 H 160 Z",
        "M 184 37 H 196 V 103 H 184 Z",
        "M 208 37 H 220 V 103 H 208 Z",
    ] {
        assert!(svg.contains(stripe), "{stripe} missing from {svg}");
    }
    // The pull arrow is a near-full circle ending in a solid head.
    assert!(
        svg.contains("M 373.2 27.36 A 48.27 49.59 0 1 0 395.53 81.91"),
        "{svg}"
    );
    assert!(
        svg.contains("M 390.71 81.91 L 397.94 69.51 L 400 84.38 Z"),
        "{svg}"
    );
    // A schedule is a plain box, and it keeps its label.
    assert!(svg.contains("M 300 140 H 400 V 180 H 300 Z"), "{svg}");
    assert!(svg.contains("Schedule"), "{svg}");
}

/// Callouts and the shaded infographic shapes. The banners and pyramids are
/// drawn as several layers — a face, a light side and a dark one — so the test
/// checks that the layers are there as well as that the outline is right.
#[test]
fn draws_the_callouts_and_shaded_shapes_an_infographic_is_built_from() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("info.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Info"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="bar" style="shape=mxgraph.infographic.barCallout;dx=60;dy=20;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="180" height="60" as="geometry"/></mxCell>
<mxCell id="rect" style="shape=mxgraph.basic.rectCallout;dx=30;dy=20;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="20" width="140" height="70" as="geometry"/></mxCell>
<mxCell id="round" style="shape=mxgraph.basic.roundRectCallout;dx=60;dy=20;size=10;html=1;" vertex="1" parent="1"><mxGeometry x="380" y="20" width="140" height="70" as="geometry"/></mxCell>
<mxCell id="tri" style="shape=mxgraph.infographic.shadedTriangle;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="120" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="pyr" style="shape=mxgraph.infographic.shadedPyramid;html=1;" vertex="1" parent="1"><mxGeometry x="140" y="120" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="step" style="shape=mxgraph.infographic.pyramidStep;html=1;" vertex="1" parent="1"><mxGeometry x="260" y="120" width="100" height="60" as="geometry"/></mxCell>
<mxCell id="banner" style="shape=mxgraph.infographic.banner;dx=25;dy=15;notch=15;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="260" width="240" height="80" as="geometry"/></mxCell>
<mxCell id="fold" style="shape=mxgraph.infographic.bannerSingleFold;dx=32;dy=15;dx2=20;notch=15;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="260" width="240" height="80" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A bar with a pointer hanging seven pixels either side of x = 60.
    assert!(
        svg.contains("M 20 20 L 200 20 L 200 60 L 87 60 L 80 80 L 73 60 L 20 60 Z"),
        "{svg}"
    );
    // A speech box, its tail below the body rather than inside it.
    assert!(
        svg.contains("M 240 70 L 220 70 L 220 20 L 360 20 L 360 70 L 260 70 L 230 90 Z"),
        "{svg}"
    );
    // The rounded one turns each corner with an arc of the requested size.
    assert!(
        svg.contains("M 430 70 L 390 70 A 10 10 0 0 1 380 60 L 380 30 A 10 10 0 0 1 390 20"),
        "{svg}"
    );
    assert!(
        svg.contains("A 38 28 0 0 1 420 90 A 18 28 0 0 0 430 70 Z"),
        "{svg}"
    );
    // The triangle is one face plus a light and a dark side, stroked over.
    assert_eq!(svg.matches("id=\"drawio-tri-part-").count(), 4, "{svg}");
    assert!(svg.contains("M 20 220 L 70 120 L 120 220 Z"), "{svg}");
    assert!(svg.contains("M 20 220 L 70 120 L 70 187 Z"), "{svg}");
    assert!(svg.contains("M 120 220 L 70 187 L 70 120 Z"), "{svg}");
    // The pyramid's ridge sits three tenths of its width above the base.
    assert!(
        svg.contains("M 140 190 L 190 120 L 240 190 L 190 220 Z"),
        "{svg}"
    );
    // The stepped block is a box with a shallow ridge on top.
    assert!(
        svg.contains("M 260 130 L 310 120 L 360 130 L 360 180 L 260 180 Z"),
        "{svg}"
    );
    // The ribbon banner: notched ends, folded tails, and shading on both ends.
    assert!(
        svg.contains(
            "M 20 275 L 45 275 L 45 260 L 235 260 L 235 275 L 260 275 L 245 307.5 \
             L 260 340 L 205 340 L 205 325 L 75 325 L 75 340 L 20 340 L 35 307.5 Z"
        ),
        "{svg}"
    );
    assert_eq!(svg.matches("id=\"drawio-banner-part-").count(), 5, "{svg}");
    // A single fold is pointed on the left and folded only on the right.
    assert!(
        svg.contains(
            "M 320 260 L 508 260 L 508 275 L 540 275 L 525 307.5 L 540 340 \
             L 478 340 L 478 325 L 320 325 L 300 292.5 Z"
        ),
        "{svg}"
    );
}

/// The controls a mock-up is assembled from, and the shapes several libraries
/// each define their own copy of: buttons rounded along one edge, rounded and
/// inset rectangles, checkboxes and radio buttons, and anchors, which draw
/// nothing at all because they are only there to attach a connector to.
#[test]
fn draws_the_controls_a_mock_up_is_assembled_from() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ui.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="UI"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="top" style="shape=mxgraph.mockup.containers.topButton;rSize=10;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="120" height="40" as="geometry"/></mxCell>
<mxCell id="bottom" style="shape=mxgraph.bootstrap.bottomButton;rSize=10;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="60" width="120" height="40" as="geometry"/></mxCell>
<mxCell id="left" style="shape=mxgraph.mockup.leftButton;rSize=10;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="20" width="60" height="40" as="geometry"/></mxCell>
<mxCell id="right" style="shape=mxgraph.bootstrap.rightButton;rSize=10;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="20" width="60" height="40" as="geometry"/></mxCell>
<mxCell id="round" style="shape=mxgraph.mockup.forms.rrect;rSize=15;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="80" width="120" height="50" as="geometry"/></mxCell>
<mxCell id="inset" style="shape=mxgraph.mockup.containers.marginRect;rectMargin=10;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="20" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="plain" style="shape=mxgraph.gmdl.marginRect;rectMargin=5;rectMarginTop=15;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="90" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="open" style="shape=mxgraph.mockup.forms.uRect;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="120" width="120" height="40" as="geometry"/></mxCell>
<mxCell id="tick" style="shape=mxgraph.bootstrap.checkbox;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="150" width="24" height="24" as="geometry"/></mxCell>
<mxCell id="radio" style="shape=mxgraph.bootstrap.radioButton;strokeColor=#0085FC;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="150" width="24" height="24" as="geometry"/></mxCell>
<mxCell id="hold" style="shape=mxgraph.ios7ui.anchor;html=1;" vertex="1" parent="1"><mxGeometry x="500" y="20" width="10" height="10" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Each button rounds the two corners on its own edge and squares the rest.
    for (id, d) in [
        (
            "top",
            "M 20 30 A 10 10 0 0 1 30 20 L 130 20 A 10 10 0 0 1 140 30 L 140 60 L 20 60 Z",
        ),
        (
            "bottom",
            "M 140 90 A 10 10 0 0 1 130 100 L 30 100 A 10 10 0 0 1 20 90 L 20 60 L 140 60 Z",
        ),
        (
            "left",
            "M 190 60 A 10 10 0 0 1 180 50 L 180 30 A 10 10 0 0 1 190 20 L 240 20 L 240 60 Z",
        ),
        (
            "right",
            "M 290 20 A 10 10 0 0 1 300 30 L 300 50 A 10 10 0 0 1 290 60 L 240 60 L 240 20 Z",
        ),
    ] {
        assert!(
            svg.contains(&format!("id=\"drawio-{id}\" d=\"{d}\"")),
            "{id} missing from {svg}"
        );
    }
    // A rounded rectangle takes its radius from `rSize`, not from `arcSize`.
    assert!(svg.contains("M 195 80 H 285 A 15 15 0 0 1 300 95"), "{svg}");
    // An inset rectangle is rounded by ten, and its plain form by nothing.
    assert!(svg.contains("M 360 30 H 460 A 10 10 0 0 1 470 40"), "{svg}");
    assert!(svg.contains("M 345 110 H 475 V 145 H 345 Z"), "{svg}");
    // Three sides of a box, open along the bottom.
    assert!(
        svg.contains("M 20 160 L 20 120 L 140 120 L 140 160"),
        "{svg}"
    );
    // A checkbox is rounded by three whatever its size, and carries a tick.
    assert!(svg.contains("M 183 150 H 201 A 3 3 0 0 1 204 153"), "{svg}");
    assert!(
        svg.contains("d=\"M 199.2 154.8 L 189.6 169.2 L 186 164.4\""),
        "{svg}"
    );
    // A radio button's dot takes the stroke colour rather than the fill.
    assert!(
        svg.contains("M 226 162 A 6 6 0 1 0 238 162 A 6 6 0 1 0 226 162 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#0085FC\""),
        "{svg}"
    );
    // An anchor has no outline of its own.
    assert!(!svg.contains("id=\"drawio-hold\""), "{svg}");
}

/// The shapes draw.io registers under a plain name, which a diagram reaches for
/// without naming a library: the isometric faces, the brace and the pie slice.
#[test]
fn draws_the_isometric_faces_the_brace_and_the_pie_slice() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("iso.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Iso"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="face" style="shape=isoRectangle;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="120" height="80" as="geometry"/></mxCell>
<mxCell id="cube" style="shape=isoCube2;isoAngle=15;html=1;" vertex="1" parent="1"><mxGeometry x="170" y="20" width="100" height="120" as="geometry"/></mxCell>
<mxCell id="brace" style="shape=curlyBracket;size=0.5;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="20" width="40" height="120" as="geometry"/></mxCell>
<mxCell id="slice" style="shape=mxgraph.basic.pie;startAngle=0.25;endAngle=0.9;html=1;" vertex="1" parent="1"><mxGeometry x="370" y="20" width="100" height="100" as="geometry"/></mxCell>
<mxCell id="half" style="shape=mxgraph.basic.pie;startAngle=0;endAngle=0.5;html=1;" vertex="1" parent="1"><mxGeometry x="490" y="20" width="100" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A rhombus on a thirty degree grid, centred in a box wider than it is.
    assert!(
        svg.contains("M 20 60 L 80 25.359 L 140 60 L 80 94.641 Z"),
        "{svg}"
    );
    // A cube: six sides, with a Y drawn over it to divide the three faces.
    assert!(
        svg.contains("M 220 20 L 270 44.008 L 270 115.992 L 220 140 L 170 115.992 L 170 44.008 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 170 44.008 L 220 68.016 L 270 44.008"),
        "{svg}"
    );
    assert!(svg.contains("M 220 68.016 L 220 140"), "{svg}");
    // A brace is drawn open, so it has strokes but no outline to fill.
    assert!(!svg.contains("id=\"drawio-brace\" "), "{svg}");
    assert!(
        svg.contains("M 340 20 L 320 20 L 320 80 L 300 80 L 320 80 L 320 140 L 340 140"),
        "{svg}"
    );
    // Two thirds of a turn is drawn as two arcs, because one arc of half a
    // turn or more leaves the direction it sweeps in undefined.
    assert!(
        svg.contains("M 420 70 L 470 70 A 50 50 0 0 1 397.3 114.55 A 50 50 0 0 1 390.611 29.549 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 540 70 L 540 20 A 50 50 0 0 1 590 70 A 50 50 0 0 1 540 120 Z"),
        "{svg}"
    );
}

/// The SysML activity and flow nodes draw.io implements in code. Each is a
/// body with square ports let into its sides, and because every part takes the
/// same fill and stroke they are drawn as one path with several subpaths.
#[test]
fn draws_the_sysml_activity_and_flow_nodes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("sysml.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="SysML"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="done" style="shape=mxgraph.sysml.actFinal;strokeColor=#FF0000;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="stop" style="shape=mxgraph.sysml.flowFinal;html=1;" vertex="1" parent="1"><mxGeometry x="90" y="20" width="50" height="50" as="geometry"/></mxCell>
<mxCell id="ctrl" style="shape=mxgraph.sysml.isControl;html=1;" vertex="1" parent="1"><mxGeometry x="170" y="20" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="inflow" style="shape=mxgraph.sysml.objFlowL;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="20" width="120" height="50" as="geometry"/></mxCell>
<mxCell id="outflow" style="shape=mxgraph.sysml.objFlowR;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="90" width="120" height="50" as="geometry"/></mxCell>
<mxCell id="items" style="shape=mxgraph.sysml.itemFlowRight;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="100" width="140" height="120" as="geometry"/></mxCell>
<mxCell id="frame" style="shape=mxgraph.sysml.paramDgm;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="160" width="160" height="100" as="geometry"/></mxCell>
<mxCell id="short" style="shape=mxgraph.sysml.paramDgm;html=1;" vertex="1" parent="1"><mxGeometry x="520" y="160" width="160" height="50" as="geometry"/></mxCell>
<mxCell id="port" style="shape=mxgraph.sysml.port1;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="250" width="100" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // An activity end is a ring with a disc inside it, and the disc takes the
    // stroke colour rather than the fill.
    assert!(
        svg.contains("M 25 45 A 20 20 0 1 0 65 45 A 20 20 0 1 0 25 45 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#FF0000\""),
        "{svg}"
    );
    // A flow end is the same ring crossed out.
    assert!(svg.contains("M 97.25 27.25 L 132.75 62.75"), "{svg}");
    assert!(svg.contains("M 132.75 27.25 L 97.25 62.75"), "{svg}");
    // A control operator has a port at each end, an object flow one.
    assert!(
        svg.contains(
            "M 170 35 H 180 V 55 H 170 Z M 190 20 H 290 A 10 10 0 0 1 300 30 V 60 \
             A 10 10 0 0 1 290 70 H 190 A 10 10 0 0 1 180 60 V 30 A 10 10 0 0 1 190 20 Z \
             M 300 35 H 310 V 55 H 300 Z"
        ),
        "{svg}"
    );
    assert!(
        svg.contains("M 340 35 H 350 V 55 H 340 Z M 360 20 H 450"),
        "{svg}"
    );
    assert!(
        svg.contains("A 10 10 0 0 1 350 90 Z M 450 105 H 460 V 125 H 450 Z"),
        "{svg}"
    );
    // An item flow carries three ports, spaced a quarter apart down its side.
    assert!(
        svg.contains(
            "M 180 100 H 310 V 220 H 180 Z M 300 120 H 320 V 140 H 300 Z \
             M 300 150 H 320 V 170 H 300 Z M 300 180 H 320 V 200 H 300 Z"
        ),
        "{svg}"
    );
    // A parametric frame carries two ports, but only when it is tall enough.
    assert!(svg.contains("M 340 175 H 360 V 195 H 340 Z"), "{svg}");
    assert!(!svg.contains("id=\"drawio-short-detail-0\""), "{svg}");
    // A port is drawn inset by a twentieth of its box on each side.
    assert!(svg.contains("M 25 250 H 115 V 290 H 25 Z"), "{svg}");
}

/// The floor-plan shapes that are drawn from their own measurements rather than
/// from a stencil: a room's four walls, a flight of stairs with a landing, and
/// a sliding door.
#[test]
fn draws_a_room_its_stairs_and_its_sliding_door() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("plan.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Plan"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="room" style="shape=mxgraph.floorplan.room;wallThickness=10;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="200" height="140" as="geometry"/></mxCell>
<mxCell id="stairs" style="shape=mxgraph.floorplan.stairsRest;html=1;" vertex="1" parent="1"><mxGeometry x="260" y="20" width="180" height="60" as="geometry"/></mxCell>
<mxCell id="door" style="shape=mxgraph.floorplan.doorBypass;dx=0.3;html=1;" vertex="1" parent="1"><mxGeometry x="260" y="110" width="160" height="30" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The inside of the room is wound the other way round, so it reads as a
    // hole in the walls rather than as a second filled box.
    assert!(
        svg.contains("M 20 160 L 20 20 L 220 20 L 220 160 Z M 30 30 L 30 150 L 210 150 L 210 30 Z"),
        "{svg}"
    );
    // A tread every twenty five pixels, stopping short of the landing.
    assert_eq!(
        svg.matches("id=\"drawio-stairs-detail-").count(),
        8,
        "{svg}"
    );
    assert!(svg.contains("M 285 20 L 285 80"), "{svg}");
    assert!(svg.contains("M 385 20 L 385 80"), "{svg}");
    // The landing carries the arrow that says which way the stairs go up.
    assert!(svg.contains("M 440 20 L 410 50 L 440 80"), "{svg}");
    // Two panels that pass each other, between a jamb at either end.
    assert!(
        svg.contains(
            "M 260 120 H 265 V 130 H 260 Z M 415 120 H 420 V 130 H 415 Z \
             M 260 125 H 340 V 130 H 260 Z M 308 120 H 388 V 125 H 308 Z"
        ),
        "{svg}"
    );
}

/// The iOS 7 controls. Several of them carry a second colour of their own —
/// the status bar's ink, a slider's track, a switch's off state — which the
/// shape has to read from the style rather than take from the fill and stroke.
#[test]
fn draws_the_ios_controls_and_the_status_bar_above_them() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("ios.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="iOS"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="phone" style="shape=mxgraph.ios7ui.phone;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="160" height="320" as="geometry"/></mxCell>
<mxCell id="bar" style="shape=mxgraph.ios7ui.appBar;fillColor2=#222222;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="20" width="220" height="24" as="geometry"/></mxCell>
<mxCell id="dots" style="shape=mxgraph.ios7ui.pageControl;fillColor=#FFFFFF;strokeColor=#999999;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="60" width="120" height="16" as="geometry"/></mxCell>
<mxCell id="load" style="shape=mxgraph.ios7ui.downloadBar;fillColor=#DDDDDD;strokeColor=#0080F0;barPos=60;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="90" width="200" height="20" as="geometry"/></mxCell>
<mxCell id="slide" style="shape=mxgraph.ios7ui.slider;barColor=#BBBBBB;barPos=40;handleSize=14;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="130" width="200" height="20" as="geometry"/></mxCell>
<mxCell id="on" style="shape=mxgraph.ios7ui.onOffButton;buttonState=on;handleColor=#FFFFFF;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="170" width="60" height="30" as="geometry"/></mxCell>
<mxCell id="off" style="shape=mxgraph.ios7ui.onOffButton;buttonState=off;fillColor2=#FFFFFF;strokeColor2=#AAAAAA;html=1;" vertex="1" parent="1"><mxGeometry x="300" y="170" width="60" height="30" as="geometry"/></mxCell>
<mxCell id="apps" style="shape=mxgraph.ios7ui.iconGrid;gridSize=4,5;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="220" width="140" height="160" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The case is rounded by twenty five, and carries the screen, the earpiece
    // and the home button as outlines over it.
    assert!(svg.contains("M 45 20 H 155 A 25 25 0 0 1 180 45"), "{svg}");
    assert!(svg.contains("M 30 68 H 170 V 292 H 30 Z"), "{svg}");
    // The earpiece is a rounded rectangle with oval, not circular, corners.
    assert!(
        svg.contains("M 83.2 44 H 116.8 A 3.2 3 0 0 1 120 47 V 47 A 3.2 3 0 0 1 116.8 50"),
        "{svg}"
    );
    // The status bar: five signal dots, then the wifi fan over its own dot.
    assert!(
        svg.contains("M 225 32 A 1.5 1.5 0 1 0 228 32 A 1.5 1.5 0 1 0 225 32 Z"),
        "{svg}"
    );
    assert!(svg.contains("M 272 33 A 3.5 3.5 0 0 1 279 33"), "{svg}");
    assert!(svg.contains("M 270 31 A 6 6 0 0 1 282 31"), "{svg}");
    // The battery, its charge, and the glyph beside it.
    assert!(
        svg.contains("M 421 30 L 434 30 L 434 34 L 421 34 Z"),
        "{svg}"
    );
    assert!(
        svg.contains(
            "M 420 29 L 435 29 L 435 31 L 436.5 31 L 436.5 33 L 435 33 L 435 35 L 420 35 Z"
        ),
        "{svg}"
    );
    // Five page dots, the last one the odd one out in the fill colour.
    assert_eq!(svg.matches("id=\"drawio-dots-part-").count(), 5, "{svg}");
    assert!(
        svg.contains("M 328 68 A 6 6 0 1 0 340 68 A 6 6 0 1 0 328 68 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#FFFFFF\""),
        "{svg}"
    );
    // The download bar draws the whole track, then the part already done.
    assert!(svg.contains("M 220 99 H 420 V 101 H 220 Z"), "{svg}");
    assert!(svg.contains("M 220 99 H 340 V 101 H 220 Z"), "{svg}");
    // The slider's track takes `barColor`, and so does its handle's edge.
    assert!(
        svg.contains("M 220 140 L 420 140\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"none\" stroke=\"#BBBBBB\""),
        "{svg}"
    );
    assert!(svg.contains("M 293 140 A 7 7 0 1 0 307 140"), "{svg}");
    // A switch is at least twice as wide as it is tall, and its handle sits at
    // whichever end matches its state.
    assert!(svg.contains("M 251 185 A 14 14 0 1 0 279 185"), "{svg}");
    assert!(svg.contains("M 300 185 A 15 15 0 1 0 330 185"), "{svg}");
    // Twenty tiles, each a tenth of its own width clear of the next.
    assert_eq!(svg.matches("H 252.558 V").count(), 5, "{svg}");
    assert!(
        svg.contains("M 255.814 220 H 288.372 V 249.63 H 255.814 Z"),
        "{svg}"
    );
}

/// The rest of the lean mapping set: the lead time ladder, the FIFO lane and
/// the lorry, whose wheels take the stroke colour.
#[test]
fn draws_the_lead_time_ladder_the_fifo_lane_and_the_lorry() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("lean.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Lean"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="ladder" style="shape=mxgraph.lean_mapping.timeline2;dy1=0;dx2=60;dy2=1;dx3=140;dy3=0;dx4=220;dy4=1;dx5=300;dy5=0;dy6=1;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="380" height="60" as="geometry"/></mxCell>
<mxCell id="lane" style="shape=mxgraph.lean_mapping.fifo_lane;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="110" width="160" height="60" as="geometry"/></mxCell>
<mxCell id="lorry" style="shape=mxgraph.lean_mapping.truck_shipment;strokeColor=#000000;html=1;" vertex="1" parent="1"><mxGeometry x="220" y="110" width="120" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A square wave that turns where each step says, and only where it turns.
    assert!(
        svg.contains(
            "M 20 20 L 80 20 L 80 80 L 160 80 L 160 20 L 240 20 L 240 80 \
             L 320 80 L 320 20 L 400 20 L 400 80"
        ),
        "{svg}"
    );
    // The lane's band, and the three shapes queued along it.
    assert!(svg.contains("M 20 122 L 180 122"), "{svg}");
    assert!(
        svg.contains(
            "M 23.2 126 H 64.8 V 166 H 23.2 Z M 76 146 A 20.8 20 0 1 0 117.6 146 \
             A 20.8 20 0 1 0 76 146 Z M 130.4 126 L 176.8 126 L 153.6 166 Z"
        ),
        "{svg}"
    );
    // A body and a cab, on wheels drawn in the stroke colour.
    assert!(svg.contains("M 220 110 H 292 V 158 H 220 Z"), "{svg}");
    assert!(
        svg.contains("M 238 164 A 12 6 0 1 0 262 164 A 12 6 0 1 0 238 164 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#000000\""),
        "{svg}"
    );
}

/// The odds and ends a real diagram reaches for: a mock-up rule, button and
/// checkbox, a close cross, and the basic set's drop, obtuse triangle and the
/// polygon a style spells out corner by corner.
#[test]
fn draws_a_polygon_a_style_spells_out_and_the_shapes_beside_it() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("misc.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Misc"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="rule" style="shape=mxgraph.mockup.markup.line;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="180" height="20" as="geometry"/></mxCell>
<mxCell id="go" style="shape=mxgraph.mockup.buttons.button;buttonStyle=round;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="60" width="140" height="40" as="geometry"/></mxCell>
<mxCell id="next" style="shape=mxgraph.mockup.buttons.button;buttonStyle=chevron;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="60" width="140" height="40" as="geometry"/></mxCell>
<mxCell id="tick" style="shape=mxgraph.mockup.forms.checkbox;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="60" width="30" height="30" as="geometry"/></mxCell>
<mxCell id="close" style="shape=mxgraph.bootstrap.x;html=1;" vertex="1" parent="1"><mxGeometry x="390" y="60" width="20" height="20" as="geometry"/></mxCell>
<mxCell id="drop" style="shape=mxgraph.basic.drop;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="130" width="70" height="110" as="geometry"/></mxCell>
<mxCell id="wedge" style="shape=mxgraph.basic.obtuse_triangle;dx=0.2;html=1;" vertex="1" parent="1"><mxGeometry x="120" y="130" width="120" height="90" as="geometry"/></mxCell>
<mxCell id="poly" style="shape=mxgraph.basic.polygon;polyCoords=[[0,0.5],[0.35,0],[1,0.15],[0.8,1],[0.15,0.9]];html=1;" vertex="1" parent="1"><mxGeometry x="270" y="130" width="140" height="110" as="geometry"/></mxCell>
<mxCell id="curved" style="shape=mxgraph.basic.polygon;polyCoords=[[0,1],[0.5,0],[1,1]];polyCurves=[[&quot;Q&quot;,0.1,0.2],null,[&quot;Q&quot;,0.5,0.6]];html=1;" vertex="1" parent="1"><mxGeometry x="440" y="130" width="140" height="110" as="geometry"/></mxCell>
<mxCell id="open" style="shape=mxgraph.basic.polygon;polyCoords=[[0,1],[0.5,0],[1,1]];polyline=1;html=1;" vertex="1" parent="1"><mxGeometry x="600" y="130" width="140" height="110" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("M 20 30 L 200 30"), "{svg}");
    assert!(svg.contains("M 30 60 H 150 A 10 10 0 0 1 160 70"), "{svg}");
    // A chevron button comes to a point on the right.
    assert!(
        svg.contains("L 318.6 78.34 A 12.6 4 0 0 1 318.6 81.66"),
        "{svg}"
    );
    assert!(svg.contains("M 364 66 L 352 84 L 347.5 78"), "{svg}");
    assert!(svg.contains("M 390 60 L 410 80"), "{svg}");
    // A drop: straight sides from the point, meeting the circle at a tangent.
    assert!(
        svg.contains("M 55 130 L 85.955 188.667 A 35 35 0 0 1 90 205"),
        "{svg}"
    );
    // The triangle's apex sits a fifth of the way along the top.
    assert!(svg.contains("M 144 220 L 120 130 L 240 220 Z"), "{svg}");
    // The polygon's corners are fractions of its own box.
    assert!(
        svg.contains("M 270 185 L 319 130 L 410 146.5 L 382 240 L 291 229 Z"),
        "{svg}"
    );
    // A curved side names its control point in a second list, one entry per
    // side, so a straight side has to keep its place in that list.
    assert!(
        svg.contains("M 440 240 Q 454 152 510 130 L 580 240 Q 510 196 440 240 Z"),
        "{svg}"
    );
    // A polyline is left open, so it has no outline to fill.
    assert!(!svg.contains("id=\"drawio-open\" "), "{svg}");
    assert!(svg.contains("M 600 240 L 670 130 L 740 240"), "{svg}");
}

/// A diagram can be saved with nothing on it. draw.io stands its own "click
/// here to edit" placeholder on such a page, but that placeholder is not in the
/// model, so the page is written blank at the size the model declares and the
/// report says why rather than leaving a speck of a page behind.
#[test]
fn writes_an_empty_drawio_page_at_the_size_the_model_declares() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("empty.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Page-1"><mxGraphModel pageWidth="850" pageHeight="1100">
<root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert_eq!(report.page_count, 1);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert!(
        report.warnings[0].contains("no cells to draw"),
        "{:?}",
        report.warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // 850 x 1100 pixels is 637.5 x 825 points.
    assert!(svg.contains("width=\"637.5pt\" height=\"825pt\""), "{svg}");

    // Without a declared size the page falls back to US Letter rather than to
    // a page barely larger than nothing.
    let bare = temporary.path().join("bare.drawio");
    let bare_output = temporary.path().join("bare-out");
    drawio_file(
        &bare,
        r##"<mxfile><diagram name="Page-1"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram></mxfile>"##,
    );
    convert_path(&bare, &bare_output, &ConvertOptions::default()).unwrap();
    let svg = fs::read_to_string(bare_output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("width=\"637.5pt\" height=\"825pt\""), "{svg}");
}

/// ArchiMate 3 draws an element as a frame with a small badge in its corner
/// saying which kind of element it is. The frame comes from `archiType` and the
/// badge from `appType` or `techType`, and both take the element's own colours,
/// except for the lines across a badge, which are stroked and not filled.
#[test]
fn draws_archimate_elements_with_the_badge_that_names_their_kind() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("archi.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="ArchiMate"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="goal" style="shape=mxgraph.archimate3.application;appType=goal;archiType=oct;strokeColor=#FF0000;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="need" style="shape=mxgraph.archimate3.application;appType=requirement;archiType=oct;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="20" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="look" style="shape=mxgraph.archimate3.application;appType=assess;archiType=oct;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="20" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="node" style="shape=mxgraph.archimate3.application;appType=node;archiType=square;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="100" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="func" style="shape=mxgraph.archimate3.application;appType=func;archiType=rounded;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="100" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="both" style="shape=mxgraph.archimate3.application;appType=collab;archiType=square;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="100" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="when" style="shape=mxgraph.archimate3.application;appType=event;archiType=rounded;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="180" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="step" style="shape=mxgraph.archimate3.application;appType=proc;archiType=rounded;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="180" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="who" style="shape=mxgraph.archimate3.application;appType=actor;archiType=square;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="180" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="offer" style="shape=mxgraph.archimate3.service;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="260" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="runs" style="shape=mxgraph.archimate3.tech;techType=sysSw;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="260" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="mystery" style="shape=mxgraph.archimate3.application;appType=nosuchtype;archiType=square;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="260" width="140" height="60" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The frame takes one of three shapes, and the badge sits fifteen pixels
    // in from its top right corner whatever size the element is.
    assert!(
        svg.contains("M 20 30 L 30 20 L 150 20 L 160 30 L 160 70 L 150 80 L 30 80 L 20 70 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 190 100 H 310 A 10 10 0 0 1 320 110"),
        "{svg}"
    );
    assert!(svg.contains("M 340 100 H 480 V 160 H 340 Z"), "{svg}");
    // A goal is three rings, and only its centre takes the stroke colour.
    assert!(svg.contains("M 140 32.5 A 7.5 7.5 0 1 0 155 32.5"), "{svg}");
    assert!(
        svg.contains("M 144.5 32.5 A 3 3 0 1 0 150.5 32.5 A 3 3 0 1 0 144.5 32.5 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#FF0000\""),
        "{svg}"
    );
    // The badges that say which kind of element each one is.
    for (what, d) in [
        ("requirement", "M 303.75 25 L 315 25 L 311.25 40 L 300 40 Z"),
        ("assessment handle", "M 460 40 L 464.8 35.2"),
        (
            "node",
            "M 140 108.75 L 143.75 105 L 155 105 L 155 116.25 L 151.25 120 L 140 120 Z \
             M 140 108.75 L 151.25 108.75 L 151.25 120",
        ),
        (
            "function",
            "M 307.5 105 L 315 108 L 315 120 L 307.5 117 L 300 120 L 300 108 Z",
        ),
        ("collaboration", "M 466 112.5 A 4.5 4.5 0 1 0 475 112.5"),
        (
            "event",
            "M 150.5 188 A 4.5 4.5 0 0 1 150.5 197 L 140 197 L 144.5 192.5 L 140 188 Z",
        ),
        (
            "process",
            "M 300 189.5 L 309 189.5 L 309 185 L 315 192.5 L 309 200 L 309 195.5 L 300 195.5 Z",
        ),
        (
            "system software",
            "M 290 282.65 A 7.35 7.35 0 1 0 304.7 282.65",
        ),
    ] {
        assert!(svg.contains(d), "{what} badge missing from {svg}");
    }
    // A figure's arms and legs are stroked, not filled.
    assert!(
        svg.contains("M 460 200 L 467.5 196.25 L 475 200\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"none\""),
        "{svg}"
    );
    // A service is a rounded bar and carries no badge of its own.
    assert!(
        svg.contains("M 135 260 A 25 25 0 0 1 135 310 L 45 310 A 25 25 0 0 1 45 260 Z"),
        "{svg}"
    );
    assert!(!svg.contains("id=\"drawio-offer-part-2\""), "{svg}");
    // An element type that names no badge keeps its frame and an empty corner
    // rather than losing the element altogether.
    assert!(svg.contains("id=\"drawio-mystery-part-1\""), "{svg}");
    assert!(!svg.contains("id=\"drawio-mystery-part-2\""), "{svg}");
}

/// The rack units draw.io implements in code: a cabinet frame that snaps to a
/// whole number of rack units, a blanking plate shaded at each mounting ear, a
/// cable duct with a bay every thirty three pixels, a shelf bracket and a patch
/// panel in its own body colour.
#[test]
fn draws_the_rack_units_a_cabinet_is_filled_with() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("rack.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Rack"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="cabinet" style="shape=mxgraph.rackGeneral.rackCabinet3;fillColor=#F4F4F4;fillColor2=#FFFFFF;rackUnitSize=14.8;numDisp=descend;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="180" height="220" as="geometry"/></mxCell>
<mxCell id="plate" style="shape=mxgraph.rackGeneral.plate;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="20" width="160" height="20" as="geometry"/></mxCell>
<mxCell id="duct" style="shape=mxgraph.rackGeneral.horCableDuct;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="60" width="160" height="20" as="geometry"/></mxCell>
<mxCell id="shelf" style="shape=mxgraph.rackGeneral.shelf;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="100" width="160" height="24" as="geometry"/></mxCell>
<mxCell id="patch" style="shape=mxgraph.rackGeneral.neatPatch;bodyColor=#666666;html=1;" vertex="1" parent="1"><mxGeometry x="240" y="140" width="160" height="24" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // The numbering column is outside the frame, so the frame starts 24 in,
    // and its height snaps to twelve units of 14.8 plus its two 21-tall rails.
    assert!(svg.contains("M 44 20 H 200 V 239.6 H 44 Z"), "{svg}");
    assert!(svg.contains("M 44 20 H 200 V 41 H 44 Z"), "{svg}");
    assert!(svg.contains("M 44 41 H 53 V 218.6 H 44 Z"), "{svg}");
    // Four screws hold it in.
    assert_eq!(svg.matches("A 3 3 0 1 0").count(), 8, "{svg}");
    // The plate's mounting ears are shaded rather than a different fill.
    assert!(
        svg.contains("M 240 20 H 249 V 40 H 240 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#000000\" fill-opacity=\"0.23\""),
        "{svg}"
    );
    // A bay every thirty three pixels, each drawn as two stacked slots.
    assert!(svg.contains("M 254 60 H 257 V 67 H 254 Z"), "{svg}");
    assert!(svg.contains("M 287 60 H 290 V 67 H 287 Z"), "{svg}");
    assert_eq!(svg.matches("id=\"drawio-duct-detail-").count(), 10, "{svg}");
    // A shelf is the bracket that holds it, so it has no outline to fill.
    assert!(!svg.contains("id=\"drawio-shelf\" "), "{svg}");
    assert!(
        svg.contains("M 241 100 L 241 123 L 399 123 L 399 101"),
        "{svg}"
    );
    // The patch panel takes its body colour, not the style's fill.
    assert!(
        svg.contains("M 240 140 H 400 V 164 H 240 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"#666666\""),
        "{svg}"
    );
}

/// An entity-relationship diagram's own shapes, a C4 person and a browser
/// window. Each carries colours or an order of its own: a weak entity gets a
/// second frame, a person's head is drawn over the shoulders, and a browser's
/// close button and chrome are separate colours from the frame.
#[test]
fn draws_er_entities_a_c4_person_and_a_browser_window() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mixed.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Mixed"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="entity" value="Customer" style="shape=mxgraph.er.entity;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="weak" value="Order" style="shape=mxgraph.er.entity;buttonStyle=dblFrame;html=1;" vertex="1" parent="1"><mxGeometry x="180" y="20" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="has" value="places" style="shape=mxgraph.er.has;html=1;" vertex="1" parent="1"><mxGeometry x="340" y="10" width="140" height="70" as="geometry"/></mxCell>
<mxCell id="owns" value="owns" style="shape=mxgraph.er.has;buttonStyle=dblFrame;html=1;" vertex="1" parent="1"><mxGeometry x="500" y="10" width="140" height="70" as="geometry"/></mxCell>
<mxCell id="user" value="User" style="shape=mxgraph.c4.person;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="110" width="100" height="120" as="geometry"/></mxCell>
<mxCell id="page" style="shape=mxgraph.mockup.containers.browserWindow;strokeColor2=#008CFF;strokeColor3=#C4C4C4;html=1;" vertex="1" parent="1"><mxGeometry x="200" y="110" width="500" height="320" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // An entity is rounded by ten; a weak one is square with a frame inside.
    assert!(svg.contains("M 30 20 H 150 A 10 10 0 0 1 160 30"), "{svg}");
    assert!(svg.contains("M 180 20 H 320 V 70 H 180 Z"), "{svg}");
    assert!(svg.contains("M 185 25 H 315 V 65 H 185 Z"), "{svg}");
    // A relationship is a diamond, and a weak one gets a second one inside.
    assert!(
        svg.contains("M 340 45 L 410 10 L 480 45 L 410 80 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 514 45 L 570 17 L 626 45 L 570 73 Z"),
        "{svg}"
    );
    // The person's head is drawn twice, so the shoulders stop at it.
    assert_eq!(
        svg.matches("M 50 130 A 20 20 0 1 0 90 130 A 20 20 0 1 0 50 130 Z")
            .count(),
        2,
        "{svg}"
    );
    assert!(
        svg.contains("M 20 162 A 20 20 0 0 1 40 142 L 100 142"),
        "{svg}"
    );
    // The browser's close button takes its own colour, and the chrome another.
    assert!(
        svg.contains("M 675 125 A 10 10 0 1 0 695 125 A 10 10 0 1 0 675 125 Z\" fill-rule=\"nonzero\" transform=\"matrix(1 0 0 1 0 0)\" fill=\"none\" stroke=\"#008CFF\""),
        "{svg}"
    );
    assert!(
        svg.contains("M 200 150 L 230 150 L 230 125 A 5 5 0 0 1 235 120 L 370 120"),
        "{svg}"
    );
    // Back and forward point opposite ways, and the page icon is drawn twice.
    assert!(
        svg.contains("M 212 184 L 222 174 L 222 180 L 232 180"),
        "{svg}"
    );
    assert!(
        svg.contains("M 262 184 L 252 174 L 252 180 L 242 180"),
        "{svg}"
    );
    assert_eq!(
        svg.matches("L 252 131 L 252 145 L 237 145 Z").count(),
        1,
        "{svg}"
    );
}

/// The last of the shapes a real diagram reaches for: the arrows2 set, a data
/// store with its identifier band, the SysML accept-event and call-behaviour
/// actions, a UML state and BPMN's data object with its transfer and collection
/// marks.
#[test]
fn draws_the_arrows_states_and_data_objects_left_in_the_tail() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("tail.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Tail"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="both" style="shape=mxgraph.arrows2.twoWayArrow;dx=30;dy=0.5;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="160" height="60" as="geometry"/></mxCell>
<mxCell id="fancy" style="shape=mxgraph.arrows2.stylisedArrow;dx=30;dy=0.5;notch=20;feather=0.6;html=1;" vertex="1" parent="1"><mxGeometry x="200" y="20" width="160" height="60" as="geometry"/></mxCell>
<mxCell id="loop" style="shape=mxgraph.arrows2.jumpInArrow;dx=32;dy=20;arrowHead=40;html=1;" vertex="1" parent="1"><mxGeometry x="380" y="20" width="120" height="100" as="geometry"/></mxCell>
<mxCell id="store" style="shape=mxgraph.dfd.dataStoreID;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="110" width="160" height="40" as="geometry"/></mxCell>
<mxCell id="wait" style="shape=mxgraph.sysml.accEvent;html=1;" vertex="1" parent="1"><mxGeometry x="200" y="110" width="140" height="50" as="geometry"/></mxCell>
<mxCell id="call" style="shape=mxgraph.sysml.callBehAct;html=1;" vertex="1" parent="1"><mxGeometry x="20" y="180" width="160" height="70" as="geometry"/></mxCell>
<mxCell id="small" style="shape=mxgraph.sysml.callBehAct;html=1;" vertex="1" parent="1"><mxGeometry x="560" y="180" width="30" height="20" as="geometry"/></mxCell>
<mxCell id="state" style="shape=umlState;rounded=1;html=1;" vertex="1" parent="1"><mxGeometry x="200" y="180" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="entry" style="shape=umlState;rounded=1;umlStateConnection=connPointRefEntry;html=1;" vertex="1" parent="1"><mxGeometry x="380" y="280" width="140" height="60" as="geometry"/></mxCell>
<mxCell id="data" style="shape=mxgraph.bpmn.data;bpmnTransferType=input;isCollection=1;html=1;" vertex="1" parent="1"><mxGeometry x="380" y="150" width="80" height="100" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // A head at each end, and a shaft half the height thick.
    assert!(
        svg.contains(
            "M 50 35 L 150 35 L 150 20 L 180 50 L 150 80 L 150 65 L 50 65 L 50 80 L 20 50 L 50 20 Z"
        ),
        "{svg}"
    );
    // A feathered tail with a notch cut into its back.
    assert!(
        svg.contains("M 200 38 L 330 35 L 320 20 L 360 50 L 320 80 L 330 65 L 200 62 L 220 50 Z"),
        "{svg}"
    );
    // A head, then two arcs that carry the shaft back round to it.
    assert!(
        svg.contains(
            "M 468 20 L 500 40 L 468 60 L 468 60 A 88 60 0 0 0 380 120 A 88 100 0 0 1 468 20 Z"
        ),
        "{svg}"
    );
    // The data store is open on its right, and banded thirty in from the left.
    assert!(
        svg.contains("M 180 150 L 20 150 L 20 110 L 180 110"),
        "{svg}"
    );
    assert!(svg.contains("M 50 110 L 50 150"), "{svg}");
    // An accept-event action is notched by three tenths of its own height.
    assert!(
        svg.contains("M 200 110 L 340 110 L 340 160 L 200 160 L 215 135 Z"),
        "{svg}"
    );
    // The rake that marks a call to another behaviour, which a box too small
    // to hold it leaves off.
    assert!(
        svg.contains("M 150 240 L 150 230 L 170 230 L 170 240"),
        "{svg}"
    );
    assert!(!svg.contains("id=\"drawio-small-detail-0\""), "{svg}");
    // A state carrying a connection point reference is indented ten on the
    // left to leave room for it; a plain one is not.
    assert!(svg.contains("M 209 180 H 331 A 9 9 0 0 1 340 189"), "{svg}");
    assert!(svg.contains("M 399 280 H 511 A 9 9 0 0 1 520 289"), "{svg}");
    // A data object is a note, with an arrow saying which way the data goes
    // and three bars saying it stands for a collection.
    assert!(
        svg.contains("M 380 150 L 445 150 L 460 165 L 460 250 L 380 250 Z"),
        "{svg}"
    );
    assert!(
        svg.contains("M 383 156.6 L 390.7 156.6 L 390.7 153 L 397 159 L 390.7 165 L 390.7 161.4 L 383 161.4 Z"),
        "{svg}"
    );
    assert!(svg.contains("M 420 238 L 420 250"), "{svg}");
}

/// A stencil can defer to the cell it is drawn for: `fill`, `stroke` and
/// `inherit` name the cell's own colours rather than colours of their own, and
/// `none` paints nothing. Reading those as literal colours left every stencil
/// that uses them painted black, which is most of the network and rack sets.
#[test]
fn lets_a_stencil_defer_to_the_colours_of_the_cell_it_is_drawn_for() {
    let temporary = TempDir::new().unwrap();
    let stencils = temporary.path().join("stencils");
    fs::create_dir(&stencils).unwrap();
    fs::write(
        stencils.join("demo.xml"),
        r##"<shapes name="mxgraph.demo">
<shape name="Deferred" h="20" w="20" aspect="fixed" strokewidth="inherit">
  <connections/>
  <foreground>
    <fillcolor color="fill"/>
    <strokecolor color="stroke"/>
    <rect x="0" y="0" w="20" h="10"/>
    <fillstroke/>
    <fillcolor color="stroke"/>
    <rect x="0" y="10" w="20" h="5"/>
    <fill/>
    <fillcolor color="none"/>
    <rect x="0" y="15" w="20" h="5"/>
    <fill/>
  </foreground>
</shape>
</shapes>"##,
    )
    .unwrap();
    let input = temporary.path().join("deferred.drawio");
    let output = temporary.path().join("out");
    drawio_file(
        &input,
        r##"<mxfile><diagram name="Deferred"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
<mxCell id="tile" style="shape=mxgraph.demo.deferred;fillColor=#EDEDED;strokeColor=#884400;" vertex="1" parent="1"><mxGeometry x="20" y="20" width="40" height="40" as="geometry"/></mxCell>
</root></mxGraphModel></diagram></mxfile>"##,
    );
    let options = ConvertOptions {
        stencil_paths: vec![stencils],
        ..Default::default()
    };

    let report = convert_path(&input, &output, &options).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // `fill` and `stroke` take the cell's own colours, not black.
    assert!(svg.contains("fill=\"#EDEDED\""), "{svg}");
    assert!(svg.contains("stroke=\"#884400\""), "{svg}");
    // A part filled with `stroke` takes the stroke colour as its fill.
    assert!(svg.contains("fill=\"#884400\""), "{svg}");
    // Nothing anywhere is painted black by accident.
    assert!(!svg.contains("fill=\"#000000\""), "{svg}");
}

/// Every shape draw.io implements in code that the example diagrams reach for
/// now draws. This walks the ones added last, which between them cover the
/// gradients, the stroke widths and the second colours the painter had to grow
/// to support, and asserts that none of them falls back to a placeholder.
#[test]
fn draws_every_coded_shape_the_example_diagrams_reach_for() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("all.drawio");
    let output = temporary.path().join("out");
    let shapes = [
        ("chart", "mxgraph.mockup.graphics.columnChart", ""),
        (
            "player",
            "mxgraph.mockup.containers.videoPlayer",
            "barHeight=30;barPos=40;",
        ),
        ("channel", "mxgraph.eip.messageChannel", ""),
        ("dead", "mxgraph.eip.deadLetterChannel", ""),
        ("tile", "mxgraph.ios7ui.icon", ""),
        ("picture", "mxgraph.bootstrap.image", ""),
        ("window", "mxgraph.mockup.containers.window", ""),
        (
            "scroll",
            "mxgraph.mockup.navigation.scrollBar",
            "barPos=40;",
        ),
        ("spin", "mxgraph.mockup.forms.spinner", ""),
        (
            "checks",
            "mxgraph.mockup.forms.checkboxGroup",
            "mainText=One,+Two;",
        ),
        ("pin", "mxgraph.mockup.misc.pin", ""),
        (
            "stars",
            "mxgraph.mockup.misc.rating",
            "grade=3;ratingScale=5;",
        ),
        (
            "hearts",
            "mxgraph.bootstrap.rating",
            "ratingStyle=heart;grade=2;ratingScale=4;",
        ),
        ("striped", "mxgraph.bootstrap.leftButtonStriped", ""),
        ("bus", "mxgraph.networks.bus", ""),
        ("face", "smileyFace", "smileyType=happy;"),
        ("sad", "smileyFace", "smileyType=sad;"),
        ("flat", "smileyFace", "smileyType=neutral;"),
        ("person", "mxgraph.mockup.containers.userMale", ""),
        ("map", "mxgraph.ios.iBgMap", ""),
        ("stripes", "mxgraph.ios.iBgStriped", ""),
        ("marker", "mxgraph.ios.iPin", ""),
        ("media", "mxgraph.gmdl.player", ""),
        (
            "gate",
            "mxgraph.electrical.logic_gates.logic_gate",
            "operation=and;numInputs=2;",
        ),
        (
            "orgate",
            "mxgraph.electrical.logic_gates.logic_gate",
            "operation=or;numInputs=3;",
        ),
        (
            "xorgate",
            "mxgraph.electrical.logic_gates.logic_gate",
            "operation=xor;negating=1;",
        ),
        ("locbar", "mxgraph.ios.iLocBar", "barPos=60;"),
        ("status", "mxgraph.android.statusBar", ""),
        ("callout", "mxgraph.infographic.circularCallout2", "dy=15;"),
        ("sheet", "mxgraph.ios7ui.actionDialog", ""),
        ("column", "mxgraph.pid2misc.column", "columnType=tray;"),
        ("trayless", "mxgraph.pid2misc.column", "columnType=fixed;"),
        ("valve", "mxgraph.pid2valves.valve", "valveType=ball;"),
        ("params", "mxgraph.sysml.actParamNode", ""),
        ("item", "mxgraph.sysml.itemFlow", "flowDir=e;"),
        (
            "times",
            "mxgraph.lean_mapping.timeline",
            "mainText=20,A,50,B,30,C;",
        ),
        ("state", "mxgraph.sysml.compState", ""),
        (
            "speech",
            "mxgraph.mockup.text.callout",
            "linkText=Note;callStyle=roundRect;",
        ),
        ("composite", "ext", "rounded=1;"),
        ("grouped", "mxgraph.aws4.groupCenter", ""),
    ];
    let mut body = String::from(
        "<mxfile><diagram name=\"All\"><mxGraphModel><root><mxCell id=\"0\"/><mxCell id=\"1\" parent=\"0\"/>",
    );
    for (index, (id, shape, extra)) in shapes.iter().enumerate() {
        let column = (index % 8) as i32 * 200;
        let row = (index / 8) as i32 * 200;
        body.push_str(&format!(
            "<mxCell id=\"{id}\" value=\"x\" style=\"shape={shape};{extra}html=1;\" vertex=\"1\" parent=\"1\">\
             <mxGeometry x=\"{column}\" y=\"{row}\" width=\"160\" height=\"120\" as=\"geometry\"/></mxCell>"
        ));
    }
    body.push_str("</root></mxGraphModel></diagram></mxfile>");
    drawio_file(&input, &body);

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();

    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Every one of them drew something of its own, and none of them fell back
    // to the placeholder rectangle, which carries the shape name as its role.
    for (id, shape, _) in shapes {
        assert!(
            svg.contains(&format!("id=\"drawio-{id}\""))
                || svg.contains(&format!("id=\"drawio-{id}-")),
            "{shape} drew nothing"
        );
        assert!(
            !svg.contains(&format!("data-semantic-role=\"{shape}\"")),
            "{shape} fell back to a placeholder"
        );
    }
    // The gradients the painter grew to support reach the output as gradients.
    assert!(svg.contains("linearGradient"), "{svg}");
}

#[test]
fn converts_dxf_file_with_lines_circles_and_layers_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("drawing.dxf");
    let output = temporary.path().join("output");

    let dxf_data = r#"  0
SECTION
  2
TABLES
  0
TABLE
  2
LAYER
  0
LAYER
  2
Walls
 70
0
 62
1
  0
LAYER
  2
Columns
 70
0
 62
5
  0
LAYER
  2
HiddenLayer
 70
1
 62
2
  0
ENDTAB
  0
ENDSEC
  0
SECTION
  2
ENTITIES
  0
LINE
  8
Walls
 10
0.0
 20
0.0
 11
400.0
 21
300.0
  0
CIRCLE
  8
Columns
 10
200.0
 20
150.0
 40
50.0
  0
TEXT
  8
Walls
  1
Room 101
 10
50.0
 20
50.0
 40
12.0
  0
LINE
  8
HiddenLayer
 10
10.0
 20
10.0
 11
20.0
 21
20.0
  0
ENDSEC
  0
EOF
"#;
    fs::write(&input, dxf_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);
    assert_eq!(report.page_count, 1);

    let svg_path = output.join("page-0001.svg");
    assert!(svg_path.exists());
    let svg = fs::read_to_string(svg_path).unwrap();

    // Verify layer groups
    assert!(svg.contains("id=\"layer-Walls\""));
    assert!(svg.contains("id=\"layer-Columns\""));
    // Frozen layer should NOT be rendered
    assert!(!svg.contains("id=\"layer-HiddenLayer\""));

    // Verify text presence
    assert!(svg.contains("Room 101"));

    // Verify manifest
    let manifest = fs::read_to_string(output.join("conversion.json")).unwrap();
    assert!(manifest.contains("\"source_format\": \"dxf\""));
}

#[test]
fn converts_dxf_blocks_and_lwpolyline_with_bulge() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("blocks_and_polylines.dxf");
    let output = temporary.path().join("output");

    let dxf_data = r#"  0
SECTION
  2
BLOCKS
  0
BLOCK
  2
CHAIR
 10
0.0
 20
0.0
  0
LINE
  8
Furniture
 10
-10.0
 20
-10.0
 11
10.0
 21
-10.0
  0
ENDBLK
  0
ENDSEC
  0
SECTION
  2
ENTITIES
  0
LWPOLYLINE
  8
Outline
 70
1
 90
2
 10
0.0
 20
0.0
 42
1.0
 10
100.0
 20
0.0
  0
INSERT
  8
Furniture
  2
CHAIR
 10
50.0
 20
50.0
 41
2.0
 42
2.0
 50
45.0
  0
ENDSEC
  0
EOF
"#;
    fs::write(&input, dxf_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Arc path command from bulge
    assert!(svg.contains(" A "), "SVG should contain arc command: {svg}");
    // Layer for outline and furniture
    assert!(svg.contains("id=\"layer-Outline\""));
    assert!(svg.contains("id=\"layer-Furniture\""));
}

#[test]
fn converts_gerber_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("board.gbr");
    let output = temporary.path().join("output");

    let gerber_data = r#"%FSLAX24Y24*%
%MOMM*%
%ADD10C,0.8000*%
%ADD11R,1.5000X1.5000*%
D10*
X000000Y000000D02*
X020000Y020000D01*
D11*
X030000Y030000D03*
M02*
"#;
    fs::write(&input, gerber_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gerber);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"pcb-substrate\""));
    assert!(svg.contains("id=\"pcb-copper-layer\""));
}

#[test]
fn converts_hpgl_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("plot.plt");
    let output = temporary.path().join("output");

    let hpgl_data = "IN;SP1;PU0,0;PD500,0,500,500,0,500,0,0;PU;SP2;CI250;SP0;";
    fs::write(&input, hpgl_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Hpgl);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"pen-1\""));
    assert!(svg.contains("M "));
}

#[test]
fn converts_ansi_windows1252_dxf_file_with_degree_symbol() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("drawing_ansi.dxf");
    let output = temporary.path().join("output");

    // DXF file encoded in Windows-1252 with degree symbol (0xB0)
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        b"0\nSECTION\n2\nENTITIES\n0\nTEXT\n8\n0\n10\n0.0\n20\n0.0\n40\n10.0\n1\nANGLE 45",
    );
    bytes.push(0xB0); // degree sign in Windows-1252 / ISO-8859-1
    bytes.extend_from_slice(b"\n0\nENDSEC\n0\nEOF\n");

    fs::write(&input, bytes).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(
        svg.contains("ANGLE 45°"),
        "SVG should contain decoded degree symbol: {svg}"
    );
}

#[test]
fn converts_dxf_dimension_leader_and_negative_layer() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("dim_leader.dxf");
    let output = temporary.path().join("output");

    let dxf_data = r#"  0
SECTION
  2
TABLES
  0
TABLE
  2
LAYER
  0
LAYER
  2
VisibleLayer
 70
0
 62
3
  6
CONTINUOUS
  0
LAYER
  2
HiddenLayer
 70
0
 62
-1
  6
CONTINUOUS
  0
ENDTAB
  0
ENDSEC
  0
SECTION
  2
BLOCKS
  0
BLOCK
  8
VisibleLayer
  2
*D0
 70
1
 10
0.0
 20
0.0
  0
LINE
  8
VisibleLayer
 10
10.0
 20
10.0
 11
100.0
 21
10.0
  0
MTEXT
  8
VisibleLayer
 10
50.0
 20
15.0
 40
5.0
  1
90.00mm
  0
ENDBLK
  0
ENDSEC
  0
SECTION
  2
ENTITIES
  0
DIMENSION
  8
VisibleLayer
  2
*D0
 10
0.0
 20
0.0
 11
50.0
 21
15.0
  0
LEADER
  8
VisibleLayer
 76
3
 10
20.0
 20
20.0
 10
30.0
 20
35.0
 10
50.0
 20
35.0
  0
LINE
  8
HiddenLayer
 10
999.0
 20
999.0
 11
1999.0
 21
1999.0
  0
ENDSEC
  0
EOF
"#;
    fs::write(&input, dxf_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Dxf);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Block *D0 expanded from DIMENSION
    assert!(svg.contains("90.00mm"));
    // Leader line rendered
    assert!(svg.contains("id=\"layer-VisibleLayer\""));
    // HiddenLayer must not be rendered
    assert!(!svg.contains("id=\"layer-HiddenLayer\""));
}

#[test]
fn converts_gerber_arc_interpolation_and_plated_holes() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("circuit.gbr");
    let output = temporary.path().join("output");

    let gbr_data = r#"%FSLAX25Y25*%
%MOMM*%
%ADD10C,0.20000*%
%ADD12C,1.50000X0.70000*%
D10*
X0000000Y0000000D02*
G02*
X0050000Y0050000I0050000J0000000D01*
G01*
D12*
X0100000Y0100000D03*
%LPC*%
X0080000Y0080000D03*
%LPD*%
M02*
"#;
    fs::write(&input, gbr_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gerber);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    // Gerber arc uses SVG A command
    assert!(svg.contains(" A "));
    // Plated hole pad renders hole cutout in background substrate color #143d22
    assert!(svg.contains("#143d22"));
    // Copper color is present
    assert!(svg.contains("#e8be38"));
}

#[test]
fn converts_hpgl_labels_and_rectangles_through_cli() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("plot.plt");
    let output = temporary.path().join("output");

    let plt_data = "IN;SP1;PU100,100;PD1000,100;EA2000,1500;PU500,500;RA1200,800;LBENGINEERING DRAWING REV 2\x03;SP0;";
    fs::write(&input, plt_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Hpgl);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("ENGINEERING DRAWING REV 2"));
    assert!(svg.contains("pen-1"));
}

#[test]
fn converts_gcode_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("mill.nc");
    let output = temporary.path().join("output");

    let gcode_data = r#"G21 G90
G00 X10.0 Y10.0 Z5.0
M03 S1500
G01 Z-2.0 F250
G01 X50.0 Y10.0 F800
G02 X70.0 Y30.0 R20.0
G01 X70.0 Y60.0
M05
G00 Z10.0
M02
"#;
    fs::write(&input, gcode_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gcode);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"cam:workspace\""));
    assert!(svg.contains("data-semantic-role=\"gcode:rapid\""));
    assert!(svg.contains("data-semantic-role=\"gcode:cut\""));
}

#[test]
fn converts_excellon_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("board.drl");
    let output = temporary.path().join("output");

    let excellon_data = r#"M48
METRIC
T01C0.8
T02C1.2
%
T01
X10.0Y15.0
X30.0Y15.0
T02
X20.0Y35.0
M30
"#;
    fs::write(&input, excellon_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Excellon);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"pcb:substrate\""));
    assert!(svg.contains("data-semantic-role=\"pcb:drill-holes\""));
}

#[test]
fn converts_stl_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("bracket.stl");
    let output = temporary.path().join("output");

    let stl_data = r#"solid bracket
  facet normal 0 0 1
    outer loop
      vertex 0 0 5
      vertex 40 0 5
      vertex 40 30 5
    endloop
  endfacet
  facet normal 0 -1 0
    outer loop
      vertex 0 0 0
      vertex 40 0 0
      vertex 40 0 10
    endloop
  endfacet
endsolid bracket
"#;
    fs::write(&input, stl_data).unwrap();

    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Stl);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("STL Slicer"));
    assert!(svg.contains("data-semantic-role=\"stl:background\""));
}

#[test]
fn converts_simulation_msh_and_vtk_files_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let msh_input = temporary.path().join("fem.msh");
    let vtk_input = temporary.path().join("thermal.vtk");
    let msh_out = temporary.path().join("out_msh");
    let vtk_out = temporary.path().join("out_vtk");

    let msh_data = r#"$MeshFormat
2.2 0 8
$EndMeshFormat
$Nodes
3
1 0.0 0.0 0.0
2 100.0 0.0 0.0
3 50.0 80.0 0.0
$EndNodes
$Elements
1
1 2 2 0 1 1 2 3
$EndElements
"#;
    fs::write(&msh_input, msh_data).unwrap();

    let report = convert_path(&msh_input, &msh_out, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Simulation);
    let msh_svg = fs::read_to_string(msh_out.join("page-0001.svg")).unwrap();
    assert!(msh_svg.contains("data-semantic-role=\"simulation:mesh\""));

    let vtk_data = r#"# vtk DataFile Version 3.0
Thermal Simulation
ASCII
DATASET UNSTRUCTURED_GRID
POINTS 3 float
0.0 0.0 0.0
100.0 0.0 0.0
50.0 80.0 0.0
CELLS 1 4
3 0 1 2
POINT_DATA 3
SCALARS temperature float 1
LOOKUP_TABLE default
20.0 85.0 150.0
"#;
    fs::write(&vtk_input, vtk_data).unwrap();

    let vtk_report = convert_path(&vtk_input, &vtk_out, &ConvertOptions::default()).unwrap();
    assert_eq!(vtk_report.source_format, SourceFormat::Simulation);
    let vtk_svg = fs::read_to_string(vtk_out.join("page-0001.svg")).unwrap();
    assert!(vtk_svg.contains("colorbar-max"));
}

#[test]
fn converts_step_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let step_input = temporary.path().join("model.step");
    let out_dir = temporary.path().join("step_out");

    let step_data = r#"
ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('STEP test'),'2;1');
FILE_NAME('test.step','2026-09-10',('Engineer'),('Testing'),'','','');
FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));
ENDSEC;
DATA;
#1 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
#2 = CARTESIAN_POINT('', (100.0, 0.0, 0.0));
#3 = CARTESIAN_POINT('', (50.0, 100.0, 50.0));
#10 = VERTEX_POINT('', #1);
#11 = VERTEX_POINT('', #2);
#12 = VERTEX_POINT('', #3);
#20 = EDGE_CURVE('', #10, #11, #0, .T.);
#21 = EDGE_CURVE('', #11, #12, #0, .T.);
#22 = EDGE_CURVE('', #12, #10, #0, .T.);
ENDSEC;
END-ISO-10303-21;
"#;
    fs::write(&step_input, step_data).unwrap();

    let report = convert_path(&step_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Step);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"step:wireframe\""));
    assert!(svg.contains("STEP ISO 10303-21"));
}

#[test]
fn converts_obj_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let obj_input = temporary.path().join("model.obj");
    let out_dir = temporary.path().join("obj_out");

    let obj_data = r#"
# Cube OBJ
v 0 0 0
v 10 0 0
v 10 10 0
v 0 10 0
v 0 0 10
v 10 0 10
v 10 10 10
v 0 10 10

f 1 2 3 4
f 5 6 7 8
f 1 2 6 5
f 2 3 7 6
f 3 4 8 7
f 4 1 5 8
"#;
    fs::write(&obj_input, obj_data).unwrap();

    let report = convert_path(&obj_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Obj);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-semantic-role=\"obj:mesh\""));
    assert!(svg.contains("data-semantic-role=\"obj:background\""));
}

#[test]
fn converts_ply_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let ply_input = temporary.path().join("cube.ply");
    let out_dir = temporary.path().join("ply_out");

    let ply_data = r#"ply
format ascii 1.0
element vertex 8
property float x
property float y
property float z
element face 6
property list uchar int vertex_indices
end_header
0 0 0
10 0 0
10 10 0
0 10 0
0 0 10
10 0 10
10 10 10
0 10 10
4 0 1 2 3
4 7 6 5 4
4 0 4 5 1
4 1 5 6 2
4 2 6 7 3
4 3 7 4 0
"#;
    fs::write(&ply_input, ply_data).unwrap();

    let report = convert_path(&ply_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Ply);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("<path id=\"face_"));
}

#[test]
fn converts_gerber_layer_extension_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let gtl_input = temporary.path().join("board_top.gtl");
    let out_dir = temporary.path().join("gtl_out");

    let gerber_data =
        "%FSLAX24Y24*%\n%MOIN*%\n%ADD10C,0.050*%\nD10*\nX0000Y0000D03*\nX1000Y1000D03*\nM02*\n";
    fs::write(&gtl_input, gerber_data).unwrap();

    let report = convert_path(&gtl_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Gerber);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"pcb-copper-layer\""));
}

#[test]
fn converts_bmp_raster_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let bmp_input = temporary.path().join("test.bmp");
    let out_dir = temporary.path().join("bmp_out");

    // 2x2 24-bit uncompressed BMP: 1 black pixel, 3 white pixels
    let mut bmp_bytes = Vec::new();
    // File header (14 bytes)
    bmp_bytes.extend_from_slice(b"BM");
    let file_size: u32 = 14 + 40 + 8; // 2 rows of 2 pixels (3 bytes each + 2 bytes padding = 4 bytes per row)
    bmp_bytes.extend_from_slice(&file_size.to_le_bytes());
    bmp_bytes.extend_from_slice(&[0, 0, 0, 0]); // reserved
    let offset: u32 = 54;
    bmp_bytes.extend_from_slice(&offset.to_le_bytes());
    // DIB header (40 bytes)
    let dib_size: u32 = 40;
    bmp_bytes.extend_from_slice(&dib_size.to_le_bytes());
    let width: i32 = 2;
    let height: i32 = 2;
    bmp_bytes.extend_from_slice(&width.to_le_bytes());
    bmp_bytes.extend_from_slice(&height.to_le_bytes());
    let planes: u16 = 1;
    let bpp: u16 = 24;
    bmp_bytes.extend_from_slice(&planes.to_le_bytes());
    bmp_bytes.extend_from_slice(&bpp.to_le_bytes());
    let comp: u32 = 0; // BI_RGB
    bmp_bytes.extend_from_slice(&comp.to_le_bytes());
    let img_size: u32 = 8;
    bmp_bytes.extend_from_slice(&img_size.to_le_bytes());
    bmp_bytes.extend_from_slice(&[0; 16]); // ppm & colors

    // Pixel data (bottom-up):
    // Row 0 (bottom): (0,0,0) black, (255,255,255) white, 2 padding bytes
    bmp_bytes.extend_from_slice(&[0, 0, 0, 255, 255, 255, 0, 0]);
    // Row 1 (top): (255,255,255) white, (255,255,255) white, 2 padding bytes
    bmp_bytes.extend_from_slice(&[255, 255, 255, 255, 255, 255, 0, 0]);

    fs::write(&bmp_input, &bmp_bytes).unwrap();

    let report = convert_path(&bmp_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Raster);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"vectorized_path\""));
}

#[test]
fn converts_iges_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let iges_input = temporary.path().join("model.iges");
    let out_dir = temporary.path().join("iges_out");

    let iges_content = format!(
        "{:72}S{:07}\n{:72}G{:07}\n{:72}D{:07}\n{:72}D{:07}\n{:72}P{:07}\n{:72}T{:07}\n",
        "Sample IGES Start Section",
        1,
        "1H,,1H;,4HSTEP,4HFILE,16,38,6,38,15,4HSTEP,1.,1,4HINCH,1,0.01;",
        1,
        "     110       1       0       0       0       0       0       000000000",
        1,
        "     110       0       0       1       0                               0",
        2,
        "110,0.,0.,0.,10.,20.,30.;",
        1,
        "S      1G      1D      2P      1",
        1
    );
    fs::write(&iges_input, iges_content).unwrap();

    let report = convert_path(&iges_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::Iges);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"iges_0\""));
}

#[test]
fn converts_threemf_file_through_public_api() {
    let temporary = TempDir::new().unwrap();
    let threemf_input = temporary.path().join("model.3mf");
    let out_dir = temporary.path().join("threemf_out");

    let file = File::create(&threemf_input).unwrap();
    let mut zip = ZipWriter::new(file);
    zip.start_file("3D/3dmodel.model", SimpleFileOptions::default())
        .unwrap();
    let model_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">
  <resources>
    <object id="1" type="model">
      <mesh>
        <vertices>
          <vertex x="0" y="0" z="0" />
          <vertex x="10" y="0" z="0" />
          <vertex x="0" y="10" z="0" />
        </vertices>
        <triangles>
          <triangle v1="0" v2="1" v3="2" />
        </triangles>
      </mesh>
    </object>
  </resources>
</model>"#;
    zip.write_all(model_xml.as_bytes()).unwrap();
    zip.finish().unwrap();

    let report = convert_path(&threemf_input, &out_dir, &ConvertOptions::default()).unwrap();
    assert_eq!(report.source_format, SourceFormat::ThreeMf);
    assert_eq!(report.page_count, 1);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg")).unwrap();
    assert!(svg.contains("id=\"face_0\""));
}

#[test]
fn covers_the_full_domain_for_a_radially_symmetric_function_shading() {
    // Real-world regression: a function shading computing distance from
    // its domain's center (sampled at y=0 and y=1, both equidistant from
    // the center at y=0.5) always shows zero change along y at the two
    // points the adaptive tessellator's root cell samples -- not by
    // floating-point coincidence, but because both really do evaluate to
    // the same value for every x. Once that made the tessellator commit
    // to only ever splitting x, x's own change kept "winning" forever,
    // starving y of ever being resampled at new points and leaving the
    // whole shading but a sliver near x=0 completely blank. Also exercises
    // a real PDF's non-standard placement of /Matrix directly on the
    // shading dictionary (not just on a pattern wrapping it) together with
    // /BBox, which a real producer's page-layout PDF used to lay out a
    // grid of shadings and which a previous bug in the BBox clip's own
    // transform separately hid entirely.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("symmetric-function-shading.pdf");
    let mut document = Document::with_version("1.7");
    let function = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into()],
        },
        b"{ 0.5 sub exch 0.5 sub dup mul exch dup mul add sqrt }".to_vec(),
    ));
    let shading = document.add_object(dictionary! {
        "ShadingType" => 1, "ColorSpace" => "DeviceGray",
        "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
        "Matrix" => vec![170.into(), 0.into(), 0.into(), 170.into(), 30.into(), 30.into()],
        "BBox" => vec![30.into(), 30.into(), 200.into(), 200.into()],
        "Function" => function,
    });
    let content = document.add_object(Stream::new(dictionary! {}, b"/SH1 sh\n".to_vec()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 250.into(), 250.into()],
        "Resources" => dictionary! { "Shading" => dictionary! { "SH1" => shading } },
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
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(report.pages[0].warnings.is_empty(), "{:?}", report.pages[0].warnings);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    let xs: Vec<f64> = svg
        .split("<path ")
        .skip(1)
        .filter(|tag| tag.contains("data-content-kind=\"function-shading-cell\""))
        .filter_map(|tag| {
            // Search for " d=\"" (with the leading space): "id=\"" also
            // contains the bare substring "d=\"", one character in.
            let d_start = tag.find(" d=\"")? + 4;
            let d_end = tag[d_start..].find('"')? + d_start;
            tag[d_start..d_end]
                .split_ascii_whitespace()
                .nth(1)?
                .parse::<f64>()
                .ok()
        })
        .collect();
    assert!(xs.len() > 100, "too few cells: {}", xs.len());
    let min_x = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_x = xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    // A starved tessellation collapses to a sliver near the domain's left
    // edge (x=30); a healthy one spans close to the full 30..200 BBox.
    assert!(
        max_x - min_x > 100.0,
        "cells span only {:.3} of the ~170-wide domain (min={min_x:.3}, max={max_x:.3}); \
         the shading was starved to one edge instead of covering it",
        max_x - min_x
    );
}

#[test]
fn degrades_gracefully_instead_of_failing_a_page_with_a_high_frequency_function_shading() {
    // Real-world regression (pdf.js's own function_based_shading.pdf test
    // file, SH8): a sin(1440 * distance-from-center) ripple oscillates
    // roughly 229 times across its unit domain, so no cell wider than
    // about 1/458th of that domain can ever land within tessellation
    // tolerance -- exhausting the adaptive tessellator's cell budget is
    // expected here, not just on a pathological input. That used to be a
    // hard error that failed the whole page; it must instead degrade to a
    // warning and a lower-resolution rendering of just that shading.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("high-frequency-function-shading.pdf");
    let mut document = Document::with_version("1.7");
    let function = document.add_object(Stream::new(
        dictionary! {
            "FunctionType" => 4,
            "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
            "Range" => vec![0.into(), 1.into()],
        },
        b"{ dup mul exch dup mul add sqrt 1440 mul sin 1 add 2 div }".to_vec(),
    ));
    let shading = document.add_object(dictionary! {
        "ShadingType" => 1, "ColorSpace" => "DeviceGray",
        "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
        "Matrix" => vec![170.into(), 0.into(), 0.into(), 170.into(), 30.into(), 30.into()],
        "Function" => function,
    });
    let content = document.add_object(Stream::new(dictionary! {}, b"/SH1 sh\n".to_vec()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 250.into(), 250.into()],
        "Resources" => dictionary! { "Shading" => dictionary! { "SH1" => shading } },
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
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert_eq!(report.page_count, 1);
    assert!(
        report.pages[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("lower-resolution approximation")),
        "{:?}",
        report.pages[0].warnings
    );
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("data-content-kind=\"function-shading-cell\""), "{svg}");
}

#[test]
fn draws_content_after_a_comment_trailing_an_operator_on_the_same_line() {
    // Real-world regression (pdf.js's own calgray.pdf/calrgb.pdf test
    // files): the exact bytes of that file's content stream, byte for
    // byte. Its `%` comments, used purely for column alignment, made the
    // content stream parser drop every single operation with no error and
    // no warning, turning an ordinary page into a silently blank one --
    // but only with this precise formatting: a smaller, hand-written
    // reproduction of "a comment trails an operator" was not enough to
    // reproduce it, so this test uses the real file's own bytes rather
    // than risk exercising a different, merely similar-looking case.
    let temporary = TempDir::new().unwrap();
    let input = temporary.path().join("trailing-comment.pdf");
    let mut document = Document::with_version("1.7");
    let color_space = document.add_object(vec![
        Object::Name(b"CalGray".to_vec()),
        dictionary! {
            "WhitePoint" => vec![Object::Integer(1), Object::Integer(1), Object::Integer(1)],
            "Gamma" => Object::Integer(1),
        }
        .into(),
    ]);
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"\n /Cs5 cs               %\n  0.00 sc              %\n   25   25  200  200 re%\n f                     %\n"
            .to_vec(),
    ));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 250.into(), 250.into()],
        "Resources" => dictionary! { "ColorSpace" => dictionary! { "Cs5" => color_space } },
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
    let output = temporary.path().join("out");
    let report = convert_path(&input, &output, &ConvertOptions::default()).unwrap();
    assert!(report.pages[0].warnings.is_empty(), "{:?}", report.pages[0].warnings);
    assert_eq!(report.pages[0].node_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg")).unwrap();
    assert!(svg.contains("#000000"), "{svg}");
}
