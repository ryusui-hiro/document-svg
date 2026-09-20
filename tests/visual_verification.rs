use base64::Engine as _;
use lopdf::{Document, Object, Stream, dictionary};
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::tempdir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use document_svg::{ConvertOptions, SourceFormat, convert_path};

#[path = "common/mod.rs"]
mod common;

fn render_svg_with_resvg(
    svg_bytes: &[u8],
) -> Result<resvg::tiny_skia::Pixmap, Box<dyn std::error::Error>> {
    let mut opt = resvg::usvg::Options {
        font_family: "sans-serif".into(),
        ..Default::default()
    };
    let mut fontdb = resvg::usvg::fontdb::Database::new();
    fontdb.load_system_fonts();
    let fallback_family = [
        "DejaVu Sans",
        "Liberation Sans",
        "Arial",
        "Helvetica",
        "Noto Sans",
    ]
    .into_iter()
    .find(|candidate| {
        fontdb.faces().any(|face| {
            face.families
                .iter()
                .any(|(family, _)| family.eq_ignore_ascii_case(candidate))
        })
    })
    .map(str::to_owned)
    .or_else(|| {
        fontdb
            .faces()
            .next()
            .and_then(|face| face.families.first())
            .map(|(family, _)| family.clone())
    })
    .ok_or("visual verification requires at least one installed font")?;
    fontdb.set_serif_family(fallback_family.clone());
    fontdb.set_sans_serif_family(fallback_family.clone());
    fontdb.set_cursive_family(fallback_family.clone());
    fontdb.set_fantasy_family(fallback_family.clone());
    fontdb.set_monospace_family(fallback_family.clone());
    opt.font_family = fallback_family;
    *opt.fontdb_mut() = fontdb;
    let tree = resvg::usvg::Tree::from_data(svg_bytes, &opt)?;
    let size = tree.size().to_int_size();
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(size.width(), size.height()).ok_or_else(|| {
            format!(
                "failed to allocate pixmap of size {}x{}",
                size.width(),
                size.height()
            )
        })?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::default(),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

#[test]
fn test_visual_rendering_pdf_text_annotations_without_appearances()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("free-text.pdf");
    let output = temp.path().join("pdf-free-text-out");
    let mut document = Document::with_version("1.7");
    let annotation = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "FreeText",
        "Rect" => vec![10.into(), 20.into(), 190.into(), 70.into()],
        "Contents" => Object::string_literal("PDF annotation text"),
        "DA" => Object::string_literal("/Helv 14 Tf 0 0.7 0 rg"),
    });
    let field = document.add_object(dictionary! {
        "FT" => "Tx", "T" => Object::string_literal("review"),
        "V" => Object::string_literal("Widget fallback text"),
    });
    let widget = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![10.into(), 72.into(), 190.into(), 96.into()],
        "Parent" => Object::Reference(field),
    });
    let checkbox_field = document.add_object(dictionary! {
        "FT" => "Btn", "T" => Object::string_literal("accepted"),
        "V" => Object::Name(b"Yes".to_vec()),
    });
    let checkbox = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![10.into(), 5.into(), 25.into(), 20.into()],
        "Parent" => Object::Reference(checkbox_field),
    });
    let radio_field = document.add_object(dictionary! {
        "FT" => "Btn", "T" => Object::string_literal("choice"),
        "Ff" => 1 << 15, "V" => Object::Name(b"OptionA".to_vec()),
    });
    let select_field = document.add_object(dictionary! {
        "FT" => "Ch", "T" => Object::string_literal("category"),
        "V" => Object::string_literal("code-1"),
        "Opt" => Object::Array(vec![Object::Array(vec![
            Object::string_literal("code-1"),
            Object::string_literal("Choice visible"),
        ])]),
    });
    let radio = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![35.into(), 5.into(), 50.into(), 20.into()],
        "Parent" => Object::Reference(radio_field), "AS" => Object::Name(b"OptionA".to_vec()),
    });
    let choice_widget = document.add_object(dictionary! {
        "Type" => "Annot", "Subtype" => "Widget",
        "Rect" => vec![55.into(), 5.into(), 190.into(), 20.into()],
        "Parent" => Object::Reference(select_field),
    });
    let content = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
        "Resources" => dictionary! {}, "Contents" => content,
        "Annots" => vec![
            Object::Reference(annotation),
            Object::Reference(widget),
            Object::Reference(checkbox),
            Object::Reference(radio),
            Object::Reference(choice_widget),
        ],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
        ),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog", "Pages" => pages,
        "AcroForm" => dictionary! {
            "Fields" => vec![
                Object::Reference(field),
                Object::Reference(checkbox_field),
                Object::Reference(radio_field),
                Object::Reference(select_field),
            ],
            "DA" => Object::string_literal("/Helv 10 Tf 0 0 1 rg"),
        },
    });
    document.trailer.set("Root", catalog);
    document.save(&input)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("PDF annotation text"));
    assert!(svg_text.contains("data-content-kind=\"annotation-freetext\""));
    assert!(svg_text.contains("Widget fallback text"));
    assert!(svg_text.contains("data-content-kind=\"annotation-widget-text\""));
    assert!(svg_text.contains("Choice visible"));
    assert!(svg_text.contains("data-content-kind=\"annotation-widget-choice\""));
    assert!(svg_text.contains("Choice visible"));
    assert!(svg_text.contains("data-content-kind=\"annotation-widget-choice\""));
    assert_eq!(
        svg_text
            .matches("data-content-kind=\"annotation-widget-button\"")
            .count(),
        2
    );
    let pixmap = render_svg_with_resvg(&svg)?;
    let green_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && pixel[1] > pixel[0].saturating_add(30)
                && pixel[1] > pixel[2].saturating_add(30)
        })
        .count();
    assert!(
        green_pixels > 10,
        "only {green_pixels} FreeText pixels rendered"
    );
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && pixel[0] < 100
                && pixel[1] < 100
                && pixel[2] > pixel[1].saturating_add(50)
        })
        .count();
    assert!(
        blue_pixels > 10,
        "only {blue_pixels} text Widget pixels rendered"
    );
    let dark_widget_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 120 && pixel[1] < 120 && pixel[2] < 120)
        .count();
    assert!(
        dark_widget_pixels > 10,
        "only {dark_widget_pixels} checkbox/radio marker pixels rendered"
    );
    Ok(())
}

fn assert_colored_mesh_fills_page_width(pixmap: &resvg::tiny_skia::Pixmap) {
    let width = pixmap.width() as usize;
    let content_limit = width * 4 / 5;
    let mut min_x = width;
    let mut max_x = 0usize;
    for (index, pixel) in pixmap.data().chunks_exact(4).enumerate() {
        let x = index % width;
        if x >= content_limit || pixel[3] == 0 {
            continue;
        }
        if pixel[0] > 35 || pixel[1] > 35 || pixel[2] > 35 {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
        }
    }
    assert!(
        max_x.saturating_sub(min_x) > width * 2 / 5,
        "colored mesh should use the page width; visible extent was {} of {width} pixels",
        max_x.saturating_sub(min_x)
    );
}

fn binary_legacy_vtk_sample() -> Vec<u8> {
    let mut bytes = b"# vtk DataFile Version 3.0\nBinary scalar mesh\nBINARY\nDATASET UNSTRUCTURED_GRID\nPOINTS 4 float\n".to_vec();
    for value in [
        0.0f32, 0.0, 0.0, 100.0, 0.0, 0.0, 100.0, 100.0, 0.0, 0.0, 100.0, 0.0,
    ] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes.extend_from_slice(b"\nCELLS 2 8\n");
    for value in [3i32, 0, 1, 2, 3, 0, 2, 3] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes.extend_from_slice(b"\nCELL_TYPES 2\n");
    for value in [5i32, 5] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes.extend_from_slice(b"\nCELL_DATA 2\nSCALARS stress float\nLOOKUP_TABLE default\n");
    for value in [2.0f32, 8.0] {
        bytes.extend_from_slice(&value.to_be_bytes());
    }
    bytes
}

fn raw_appended_vtu_sample() -> Vec<u8> {
    let mut appended = Vec::new();
    let mut append_array = |data: &[u8]| {
        let offset = appended.len();
        appended.extend_from_slice(&(data.len() as u32).to_le_bytes());
        appended.extend_from_slice(data);
        offset
    };
    let mut points = Vec::new();
    for value in [
        0.0f32, 0.0, 0.0, 100.0, 0.0, 0.0, 100.0, 100.0, 0.0, 0.0, 100.0, 0.0,
    ] {
        points.extend_from_slice(&value.to_le_bytes());
    }
    let point_offset = append_array(&points);
    let mut connectivity = Vec::new();
    for value in [0i32, 1, 2, 0, 2, 3] {
        connectivity.extend_from_slice(&value.to_le_bytes());
    }
    let connectivity_offset = append_array(&connectivity);
    let mut offsets = Vec::new();
    for value in [3i32, 6] {
        offsets.extend_from_slice(&value.to_le_bytes());
    }
    let offsets_offset = append_array(&offsets);
    let types_offset = append_array(&[5u8, 5]);

    let prefix = format!(
        "<VTKFile type=\"UnstructuredGrid\" version=\"1.0\" byte_order=\"LittleEndian\"><UnstructuredGrid><Piece NumberOfPoints=\"4\" NumberOfCells=\"2\"><Points><DataArray type=\"Float32\" NumberOfComponents=\"3\" format=\"appended\" offset=\"{point_offset}\"/></Points><Cells><DataArray type=\"Int32\" Name=\"connectivity\" format=\"appended\" offset=\"{connectivity_offset}\"/><DataArray type=\"Int32\" Name=\"offsets\" format=\"appended\" offset=\"{offsets_offset}\"/><DataArray type=\"UInt8\" Name=\"types\" format=\"appended\" offset=\"{types_offset}\"/></Cells></Piece></UnstructuredGrid><AppendedData encoding=\"raw\">_"
    );
    let mut bytes = prefix.into_bytes();
    bytes.extend_from_slice(&appended);
    bytes.extend_from_slice(&[0, 0xff]);
    bytes.extend_from_slice(b"</AppendedData>is binary data, not the XML boundary");
    bytes.extend_from_slice(b"</AppendedData></VTKFile>");
    bytes
}

fn write_xps_visual_fixture(path: &Path) -> std::io::Result<()> {
    let mut archive = ZipWriter::new(fs::File::create(path)?);
    let options = SimpleFileOptions::default();
    let parts = [
        (
            "_rels/.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/xps/2005/06/fixedrepresentation" Target="/FixedDocumentSequence.fdseq"/></Relationships>"#,
        ),
        (
            "FixedDocumentSequence.fdseq",
            r#"<FixedDocumentSequence xmlns="http://schemas.microsoft.com/xps/2005/06"><DocumentReference Source="Documents/1/FixedDocument.fdoc"/></FixedDocumentSequence>"#,
        ),
        (
            "Documents/1/FixedDocument.fdoc",
            r#"<FixedDocument xmlns="http://schemas.microsoft.com/xps/2005/06"><PageContent Source="Pages/1.fpage" Width="200" Height="120"/></FixedDocument>"#,
        ),
        (
            "Documents/1/Pages/1.fpage",
            r##"<FixedPage xmlns="http://schemas.microsoft.com/xps/2005/06" Width="200" Height="120"><Path Fill="#FFFF0000" Data="F0 M 10,10 L 190,10 190,50 10,50 Z"/><Canvas Opacity="0.5" RenderTransform="0.75,0,0,0.75,5,0"><Path><Path.Fill><SolidColorBrush Color="#FF00FF00"/></Path.Fill><Path.Data><PathGeometry Figures="F1 M 20,55 L 180,55 180,100 20,100 Z"/></Path.Data></Path></Canvas><Path><Path.Fill><ImageBrush ImageSource="/Resources/Images/image1.png"/></Path.Fill><Path.Data><PathGeometry Figures="F1 M 30,60 L 170,60 170,100 30,100 Z"/></Path.Data></Path><Glyphs FontUri="/Resources/Fonts/missing.odttf" OriginX="20" OriginY="115" UnicodeString="XPS &amp; OXPS" FontRenderingEmSize="12" Fill="#FF0000FF"/></FixedPage>"##,
        ),
    ];
    for (name, contents) in parts {
        archive.start_file(name, options)?;
        archive.write_all(contents.as_bytes())?;
    }
    let mut image_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut image_bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&[0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 0])?;
    }
    archive.start_file("Resources/Images/image1.png", options)?;
    archive.write_all(&image_bytes)?;
    archive.finish()?;
    Ok(())
}

fn write_cbz_visual_fixture(path: &Path) -> std::io::Result<()> {
    let mut archive = ZipWriter::new(fs::File::create(path)?);
    let options = SimpleFileOptions::default();
    for (name, color) in [("page10.png", [0, 40, 245]), ("page2.png", [245, 24, 16])] {
        let mut pixels = Vec::with_capacity(20 * 10 * 3);
        for _ in 0..20 * 10 {
            pixels.extend_from_slice(&color);
        }
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, 20, 10);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
        }
        archive.start_file(name, options)?;
        archive.write_all(&png_bytes)?;
    }
    archive.finish()?;
    Ok(())
}

#[test]
fn test_visual_rendering_html_named_entities() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let html_path = temp.path().join("entities.html");
    let out_dir = temp.path().join("entities_out");

    let html_content = r#"<!DOCTYPE html>
<html>
<body>
  <h1>DocSVG Entities &copy; 2026</h1>
  <p>Price: 100 &euro; &plusmn; 5 &yen; &mdash; High quality &ndash; Fast &hellip;</p>
  <p>Non-breaking&nbsp;Space &middot; Bullet &bull; Registered &reg; Trade &trade;</p>
</body>
</html>"#;
    fs::write(&html_path, html_content)?;

    let report = convert_path(&html_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Verify entities were decoded into actual glyphs
    assert!(svg_str.contains("©"), "missing ©");
    assert!(svg_str.contains("€"), "missing €");
    assert!(svg_str.contains("±"), "missing ±");
    assert!(svg_str.contains("—"), "missing —");
    assert!(svg_str.contains("…"), "missing …");
    assert!(svg_str.contains("®"), "missing ®");
    assert!(svg_str.contains("™"), "missing ™");
    assert!(!svg_str.contains("&copy;"), "raw &copy; found");

    // Verify it renders cleanly in resvg
    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    // Ensure non-blank pixels
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_rtf_windows_1252_and_unicode() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("report.rtf");
    let output = temp.path().join("rtf_out");
    let source = br#"{\rtf1\ansi\ansicpg1252\uc1
{\fonttbl{\f0 Arial;}}
{\info{\title RTF visual check}}
Overview\par
R\'e9sum\'e9 \emdash  Unicode \u-10179?\u-8701?\par
{\ansicpg932\uc1 \'82\'a0}\par
{\pict\pngblip 89504e470d0a}\page Image fallback omitted.
}"#;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Rtf);
    assert_eq!(report.page_count, 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid RTF PNG/JPEG picture data"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Overview"));
    assert!(svg_text.contains("Résumé — Unicode 😃"), "{svg_text}");
    assert!(svg_text.contains("あ"), "{svg_text}");
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().chunks_exact(4).any(|pixel| pixel[3] > 0));
    let second_svg = fs::read(output.join("page-0002.svg"))?;
    assert!(std::str::from_utf8(&second_svg)?.contains("Image fallback omitted."));
    let second_pixmap = render_svg_with_resvg(&second_svg)?;
    assert!(
        second_pixmap
            .data()
            .chunks_exact(4)
            .any(|pixel| pixel[3] > 0)
    );
    Ok(())
}

#[test]
fn test_visual_rendering_multipage_code_block() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("long_code.md");
    let out_dir = temp.path().join("long_code_out");

    let mut content = String::from("# Long Source Code Listing\n\n```rust\n");
    for i in 1..=120 {
        content.push_str(&format!(
            "    let variable_{i:03} = compute_value({i}); // line {i}\n"
        ));
    }
    content.push_str("```\n\nEnd of code.\n");
    fs::write(&md_path, content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert!(
        report.page_count >= 2,
        "expected at least 2 pages for 120 lines of code, got {}",
        report.page_count
    );

    // Verify every page renders cleanly through resvg
    for i in 1..=report.page_count {
        let svg_file = out_dir.join(format!("page-{i:04}.svg"));
        assert!(svg_file.exists());
        let bytes = fs::read(&svg_file)?;
        let pixmap = render_svg_with_resvg(&bytes)?;
        assert!(pixmap.width() > 0);
    }

    Ok(())
}

#[test]
fn test_visual_rendering_table_overflow_clipping() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("overflow_table.md");
    let out_dir = temp.path().join("table_out");

    let md_content = r#"# Table With Long Text

| Short | Extremely Long Description Column That Would Overflow Adjacent Columns If Unclipped |
| :--- | :--- |
| Item 1 | This is a very lengthy explanation containing detailed descriptions of internal processes and subsystems. |
| Item 2 | Another continuous stream of text designed to test boundary containment within the table cell. |
"#;
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Verify ellipsis was inserted for overly long header or cell text
    assert!(
        svg_str.contains('…'),
        "expected ellipsis in truncated cell text"
    );

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0);

    Ok(())
}

#[test]
fn test_visual_rendering_markdown_inline_cleaning() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("clean_inline.md");
    let out_dir = temp.path().join("clean_inline_out");

    let md_content = r#"# Title with **Bold** and `Code`

This is a paragraph with [hyperlink text](https://example.com) and **strong emphasis**.

- Bullet with `inline_code()` and ~~strikethrough~~
"#;
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Verify inline formatting delimiters were cleaned
    assert!(svg_str.contains("Title with Bold and Code"));
    assert!(svg_str.contains("hyperlink text and strong emphasis"));
    assert!(svg_str.contains("Bullet with inline_code() and strikethrough"));
    assert!(!svg_str.contains("**Bold**"));
    assert!(!svg_str.contains("[hyperlink text]("));

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0);

    Ok(())
}

#[test]
fn test_visual_rendering_sample_sources_batch() -> Result<(), Box<dyn std::error::Error>> {
    let samples = [
        "samples/source/sample.dot",
        "samples/source/sample.mmd",
        "samples/source/sample.msh",
        "samples/source/sample.vtk",
        "samples/source/sample.chart",
        "samples/source/sample.tex",
        "samples/source/sample.qr",
        "samples/source/sample.stl",
        "samples/source/sample.obj",
        "samples/source/sample.dxf",
        "samples/source/sample.gbr",
        "samples/source/sample.drl",
        "samples/source/sample.plt",
        "samples/source/sample.nc",
        "samples/source/sample.png",
        "samples/source/sample.step",
        "samples/source/sample.drawio",
        "samples/source/sample-architecture.dot",
        "samples/source/sample-sequence.mmd",
        "samples/source/sample-math.tex",
        "samples/source/sample-qr.qr",
        "samples/source/sample-table.md",
        "samples/source/sample-metrics.chart.json",
        "samples/source/sample.adoc",
        "samples/source/sample.html",
        "samples/source/sample.d2",
        "samples/source/sample.puml",
        "samples/source/sample.csv",
        "samples/source/sample.ply",
    ];

    for sample in samples {
        let sample_path = Path::new(sample);
        if !sample_path.exists() {
            continue;
        }

        let temp = tempdir()?;
        let out_dir = temp.path().join("out");
        let report = convert_path(sample_path, &out_dir, &ConvertOptions::default())?;
        assert!(report.page_count >= 1, "failed page count for {}", sample);

        for page in 1..=report.page_count {
            let svg_path = out_dir.join(format!("page-{page:04}.svg"));
            assert!(svg_path.exists(), "missing svg page for {}", sample);
            let svg_bytes = fs::read(&svg_path)?;
            let pixmap = render_svg_with_resvg(&svg_bytes)
                .map_err(|e| format!("Failed to render {} page {}: {}", sample, page, e))?;
            assert!(pixmap.width() > 0 && pixmap.height() > 0);
            assert!(pixmap.data().iter().any(|&b| b > 0));
        }
    }

    Ok(())
}

#[test]
fn test_visual_rendering_asciidoc_complete() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let adoc_path = temp.path().join("spec.adoc");
    let out_dir = temp.path().join("adoc_out");

    let adoc_content = r#"= Enterprise Architecture Specification

== Overview

This document specifies the core services and interfaces of the enterprise cloud platform.

* Scalable microservice mesh
* Zero-trust security model with mTLS
* Distributed vector storage

=== Service Endpoints

. Authorization Service: `/api/v2/auth`
. Document Ingestion: `/api/v2/documents`
. Vector Processing: `/api/v2/vectorize`

[source,rust]
----
pub struct ServiceConfig {
    pub port: u16,
    pub timeout_ms: u64,
}
----

'''
Approved by Architecture Review Board.
"#;
    fs::write(&adoc_path, adoc_content)?;

    let report = convert_path(&adoc_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_restructuredtext_local_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rst");
    let output = temp.path().join("rst-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Rst);
    let mut red_pixels = 0usize;
    let mut blue_pixels = 0usize;
    for page in 1..=report.page_count {
        let bytes = fs::read(output.join(format!("page-{page:04}.svg")))?;
        let pixmap = render_svg_with_resvg(&bytes)?;
        red_pixels += pixmap
            .data()
            .chunks_exact(4)
            .filter(|rgba| rgba[0] > 200 && rgba[1] < 100 && rgba[2] < 100)
            .count();
        blue_pixels += pixmap
            .data()
            .chunks_exact(4)
            .filter(|rgba| rgba[2] > 150 && rgba[0] < 100 && rgba[1] < 150)
            .count();
    }
    assert!(red_pixels > 0, "local RST PNG red channel was not rendered");
    assert!(
        blue_pixels > 0,
        "local RST PNG blue channel was not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_org_local_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.org");
    let output = temp.path().join("org-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Org);
    let mut red_pixels = 0usize;
    let mut blue_pixels = 0usize;
    for page in 1..=report.page_count {
        let bytes = fs::read(output.join(format!("page-{page:04}.svg")))?;
        let pixmap = render_svg_with_resvg(&bytes)?;
        red_pixels += pixmap
            .data()
            .chunks_exact(4)
            .filter(|rgba| rgba[0] > 200 && rgba[1] < 100 && rgba[2] < 100)
            .count();
        blue_pixels += pixmap
            .data()
            .chunks_exact(4)
            .filter(|rgba| rgba[2] > 150 && rgba[0] < 100 && rgba[1] < 150)
            .count();
    }
    assert!(red_pixels > 0, "local Org PNG red channel was not rendered");
    assert!(
        blue_pixels > 0,
        "local Org PNG blue channel was not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_complex_cjk_document() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("cjk_report.md");
    let out_dir = temp.path().join("cjk_out");

    let md_content = r#"# 統合ドキュメント変換システム仕様書

## 概要

DocSVGはRustで記述された高性能かつセキュアなドキュメント変換エンジンです。あらゆる入力を単一の高品質なベクターSVGへと変換し、さらにCAD、プロッター、3Dメッシュ、およびWebフロントエンドコードへと可逆または決定論的に逆変換できます。

### 特徴一覧

- **多形式対応**: PDF、Word、Excel、PowerPoint、CAD、ダイアグラム、数式、チャート
- **日本語禁則処理**: 句読点（。や、など）が行頭に来ないタイポグラフィ制御
- **自動改ページ**: 長大なコードブロックやテーブルもページ境界で自動分割

```rust
fn main() {
    println!("DocSVG: 高度な組版エンジンが稼働中");
}
```

| 項目 | ステータス | 備考 |
| :--- | :---: | ---: |
| タイポグラフィ | 合格 | 禁則処理および文字単位折り返し |
| テーブル組版 | 合格 | 自動列幅計算およびゼブラ配色 |
| 3Dメッシュ | 合格 | 等角投影および2.5D厚み押し出し |

---
策定完了。
"#;
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert!(report.page_count >= 1);

    for page in 1..=report.page_count {
        let svg_bytes = fs::read(out_dir.join(format!("page-{page:04}.svg")))?;
        let pixmap = render_svg_with_resvg(&svg_bytes)?;
        assert!(pixmap.width() > 0 && pixmap.height() > 0);
        assert!(pixmap.data().iter().any(|&b| b > 0));
    }

    Ok(())
}

#[test]
fn test_visual_rendering_reverse_formats() -> Result<(), Box<dyn std::error::Error>> {
    use document_svg::{ReverseOptions, svg_to_document};

    let temp = tempdir()?;
    let svg_path = temp.path().join("test.svg");
    let svg_content = r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200" viewBox="0 0 200 200">
  <rect x="10" y="10" width="80" height="80" fill="#2563eb" stroke="#1e40af" stroke-width="2"/>
  <circle cx="150" cy="150" r="30" fill="#10b981"/>
  <text x="20" y="180" font-family="sans-serif" font-size="14" fill="#0f172a">Vector Preview</text>
</svg>"##;
    fs::write(&svg_path, svg_content)?;

    let reverse_cases = [
        ("out.dxf", "SECTION"),
        ("out.stl", "solid"),
        ("out.obj", "v "),
        ("out.ply", "ply"),
        ("out.nc", "G21"),
        ("out.gbr", "G04"),
        ("out.drl", "M48"),
        ("out.plt", "IN;"),
        ("out.jsx", "export default"),
        ("out.tsx", "export default"),
        ("out.vue", "<template>"),
        ("out.datauri", "data:image/svg+xml;base64,"),
    ];

    for (filename, expected_substring) in reverse_cases {
        let out_file = temp.path().join(filename);
        let rev_report = svg_to_document(&svg_path, &out_file, &ReverseOptions::default())?;
        assert_eq!(rev_report.page_count, 1, "failed reverse for {}", filename);
        assert!(out_file.exists(), "file was not created: {}", filename);
        let content = fs::read_to_string(&out_file)?;
        assert!(
            content.contains(expected_substring),
            "output file {} did not contain '{}'",
            filename,
            expected_substring
        );
    }

    Ok(())
}

#[test]
fn test_visual_rendering_asciidoc_local_block_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("image-appendix.adoc");
    let assets = temp.path().join("assets");
    fs::create_dir_all(&assets)?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/asciidoc_image.adoc"),
        &input,
    )?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        assets.join("red-blue.png"),
    )?;
    let output = temp.path().join("asciidoc-image-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "AsciiDoc local image red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "AsciiDoc local image blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_asciidoc_tables_and_admonitions() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let adoc_path = temp.path().join("spec.adoc");
    let out_dir = temp.path().join("adoc_out");

    let adoc_content = r#"= Architecture Specification
Author: Systems Team
v1.0, 2026-09-12

== Overview

NOTE: This specification is automatically verified against production schemas.

TIP: Use deterministic serialization options for reproducible builds.

WARNING: Modifying buffer sizes without measuring memory limits may fail on 32-bit hosts.

== Feature Comparison Table

|===
| Module | Maturity | Target Pipeline | Coverage

| Document Engine | Stable | HTML / AsciiDoc / EPUB | 99.4%
| CAD Slicer | Beta | STL / DXF / STEP | 95.8%
| Vectorizer | Stable | Potrace VTracer | 98.2%
|===

== Implementation Notes

The table above is rendered via high-fidelity vector cells with background styling.
"#;
    fs::write(&adoc_path, adoc_content)?;

    let report = convert_path(&adoc_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Verify table elements and admonitions exist
    assert!(
        svg_str.contains("Document Engine"),
        "missing table cell content"
    );
    assert!(
        svg_str.contains("[NOTE]"),
        "missing [NOTE] admonition prefix"
    );
    assert!(
        svg_str.contains("[WARNING]"),
        "missing [WARNING] admonition prefix"
    );

    // Render with resvg
    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_d2_bidirectional_and_containers() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let d2_path = temp.path().join("system.d2");
    let out_dir = temp.path().join("d2_out");

    let d2_content = r#"// Microservice Topology
# Primary frontend and API layer
frontend: "Web Client" {
  shape: circle
}

gateway: "API Gateway"
db: "PostgreSQL Cluster" {
  shape: cylinder
}

// Edge declarations
frontend -> gateway: "HTTPS / 443"
gateway <-> db: "Connection Pool"
cache <- gateway: "Cache lookup"
"#;
    fs::write(&d2_path, d2_content)?;

    let report = convert_path(&d2_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    assert!(svg_str.contains("Web Client"));
    assert!(svg_str.contains("PostgreSQL Cluster"));
    assert!(
        !svg_str.contains(">// Microservice Topology<"),
        "comment leaked into visual SVG text"
    );

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_plantuml_reverse_sequence_and_shapes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    // 1. Sequence diagram with reverse arrow <-- and autonumber
    let seq_path = temp.path().join("auth.puml");
    let seq_out = temp.path().join("auth_out");
    let seq_content = r#"@startuml
autonumber
actor User
participant "Auth Service" as Auth
database "User DB" as DB

User -> Auth : POST /login
Auth -> DB : SELECT credentials
DB <-- Auth : return user record
User <-- Auth : 200 OK (JWT)
@enduml"#;
    fs::write(&seq_path, seq_content)?;

    let report_seq = convert_path(&seq_path, &seq_out, &ConvertOptions::default())?;
    assert_eq!(report_seq.page_count, 1);

    let svg_bytes = fs::read(seq_out.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;
    assert!(svg_str.contains("POST /login"));
    assert!(svg_str.contains("200 OK (JWT)"));

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    // 2. Class diagram with UML inheritance and interfaces
    let class_path = temp.path().join("domain.puml");
    let class_out = temp.path().join("domain_out");
    let class_content = r#"@startuml
interface Repository
database Storage
class PostgresRepo

Storage <|-- PostgresRepo
Repository <|.. PostgresRepo
PostgresRepo ..> Storage : query
@enduml"#;
    fs::write(&class_path, class_content)?;

    let report_class = convert_path(&class_path, &class_out, &ConvertOptions::default())?;
    assert_eq!(report_class.page_count, 1);

    let svg_bytes_class = fs::read(class_out.join("page-0001.svg"))?;
    let pixmap_class = render_svg_with_resvg(&svg_bytes_class)?;
    assert!(pixmap_class.width() > 0 && pixmap_class.height() > 0);
    assert!(pixmap_class.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_cad_and_chart_nan_resilience() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    // 1. Chart JSON with edge values (zero, large values, empty categories)
    let chart_path = temp.path().join("resilient.chart.json");
    let chart_out = temp.path().join("chart_out");
    let chart_json = r#"{
        "type": "bar",
        "title": "Edge Case Metrics",
        "labels": ["Alpha", "Beta", "Gamma"],
        "series": [
            {"name": "Zero Series", "data": [0.0, 0.0, 0.0]},
            {"name": "Normal Series", "data": [10.5, 42.0, 15.2]}
        ]
    }"#;
    fs::write(&chart_path, chart_json)?;

    let report_chart = convert_path(&chart_path, &chart_out, &ConvertOptions::default())?;
    assert_eq!(report_chart.page_count, 1);
    let svg_bytes = fs::read(chart_out.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Ensure no NaN coordinates exist
    assert!(
        !svg_str.contains("NaN"),
        "chart SVG contains NaN coordinate"
    );
    assert!(
        !svg_str.contains("inf"),
        "chart SVG contains inf coordinate"
    );

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    // 2. Line Chart with zero sum and single data points
    let line_path = temp.path().join("single_line.chart.json");
    let line_out = temp.path().join("line_out");
    let line_json = r#"{
        "type": "line",
        "title": "Single Point",
        "labels": ["Single"],
        "series": [
            {"name": "Metric", "data": [100.0]}
        ]
    }"#;
    fs::write(&line_path, line_json)?;
    let report_line = convert_path(&line_path, &line_out, &ConvertOptions::default())?;
    assert_eq!(report_line.page_count, 1);
    let line_svg = fs::read(line_out.join("page-0001.svg"))?;
    let line_str = std::str::from_utf8(&line_svg)?;
    assert!(!line_str.contains("NaN"));
    let pixmap_line = render_svg_with_resvg(&line_svg)?;
    assert!(pixmap_line.width() > 0 && pixmap_line.height() > 0);

    Ok(())
}

#[test]
fn test_visual_rendering_mermaid_subgraphs_and_shapes() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let mmd_path = temp.path().join("shapes.mmd");
    let mmd_out = temp.path().join("mmd_out");

    let mmd_content = r#"flowchart TD
subgraph ClusterA
    A((Start Circle)) --> B([Stadium Node])
end
subgraph ClusterB
    B -.-> C{{Hexagon Node}}
    C ==> D{Decision}
    D -- yes --> E[Done]
    D <--> A
end"#;
    fs::write(&mmd_path, mmd_content)?;

    let report = convert_path(&mmd_path, &mmd_out, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(mmd_out.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Visual resvg rasterization check
    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    // Verify shapes and labels are rendered in SVG
    assert!(svg_str.contains("Start Circle"));
    assert!(svg_str.contains("Stadium Node"));
    assert!(svg_str.contains("Hexagon Node"));
    // Verify subgraph keyword was stripped from nodes
    assert!(!svg_str.contains(">subgraph ClusterA<"));

    Ok(())
}

#[test]
fn test_visual_rendering_markdown_task_lists_and_table_alignments()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("tasks_and_table.md");
    let md_out = temp.path().join("md_out");

    let md_content = r#"# Project Status

- [x] Implement TrueType Post table glyph lookup
- [x] Sanitize NaN and Inf coordinates in CAD renderers
- [ ] Next milestone release

| Task | Status | Completion |
|:-----|:------:|-----------:|
| Parsing | Done | 100% |
| Validation | Active | 85% |
| Delivery | Pending | 0% |
"#;
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &md_out, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(md_out.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Check task checkboxes
    assert!(svg_str.contains("☑"));
    assert!(svg_str.contains("☐"));

    // Check table alignments
    assert!(
        svg_str.contains("text-anchor=\"start\"") || svg_str.contains("text-anchor=\"middle\"")
    );
    assert!(svg_str.contains("text-anchor=\"end\""));

    // Verify resvg rasterization
    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_latex_math_extensions() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let math_path = temp.path().join("formula.tex");
    let math_out = temp.path().join("math_out");

    let math_content = r#"\text{Energy} = \frac{m c^2}{\sqrt{1 - \frac{v^2}{c^2}}} + \alpha \Delta \Omega \sin(x) + \{ x \}"#;
    fs::write(&math_path, math_content)?;

    let report = convert_path(&math_path, &math_out, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(math_out.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Check that Greek letters, fractions, and text are present
    assert!(svg_str.contains("Energy"));
    assert!(svg_str.contains("α"));
    assert!(svg_str.contains("Δ"));
    assert!(svg_str.contains("Ω"));
    assert!(svg_str.contains("sin"));
    assert!(svg_str.contains("{"));
    assert!(svg_str.contains("}"));

    // Verify resvg rasterization
    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_cad_gcode_g92_and_step_comments() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;

    // 1. G-code with G92 reset
    let gcode_path = temp.path().join("reset.nc");
    let gcode_out = temp.path().join("gcode_out");
    let gcode_content = r#"G21 (Metric)
G90 (Absolute)
G0 X10.0 Y10.0
G92 X0.0 Y0.0 (Logical reset)
G1 X20.0 Y20.0 F500
G2 X40.0 Y20.0 R10.0
M30
"#;
    fs::write(&gcode_path, gcode_content)?;

    let report_gcode = convert_path(&gcode_path, &gcode_out, &ConvertOptions::default())?;
    assert_eq!(report_gcode.page_count, 1);
    let gcode_svg = fs::read(gcode_out.join("page-0001.svg"))?;
    let gcode_pixmap = render_svg_with_resvg(&gcode_svg)?;
    assert!(gcode_pixmap.width() > 0 && gcode_pixmap.height() > 0);
    assert!(gcode_pixmap.data().iter().any(|&b| b > 0));

    // 2. STEP with multiline comments and .5 coordinates
    let step_path = temp.path().join("commented.step");
    let step_out = temp.path().join("step_out");
    let step_content = r#"ISO-10303-21;
HEADER;
/* Multi-line
   comment in header */
FILE_DESCRIPTION(('STEP AP214'), '1');
ENDSEC;
/* Comment before data */
DATA;
#1 = CARTESIAN_POINT('', (.0, .0, .0));
#2 = CARTESIAN_POINT('', (10.5, .5, 5.0));
#3 = VERTEX_POINT('', #1);
#4 = VERTEX_POINT('', #2);
#5 = EDGE_CURVE('', #3, #4, #1, .T.);
ENDSEC;
END-ISO-10303-21;
"#;
    fs::write(&step_path, step_content)?;

    let report_step = convert_path(&step_path, &step_out, &ConvertOptions::default())?;
    assert_eq!(report_step.page_count, 1);
    let step_svg = fs::read(step_out.join("page-0001.svg"))?;
    let step_pixmap = render_svg_with_resvg(&step_svg)?;
    assert!(step_pixmap.width() > 0 && step_pixmap.height() > 0);
    assert!(step_pixmap.data().iter().any(|&b| b > 0));

    // 3. Gerber with trailing zero suppression
    let gerber_path = temp.path().join("trailing.gbr");
    let gerber_out = temp.path().join("gerber_out");
    let gerber_content = r#"%FSLAX24Y24*%
%MOMM*%
%ADD10C,.5*%
%LPD*%
D10*
X100000Y100000D02*
X200000Y200000D01*
M02*
"#;
    fs::write(&gerber_path, gerber_content)?;

    let report_gerber = convert_path(&gerber_path, &gerber_out, &ConvertOptions::default())?;
    assert_eq!(report_gerber.page_count, 1);
    let gerber_svg = fs::read(gerber_out.join("page-0001.svg"))?;
    let gerber_pixmap = render_svg_with_resvg(&gerber_svg)?;
    assert!(gerber_pixmap.width() > 0 && gerber_pixmap.height() > 0);
    assert!(gerber_pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_vtk_xml_unstructured_grid() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("stress.vtu");
    let output = temp.path().join("vtu_out");
    let source = r#"<?xml version="1.0"?>
<VTKFile type="UnstructuredGrid" version="1.0" byte_order="LittleEndian">
  <UnstructuredGrid><Piece NumberOfPoints="4" NumberOfCells="2">
    <PointData Scalars="temperature"><DataArray type="Float32" Name="temperature" format="ascii">10 20 30 40</DataArray></PointData>
    <CellData Scalars="stress"><DataArray type="Float32" Name="stress" format="ascii">2 8</DataArray></CellData>
    <Points><DataArray type="Float32" NumberOfComponents="3" format="ascii">0 0 0  100 0 0  100 100 0  0 100 0</DataArray></Points>
    <Cells>
      <DataArray type="Int32" Name="connectivity" format="ascii">0 1 2  0 2 3</DataArray>
      <DataArray type="Int32" Name="offsets" format="ascii">3 6</DataArray>
      <DataArray type="UInt8" Name="types" format="ascii">5 5</DataArray>
    </Cells>
  </Piece></UnstructuredGrid>
</VTKFile>"#;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("color map uses cell values"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg_text.contains("colorbar-max"));

    let rendered = render_svg_with_resvg(&svg)?;
    assert_colored_mesh_fills_page_width(&rendered);
    let distinct_colors = rendered
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct_colors.len() > 24,
        "VTK scalar field should render its mesh and colorbar"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_vtk_xml_raw_appended_binary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("raw-appended.vtu");
    let output = temp.path().join("vtu_out");
    fs::write(&input, raw_appended_vtu_sample())?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    let rendered = render_svg_with_resvg(&svg)?;
    assert_colored_mesh_fills_page_width(&rendered);
    Ok(())
}

#[test]
fn test_visual_rendering_vtk_xml_image_data() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("temperature.vti");
    let output = temp.path().join("vti_out");
    let source = r#"<?xml version="1.0"?>
<VTKFile type="ImageData" version="1.0" byte_order="LittleEndian">
  <ImageData WholeExtent="0 2 0 2 0 0" Origin="-10 25 0" Spacing="5 8 1">
    <Piece Extent="0 2 0 2 0 0">
      <CellData Scalars="temperature"><DataArray type="Float32" Name="temperature" format="ascii">0 1 2 3</DataArray></CellData>
    </Piece>
  </ImageData>
</VTKFile>"#;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg_text.contains("colorbar-max"));

    let rendered = render_svg_with_resvg(&svg)?;
    assert_colored_mesh_fills_page_width(&rendered);
    let distinct_colors = rendered
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct_colors.len() > 24,
        "VTK ImageData scalar field should render its grid and colorbar"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_xps_fixed_page_path_and_glyphs() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = temp.path().join("invoice.xps");
    let output = temp.path().join("xps_out");
    write_xps_visual_fixture(&input)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("embedded fonts"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data-semantic-role=\"xps:path\""));
    assert!(svg_text.contains("data-semantic-role=\"xps:glyphs\""));
    assert!(svg_text.contains("XPS &amp; OXPS"));
    assert!(svg_text.contains("matrix(0.75 0 0 0.75 5 0)"));
    assert!(svg_text.contains("opacity=\"0.5\""));
    assert!(svg_text.contains("data:image/png;base64,"));
    assert!(svg_text.contains("clip-path=\"url(#xps-image-clip-"));

    let rendered = render_svg_with_resvg(&svg)?;
    let mut red_pixels = 0usize;
    let mut green_pixels = 0usize;
    for pixel in rendered.data().chunks_exact(4) {
        if pixel[3] == 0 {
            continue;
        }
        if pixel[0] > pixel[1].saturating_add(80) && pixel[0] > pixel[2].saturating_add(80) {
            red_pixels += 1;
        }
        if pixel[1] > pixel[0].saturating_add(20) && pixel[1] > pixel[2].saturating_add(20) {
            green_pixels += 1;
        }
    }
    assert!(red_pixels > 1_000, "XPS filled Path did not render visibly");
    assert!(
        green_pixels > 100,
        "XPS transformed, translucent Path did not render visibly"
    );
    Ok(())
}

#[test]
fn dwfx_alias_renders_visible_fixed_page() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dwfx");
    let output = temp.path().join("dwfx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xps);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn op2_preflight_renders_visible_table_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.op2");
    let output = temp.path().join("op2-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Op2);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn ipc2581_renders_visible_pcb_exchange_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ipc2581");
    let output = temp.path().join("ipc2581-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ipc2581);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn jt_header_renders_visible_version_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jt");
    let output = temp.path().join("jt-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Jt);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn test_visual_rendering_multiframe_tiff_preserves_rgb_pixels()
-> Result<(), Box<dyn std::error::Error>> {
    use tiff::encoder::{TiffEncoder, colortype};

    let temp = tempdir()?;
    let input = temp.path().join("color-pages.tif");
    {
        let mut pixels = Vec::new();
        for _y in 0..10 {
            for x in 0..20 {
                if x < 10 {
                    pixels.extend_from_slice(&[240, 16, 24]);
                } else {
                    pixels.extend_from_slice(&[12, 220, 32]);
                }
            }
        }
        let mut encoder = TiffEncoder::new(fs::File::create(&input)?).unwrap();
        encoder.write_image::<colortype::RGB8>(20, 10, &pixels)?;
        encoder.write_image::<colortype::Gray8>(3, 2, &[0, 80, 255, 255, 80, 0])?;
    }

    let output = temp.path().join("tiff_visual_out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 2);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let mut red_pixels = 0usize;
    let mut green_pixels = 0usize;
    for pixel in rendered.data().chunks_exact(4) {
        if pixel[3] == 0 {
            continue;
        }
        if pixel[0] > pixel[1].saturating_add(80) && pixel[0] > pixel[2].saturating_add(80) {
            red_pixels += 1;
        }
        if pixel[1] > pixel[0].saturating_add(80) && pixel[1] > pixel[2].saturating_add(80) {
            green_pixels += 1;
        }
    }
    assert!(
        red_pixels > 20,
        "TIFF red pixels did not survive PNG embedding"
    );
    assert!(
        green_pixels > 20,
        "TIFF green pixels did not survive PNG embedding"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_dicom_rgb_pixels_and_keeps_metadata_out_of_svg()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_rgb.dcm");
    let output = temp.path().join("dicom-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Dicom);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(!svg_text.contains("Private^Image"));
    let rendered = render_svg_with_resvg(&svg)?;
    let mut red_pixels = 0usize;
    let mut blue_pixels = 0usize;
    for pixel in rendered.data().chunks_exact(4) {
        if pixel[3] == 0 {
            continue;
        }
        if pixel[0] > pixel[1].saturating_add(80) && pixel[0] > pixel[2].saturating_add(80) {
            red_pixels += 1;
        }
        if pixel[2] > pixel[0].saturating_add(80) && pixel[2] > pixel[1].saturating_add(80) {
            blue_pixels += 1;
        }
    }
    assert!(
        red_pixels > 50,
        "only {red_pixels} red DICOM pixels rendered"
    );
    assert!(
        blue_pixels > 50,
        "only {blue_pixels} blue DICOM pixels rendered"
    );

    let ct_input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_ct_16bit.dcm");
    let ct_output = temp.path().join("dicom-ct-visual");
    let ct_report = convert_path(&ct_input, &ct_output, &ConvertOptions::default())?;
    assert_eq!(ct_report.page_count, 1);
    let ct_svg = fs::read(ct_output.join("page-0001.svg"))?;
    assert!(!std::str::from_utf8(&ct_svg)?.contains("Private^CT"));
    let ct = render_svg_with_resvg(&ct_svg)?;
    let grayscale_values = ct
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] == pixel[1] && pixel[1] == pixel[2])
        .map(|pixel| pixel[0])
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        grayscale_values.len() >= 4,
        "only {} distinct CT grayscale levels rendered",
        grayscale_values.len()
    );

    let jpeg2000_input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_jpeg2000.dcm");
    let jpeg2000_output = temp.path().join("dicom-jpeg2000-visual");
    let jpeg2000_report = convert_path(
        &jpeg2000_input,
        &jpeg2000_output,
        &ConvertOptions::default(),
    )?;
    assert_eq!(jpeg2000_report.source_format, SourceFormat::Dicom);
    assert_eq!(jpeg2000_report.page_count, 1);
    let jpeg2000_svg = fs::read(jpeg2000_output.join("page-0001.svg"))?;
    let jpeg2000 = render_svg_with_resvg(&jpeg2000_svg)?;
    let jpeg2000_gray = jpeg2000
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] == pixel[1] && pixel[1] == pixel[2])
        .map(|pixel| pixel[0])
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        jpeg2000_gray.len() >= 8,
        "only {} distinct JPEG 2000 grayscale levels rendered",
        jpeg2000_gray.len()
    );

    let dicomdir_input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dicom_set/DICOMDIR");
    let dicomdir_output = temp.path().join("dicomdir-visual");
    let dicomdir_report = convert_path(
        &dicomdir_input,
        &dicomdir_output,
        &ConvertOptions::default(),
    )?;
    assert_eq!(dicomdir_report.source_format, SourceFormat::DicomDir);
    assert_eq!(dicomdir_report.page_count, 3);
    let dicomdir_page = fs::read(dicomdir_output.join("page-0003.svg"))?;
    let dicomdir_svg = std::str::from_utf8(&dicomdir_page)?;
    assert!(!dicomdir_svg.contains("Private^DirectoryPatient"));
    assert!(
        !dicomdir_report.pages[2]
            .warnings
            .iter()
            .any(|warning| warning.contains("2 sequential SVG pages"))
    );
    let dicomdir_rendered = render_svg_with_resvg(&dicomdir_page)?;
    let mut directory_red = 0usize;
    let mut directory_blue = 0usize;
    for pixel in dicomdir_rendered.data().chunks_exact(4) {
        if pixel[3] == 0 {
            continue;
        }
        if pixel[0] > pixel[1].saturating_add(80) && pixel[0] > pixel[2].saturating_add(80) {
            directory_red += 1;
        }
        if pixel[2] > pixel[0].saturating_add(80) && pixel[2] > pixel[1].saturating_add(80) {
            directory_blue += 1;
        }
    }
    assert!(directory_red > 50 && directory_blue > 50);
    Ok(())
}

#[test]
fn test_visual_rendering_dicom_encapsulated_pdf_report_text()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_encapsulated_pdf.dcm");
    let output = temp.path().join("dicom-encapsulated-pdf-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Dicom);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Synthetic radiology report"));
    assert!(!svg_text.contains("Synthetic^Patient"));
    let rendered = render_svg_with_resvg(&svg)?;
    let dark_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 96 && pixel[1] < 96 && pixel[2] < 96)
        .count();
    assert!(
        dark_pixels > 100,
        "only {dark_pixels} dark report pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_multimolecule_sdf_bonds_and_element_labels()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_molecules.sdf");
    let output = temp.path().join("sdf-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sdf);
    assert_eq!(report.page_count, 2);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let mut red_pixels = 0usize;
    let mut blue_pixels = 0usize;
    let mut dark_pixels = 0usize;
    for pixel in rendered.data().chunks_exact(4) {
        if pixel[3] == 0 {
            continue;
        }
        if pixel[0] > pixel[1].saturating_add(60) && pixel[0] > pixel[2].saturating_add(60) {
            red_pixels += 1;
        }
        if pixel[2] > pixel[0].saturating_add(60) && pixel[2] > pixel[1].saturating_add(60) {
            blue_pixels += 1;
        }
        if pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80 {
            dark_pixels += 1;
        }
    }
    assert!(
        red_pixels > 20,
        "oxygen label produced only {red_pixels} red pixels"
    );
    assert!(
        blue_pixels > 20,
        "nitrogen label produced only {blue_pixels} blue pixels"
    );
    assert!(
        dark_pixels > 300,
        "bonds/labels produced only {dark_pixels} dark pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_v3000_charge_isotope_and_stereobond()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_molecule_v3000.mol");
    let output = temp.path().join("mol-v3000-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mol);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("18O-"));
    let rendered = render_svg_with_resvg(&svg)?;
    let red_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > pixel[1].saturating_add(60))
        .count();
    let dark_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 90 && pixel[1] < 90 && pixel[2] < 90)
        .count();
    assert!(
        red_pixels > 20,
        "oxygen isotope/charge label only had {red_pixels} red pixels"
    );
    assert!(
        dark_pixels > 200,
        "V3000 bonds/hash wedge only had {dark_pixels} dark pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_v2000_rxn_component_panels_and_arrow()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_reaction.rxn");
    let output = temp.path().join("rxn-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Rxn);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let dark_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 90 && pixel[1] < 90 && pixel[2] < 90)
        .count();
    assert!(
        dark_pixels > 1_000,
        "reaction diagram produced only {dark_pixels} dark pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_pdf_jpx_image_as_embedded_png() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("jpx-image.pdf");
    let output = temp.path().join("jpx-image-out");
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
            "Filter" => "JPXDecode",
        },
        include_bytes!("fixtures/sample_jpeg2000.jp2").to_vec(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
    });
    let content = lopdf::content::Content {
        operations: vec![
            lopdf::content::Operation::new("q", vec![]),
            lopdf::content::Operation::new(
                "cm",
                vec![
                    120.into(),
                    0.into(),
                    0.into(),
                    120.into(),
                    50.into(),
                    50.into(),
                ],
            ),
            lopdf::content::Operation::new("Do", vec![Object::Name(b"JPX".to_vec())]),
            lopdf::content::Operation::new("Q", vec![]),
        ],
    };
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode()?));
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
    document.save(&input)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Pdf);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data:image/png;base64,"));
    assert!(!svg_text.contains("data:image/jp2"));
    let rendered = render_svg_with_resvg(&svg)?;
    let grayscale_values = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] == pixel[1] && pixel[1] == pixel[2])
        .map(|pixel| pixel[0])
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        grayscale_values.len() >= 8,
        "only {} distinct grayscale levels rendered from JPX",
        grayscale_values.len()
    );
    Ok(())
}

#[test]
fn test_visual_rendering_pdf_jpx_embedded_alpha() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("jpx-alpha-image.pdf");
    let output = temp.path().join("jpx-alpha-image-out");
    let mut document = Document::with_version("1.7");
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "SMaskInData" => 1,
            "Filter" => "JPXDecode",
        },
        include_bytes!("fixtures/sample_jpeg2000_rgba.jp2").to_vec(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
    });
    let content = lopdf::content::Content {
        operations: vec![
            lopdf::content::Operation::new("q", vec![]),
            lopdf::content::Operation::new(
                "cm",
                vec![
                    120.into(),
                    0.into(),
                    0.into(),
                    120.into(),
                    50.into(),
                    50.into(),
                ],
            ),
            lopdf::content::Operation::new("Do", vec![Object::Name(b"JPX".to_vec())]),
            lopdf::content::Operation::new("Q", vec![]),
        ],
    };
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode()?));
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
    document.save(&input)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("data:image/png;base64,"));
    let rendered = render_svg_with_resvg(&svg)?;
    let pixels = rendered.data().chunks_exact(4).collect::<Vec<_>>();
    let opaque_red = pixels
        .iter()
        .filter(|pixel| pixel[0] > 240 && pixel[1] < 5 && pixel[2] < 5 && pixel[3] > 240)
        .count();
    let blended_half_alpha_green = pixels
        .iter()
        .filter(|pixel| {
            pixel[0] > 90
                && pixel[0] < 180
                && pixel[1] > 230
                && pixel[2] > 90
                && pixel[2] < 180
                && pixel[3] > 240
        })
        .count();
    assert!(opaque_red > 0, "opaque red from JPX was not visible");
    assert!(
        blended_half_alpha_green > 0,
        "half-transparent green from JPX was not composited against the page background"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_pdf_jpx_with_external_soft_mask() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = temp.path().join("jpx-external-soft-mask.pdf");
    let output = temp.path().join("jpx-external-soft-mask-out");
    let mut document = Document::with_version("1.7");
    let mask_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 2,
            "Height" => 2,
            "ColorSpace" => "DeviceGray",
            "BitsPerComponent" => 8,
        },
        vec![0, 64, 128, 255],
    ));
    let image_id = document.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => 4,
            "Height" => 4,
            "ColorSpace" => "DeviceRGB",
            "BitsPerComponent" => 8,
            "Filter" => "JPXDecode",
            "SMask" => Object::Reference(mask_id),
        },
        include_bytes!("fixtures/sample_jpeg2000_rgb.jp2").to_vec(),
    ));
    let resources_id = document.add_object(dictionary! {
        "XObject" => dictionary! { "JPX" => Object::Reference(image_id) },
    });
    let content = lopdf::content::Content {
        operations: vec![
            lopdf::content::Operation::new("q", vec![]),
            lopdf::content::Operation::new(
                "cm",
                vec![
                    120.into(),
                    0.into(),
                    0.into(),
                    120.into(),
                    40.into(),
                    40.into(),
                ],
            ),
            lopdf::content::Operation::new("Do", vec![Object::Name(b"JPX".to_vec())]),
            lopdf::content::Operation::new("Q", vec![]),
        ],
    };
    let pages_id = document.new_object_id();
    let page_id = document.new_object_id();
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode()?));
    document.objects.insert(
        page_id,
        Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "Contents" => Object::Reference(content_id),
            "Resources" => Object::Reference(resources_id),
            "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
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
    document.save(&input)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("data:image/png;base64,"));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_soft_blue_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] < 240 && pixel[1] < 240 && pixel[2] > 240 && pixel[3] > 240)
        .count();
    assert!(
        visible_soft_blue_pixels > 100,
        "only {visible_soft_blue_pixels} partially masked blue pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_pdf_jpx_smask_in_data_2_matte_unblending()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_jpx_smask_in_data_2.pdf");
    let output = temp.path().join("jpx-preblended-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;

    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("data:image/png;base64,"));
    let rendered = render_svg_with_resvg(&svg)?;
    let pixels = rendered.data().chunks_exact(4).collect::<Vec<_>>();
    let opaque_red = pixels
        .iter()
        .filter(|pixel| pixel[0] > 240 && pixel[1] < 5 && pixel[2] < 5 && pixel[3] > 240)
        .count();
    let unblended_half_alpha_green = pixels
        .iter()
        .filter(|pixel| {
            pixel[0] > 90
                && pixel[0] < 180
                && pixel[1] > 230
                && pixel[2] > 90
                && pixel[2] < 180
                && pixel[3] > 240
        })
        .count();
    assert!(opaque_red > 0, "opaque red was lost: {opaque_red} pixels");
    assert!(
        unblended_half_alpha_green > 0,
        "preblended green was not unblended before transparency: {unblended_half_alpha_green} pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_legacy_word_binary_main_story() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_legacy.doc");
    let output = temp.path().join("legacy-word-out");

    let report = convert_path(&input, &output, &ConvertOptions::default())?;

    assert_eq!(report.source_format, SourceFormat::Doc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("document-svg sample"));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_text_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(
        visible_text_pixels > 500,
        "only {visible_text_pixels} text pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_legacy_powerpoint_binary_slide_text()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_legacy.ppt");
    let output = temp.path().join("legacy-ppt-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ppt);
    assert_eq!(report.page_count, 2);
    let svg = fs::read(output.join("page-0002.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("One page in, one SVG out"));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_text_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(
        visible_text_pixels > 500,
        "only {visible_text_pixels} text pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_kicad_pcb_layers_and_pads() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.kicad_pcb");
    let output = temp.path().join("kicad-pcb-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;

    assert_eq!(report.source_format, SourceFormat::KicadPcb);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("PCB DEMO"));
    let rendered = render_svg_with_resvg(&svg)?;
    let pixels = rendered.data().chunks_exact(4).collect::<Vec<_>>();
    let front_copper_pixels = pixels
        .iter()
        .filter(|pixel| pixel[0] > 150 && pixel[1] < 120 && pixel[2] < 120 && pixel[3] > 240)
        .count();
    let back_copper_pixels = pixels
        .iter()
        .filter(|pixel| pixel[0] < 120 && pixel[1] > 70 && pixel[2] > 120 && pixel[3] > 240)
        .count();
    assert!(
        front_copper_pixels > 100,
        "front copper rendered {front_copper_pixels} pixels"
    );
    assert!(
        back_copper_pixels > 100,
        "back copper rendered {back_copper_pixels} pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_tiff_white_is_zero_low_bit_grayscale()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let gray_input = temp.path().join("white-is-zero.tif");
    fs::write(&gray_input, common::tiff_white_is_zero_gray(1))?;
    let palette_input = temp.path().join("palette.tif");
    fs::write(&palette_input, common::tiff_palette_1bit())?;
    assert!(matches!(
        convert_path(
            &palette_input,
            temp.path().join("palette-out"),
            &ConvertOptions::default()
        ),
        Err(document_svg::Error::Unsupported(_))
    ));

    let gray_output = temp.path().join("gray-out");
    let gray_report = convert_path(&gray_input, &gray_output, &ConvertOptions::default())?;
    assert_eq!(gray_report.page_count, 1);
    let gray = render_svg_with_resvg(&fs::read(gray_output.join("page-0001.svg"))?)?;
    let dark_pixels = gray
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 80 && pixel[1] < 80 && pixel[2] < 80)
        .count();
    assert!(
        dark_pixels > 20,
        "only {dark_pixels} WhiteIsZero TIFF pixels rendered black"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_cbz_pages_use_natural_filename_order()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("comic.cbz");
    let output = temp.path().join("cbz_visual_out");
    write_cbz_visual_fixture(&input)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Cbz);
    assert_eq!(report.page_count, 2);

    let first = render_svg_with_resvg(&fs::read(output.join("page-0001.svg"))?)?;
    let second = render_svg_with_resvg(&fs::read(output.join("page-0002.svg"))?)?;
    let count_pixels = |pixmap: &resvg::tiny_skia::Pixmap, is_red: bool| {
        pixmap
            .data()
            .chunks_exact(4)
            .filter(|pixel| {
                pixel[3] > 0
                    && if is_red {
                        pixel[0] > pixel[1].saturating_add(100)
                            && pixel[0] > pixel[2].saturating_add(100)
                    } else {
                        pixel[2] > pixel[0].saturating_add(100)
                            && pixel[2] > pixel[1].saturating_add(100)
                    }
            })
            .count()
    };
    assert!(
        count_pixels(&first, true) > 20,
        "natural page 2 should be first"
    );
    assert!(count_pixels(&first, false) == 0);
    assert!(
        count_pixels(&second, false) > 20,
        "natural page 10 should be second"
    );
    assert!(count_pixels(&second, true) == 0);
    Ok(())
}

#[test]
fn test_visual_rendering_abaqus_part_instances() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("frame.inp");
    let output = temp.path().join("abaqus_visual_out");
    let deck = "*Heading\nAbaqus visual mesh\n*Part, name=Tri\n*Node\n1, 0, 0, 0\n2, 1, 0, 0\n3, 0, 1, 0\n*Element, type=CPS3\n1, 1, 2, 3\n*End Part\n*Assembly, name=Assembly\n*Instance, name=Part-1, part=Tri\n*End Instance\n*Instance, name=Part-2, part=Tri\n2, 0, 0\n*End Instance\n*End Assembly\n";
    fs::write(&input, deck)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Abaqus);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Abaqus visual mesh"));
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    let rendered = render_svg_with_resvg(&svg)?;
    let colors = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        colors.len() > 3,
        "Abaqus mesh should render visibly over its background"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_nastran_bulk_data_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("plate.nas");
    let output = temp.path().join("nastran_visual_out");
    let deck =
        "$ Nastran visual mesh\nGRID,1,,0,0,0\nGRID,2,,1,0,0\nGRID,3,,0,1,0\nCTRIA3,10,1,1,2,3\n";
    fs::write(&input, deck)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Nastran);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Nastran Bulk Data mesh"));
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    let rendered = render_svg_with_resvg(&svg)?;
    let colors = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        colors.len() > 3,
        "Nastran mesh should render visibly over its background"
    );
    let outline_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 100 && pixel[1] > 110 && pixel[2] > 125)
        .count();
    assert!(
        outline_pixels > 100,
        "Nastran mesh outline should remain distinguishable from its dark background"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_lsdyna_keyword_shell_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("panel.k");
    let output = temp.path().join("lsdyna_visual_out");
    let deck = "*KEYWORD\n*TITLE\nLS-DYNA visual panel\n*ELEMENT_SHELL\n1,1,1,2,3,4\n*NODE\n1,0,0,0\n2,100,0,0\n3,100,50,0\n4,0,50,0\n*END\n";
    fs::write(&input, deck)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::LsDyna);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("LS-DYNA visual panel"));
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    let rendered = render_svg_with_resvg(&svg)?;
    let outline_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 100 && pixel[1] > 110 && pixel[2] > 125)
        .count();
    assert!(
        outline_pixels > 100,
        "LS-DYNA shell outline should remain distinguishable from its dark background"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_jupyter_code_and_png_output() -> Result<(), Box<dyn std::error::Error>> {
    use base64::Engine as _;

    let temp = tempdir()?;
    let input = temp.path().join("analysis.ipynb");
    let output = temp.path().join("jupyter_visual_out");
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 8, 8);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&[255, 0, 0].repeat(64))?;
    }
    let image = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    let notebook = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [
            {"cell_type":"markdown", "metadata":{}, "source":["# Notebook visual preview"]},
            {"cell_type":"code", "execution_count":1, "metadata":{}, "source":["plot()"], "outputs":[
                {"output_type":"display_data", "data":{"image/png":image}, "metadata":{}}
            ]}
        ]
    });
    fs::write(&input, notebook.to_string())?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Jupyter);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Notebook visual preview"));
    assert!(svg_text.contains("plot()"));
    assert!(svg_text.contains("data:image/png;base64,"));
    let rendered = render_svg_with_resvg(&svg)?;
    let red_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 200 && pixel[1] < 80 && pixel[2] < 80)
        .count();
    assert!(
        red_pixels > 20,
        "embedded notebook PNG output should be visible"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_jupyter_markdown_attachment_image()
-> Result<(), Box<dyn std::error::Error>> {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jupyter_attachment.ipynb");
    let temp = tempdir()?;
    let output = temp.path().join("jupyter-attachment-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Jupyter);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.data().chunks_exact(4).any(|pixel| pixel[3] > 0));
    Ok(())
}

#[test]
fn test_visual_rendering_quarto_frontmatter_and_source_chunks()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("report.qmd");
    let output = temp.path().join("quarto_visual_out");
    fs::write(
        &input,
        "---\ntitle: \"Quarto visual report\"\nformat: html\n---\n\n".to_owned()
            + "# Mesh summary\n\nThe code is shown as source.\n\n```{python}\nprint('preview')\n```\n",
    )?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Quarto);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("are not executed"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Quarto visual report"));
    assert!(svg_text.contains("print('preview')"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 240 || pixel[1] < 240 || pixel[2] < 240))
        .count();
    assert!(visible_pixels > 100, "Quarto content should render visibly");
    Ok(())
}

#[test]
fn test_visual_rendering_quarto_local_reference_images() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("report.qmd");
    fs::create_dir_all(temp.path().join("assets"))?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quarto_image.qmd"),
        &input,
    )?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        temp.path().join("assets/red-blue.png"),
    )?;
    let output = temp.path().join("quarto-image-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Quarto);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "Quarto local image red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "Quarto local image blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_jats_local_figure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("article.jats");
    fs::create_dir_all(temp.path().join("assets"))?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jats"),
        &input,
    )?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        temp.path().join("assets/red-blue.png"),
    )?;
    let output = temp.path().join("jats-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Jats);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(red_pixels > 0, "JATS figure red pixels were not rendered");
    assert!(blue_pixels > 0, "JATS figure blue pixels were not rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_docbook_local_figure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("article.dbk");
    fs::create_dir_all(temp.path().join("assets"))?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.docbook"),
        &input,
    )?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        temp.path().join("assets/red-blue.png"),
    )?;
    let output = temp.path().join("docbook-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Docbook);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "DocBook figure red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "DocBook figure blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_dita_local_figure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("guide.dita");
    fs::create_dir_all(temp.path().join("assets"))?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dita"),
        &input,
    )?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        temp.path().join("assets/red-blue.png"),
    )?;
    let output = temp.path().join("dita-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Dita);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(red_pixels > 0, "DITA figure red pixels were not rendered");
    assert!(blue_pixels > 0, "DITA figure blue pixels were not rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_pdb_models() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("models.pdb");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdb"),
        &input,
    )?;
    let output = temp.path().join("pdb-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Pdb);
    assert_eq!(report.page_count, 2);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(nonwhite > 100, "PDB model did not render visible markings");
    Ok(())
}

#[test]
fn test_visual_rendering_hwpx_package_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("report.hwpx");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.hwpx"),
        &input,
    )?;
    let output = temp.path().join("hwpx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Hwpx);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(red_pixels > 0, "HWPX image red pixels were not rendered");
    assert!(blue_pixels > 0, "HWPX image blue pixels were not rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_collada_triangle() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("triangle.dae");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.dae"),
        &input,
    )?;
    let output = temp.path().join("collada-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Collada);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 80 && pixel[0] < 100)
        .count();
    assert!(
        blue_pixels > 100,
        "COLLADA triangle was not visibly rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_x3d_triangle() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("triangle.x3d");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.x3d"),
        &input,
    )?;
    let output = temp.path().join("x3d-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::X3d);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 80 && pixel[0] < 100)
        .count();
    assert!(blue_pixels > 100, "X3D triangle was not visibly rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_xmind_outline() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("map.xmind");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xmind"),
        &input,
    )?;
    let output = temp.path().join("xmind-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xmind);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(nonwhite > 100, "XMind outline did not render visible text");
    Ok(())
}

#[test]
fn test_visual_rendering_xmind_json_outline() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("json-map.xmind");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_json.xmind"),
        &input,
    )?;
    let output = temp.path().join("xmind-json-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xmind);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(
        nonwhite > 100,
        "XMind JSON outline did not render visible text"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_nifti_slice() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("volume.nii");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nii"),
        &input,
    )?;
    let output = temp.path().join("nifti-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Nifti);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(nonwhite > 0, "NIfTI slice did not render");
    Ok(())
}

#[test]
fn test_visual_rendering_fits_plane() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("image.fits");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fits"),
        &input,
    )?;
    let output = temp.path().join("fits-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Fits);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(nonwhite > 0, "FITS plane did not render");
    Ok(())
}

#[test]
fn test_visual_rendering_mrc_plane() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("map.mrc");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mrc"),
        &input,
    )?;
    let output = temp.path().join("mrc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mrc);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(nonwhite > 0, "MRC plane did not render");
    Ok(())
}

#[test]
fn test_visual_rendering_sqlite_tables() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("data.sqlite");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sqlite"),
        &input,
    )?;
    let output = temp.path().join("sqlite-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sqlite);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(nonwhite > 100, "SQLite tables did not render visible cells");
    Ok(())
}

#[test]
fn test_visual_rendering_cif_models() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("structure.cif");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cif"),
        &input,
    )?;
    let output = temp.path().join("cif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Cif);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(nonwhite > 100, "CIF model did not render visible atoms");
    Ok(())
}

#[test]
fn test_visual_rendering_mol2_structure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("structure.mol2");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mol2"),
        &input,
    )?;
    let output = temp.path().join("mol2-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mol2);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(
        nonwhite > 100,
        "MOL2 structure did not render visible atoms"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_turtle_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("graph.ttl");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ttl"),
        &input,
    )?;
    let output = temp.path().join("turtle-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Turtle);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let nonwhite = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 230 || pixel[1] < 230 || pixel[2] < 230))
        .count();
    assert!(nonwhite > 100, "RDF table did not render visible cells");
    Ok(())
}

#[test]
fn test_visual_rendering_eps_triangle() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("triangle.eps");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.eps"),
        &input,
    )?;
    let output = temp.path().join("eps-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Eps);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let colored = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 100)
        .count();
    assert!(colored > 100, "EPS triangle did not render visibly");
    Ok(())
}

#[test]
fn test_visual_rendering_medit_ascii_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("surface.mesh");
    let output = temp.path().join("medit_visual_out");
    let mesh = "MeshVersionFormatted 2\nDimension 2\nVertices 4\n0 0 1\n100 0 1\n100 100 1\n0 100 1\nTriangles 2\n1 2 3 0\n1 3 4 0\nEnd\n";
    fs::write(&input, mesh)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Medit);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let outline_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 100 && pixel[1] > 110 && pixel[2] > 125)
        .count();
    assert!(
        outline_pixels > 100,
        "MEDIT mesh edges should render visibly"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_binary_medit_meshb() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/medit_binary_v2.meshb");
    let output = temp.path().join("meshb_visual_out");
    assert_eq!(fs::read(&input)?, common::medit_binary_tetrahedron());

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Medit);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("MEDIT binary finite-element mesh"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 240 || pixel[1] < 240 || pixel[2] < 240))
        .count();
    assert!(
        visible > 300,
        "binary MEDIT wireframe should render visibly"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_off_polygon_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("surface.off");
    let output = temp.path().join("off_visual_out");
    let mesh = "OFF\n4 1 4\n0 0 0\n100 0 0\n100 100 0\n0 100 0\n4 0 1 2 3\n";
    fs::write(&input, mesh)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Off);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("OFF polygon mesh"));
    assert!(svg_text.contains("data-semantic-role=\"obj:mesh\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible_colors = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        visible_colors.len() > 5,
        "OFF polygon should render with shading"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_ifc4_tessellated_geometry() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ifc");
    let output = temp.path().join("ifc-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ifc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("IFC tessellated building model"));
    assert!(text.contains("data-semantic-role=\"obj:mesh\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let colors = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        colors.len() > 8,
        "IFC model rendered only {} colors",
        colors.len()
    );
    let mesh_bounds = pixmap
        .data()
        .chunks_exact(4)
        .enumerate()
        .filter(|(_, pixel)| {
            u16::from(pixel[0]) > 20
                && u16::from(pixel[1]) > u16::from(pixel[0]) * 2
                && u16::from(pixel[2]) > u16::from(pixel[1]) * 3 / 2
        })
        .map(|(index, _)| {
            (
                (index as u32) % pixmap.width(),
                (index as u32) / pixmap.width(),
            )
        })
        .fold(None, |bounds, (x, y)| match bounds {
            None => Some((x, y, x, y)),
            Some((min_x, min_y, max_x, max_y)) => {
                Some((min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y)))
            }
        })
        .expect("IFC mesh colors should be visible");
    assert!(
        mesh_bounds.2 - mesh_bounds.0 > pixmap.width() / 2,
        "small IFC model should be fitted to the preview page"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_reused_ifc_mapped_mesh_instances() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_mapped.ifc");
    let output = temp.path().join("ifc-mapped-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ifc);
    assert_eq!(report.page_count, 1);
    assert!(!report.warnings.iter().any(|warning| {
        warning.contains("mapped geometry") || warning.contains("mapped representation")
    }));
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("data-semantic-role=\"obj:mesh\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let foreground = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3].iter().any(|channel| *channel < 242))
        .count();
    assert!(
        foreground > 500,
        "mapped IFC meshes rendered only {foreground} foreground pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_ifc_extruded_profile_solids() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_extruded.ifc");
    let output = temp.path().join("ifc-extruded-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ifc);
    assert_eq!(report.page_count, 1);
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("unsupported mapped"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("data-semantic-role=\"obj:mesh\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let foreground = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3].iter().any(|channel| *channel < 242))
        .count();
    assert!(
        foreground > 1_000,
        "extruded IFC solids rendered only {foreground} foreground pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_buildingsmart_ifcxml_geometry() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_ifcxml.ifcxml");
    let output = temp.path().join("ifcxml-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::IfcXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("data-semantic-role=\"obj:mesh\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let foreground = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3].iter().any(|channel| *channel < 242))
        .count();
    assert!(
        foreground > 1_000,
        "IFCXML solid rendered only {foreground} foreground pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_su2_two_dimensional_cfd_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.su2");
    let output = temp.path().join("su2-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Su2);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("SU2 CFD mesh"));
    assert!(text.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(text.contains("data-semantic-role=\"simulation:marker-boundary\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let mesh_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 80 && pixel[1] > 100 && pixel[2] > 130)
        .count();
    assert!(
        mesh_pixels > 500,
        "SU2 mesh rendered only {mesh_pixels} visible pixels"
    );
    let marker_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 200 && pixel[1] > 70 && pixel[1] < 180 && pixel[2] < 70)
        .count();
    assert!(
        marker_pixels > 500,
        "SU2 boundary markers rendered {marker_pixels} colored pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_su2_three_dimensional_volume_wireframe()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("tetrahedron.su2");
    let output = temp.path().join("su2-3d-out");
    fs::write(
        &input,
        "NDIME= 3\nNELEM= 1\n10 0 1 2 3\nNPOIN= 4\n0 0 0\n1 0 0\n0 1 0\n0 0 1\nNMARK= 1\nMARKER_TAG= wall\nMARKER_ELEMS= 4\n5 0 2 1\n5 0 1 3\n5 1 2 3\n5 2 0 3\n",
    )?;
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Su2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("XY plane"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(text.contains("data-semantic-role=\"simulation:marker-boundary\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let marker_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 200 && pixel[1] > 70 && pixel[1] < 180 && pixel[2] < 70)
        .count();
    assert!(
        marker_pixels > 500,
        "SU2 3D boundary wireframe rendered only {marker_pixels} orange pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_openfoam_boundary_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openfoam_case/sample.foam");
    let temp = tempdir()?;
    let output = temp.path().join("openfoam-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OpenFoam);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("simulation:mesh"));
    assert!(text.contains("simulation:boundary-patch"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let orange_edges = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 200 && pixel[1] > 70 && pixel[1] < 180 && pixel[2] < 70)
        .count();
    assert!(
        orange_edges > 500,
        "OpenFOAM boundary edges rendered {orange_edges} pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_gzip_compressed_openfoam_boundary_mesh()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openfoam_case");
    let case = temp.path().join("compressed-case");
    let poly_mesh = case.join("constant/polyMesh");
    fs::create_dir_all(&poly_mesh)?;
    fs::copy(source.join("sample.foam"), case.join("case.foam"))?;
    for name in ["points", "faces", "owner", "neighbour", "boundary"] {
        let input = fs::read(source.join("constant/polyMesh").join(name))?;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&input)?;
        fs::write(poly_mesh.join(format!("{name}.gz")), encoder.finish()?)?;
    }
    let output = temp.path().join("openfoam-gzip-visual-out");
    let report = convert_path(case.join("case.foam"), &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OpenFoam);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("simulation:boundary-patch"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let orange_edges = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 200 && pixel[1] > 70 && pixel[1] < 180 && pixel[2] < 70)
        .count();
    assert!(
        orange_edges > 500,
        "gzip-compressed OpenFOAM boundary edges rendered {orange_edges} pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_ply_point_cloud_without_faces() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("point-cloud.ply");
    let output = temp.path().join("ply-point-cloud-out");
    fs::write(
        &input,
        "ply\nformat ascii 1.0\nelement vertex 6\nproperty float x\nproperty float y\nproperty float z\nproperty uchar red\nproperty uchar green\nproperty uchar blue\nend_header\n-10 0 0 255 0 0\n10 0 0 0 255 0\n0 -10 0 0 0 255\n0 10 0 255 255 0\n0 0 -10 255 0 255\n0 0 10 0 255 255\n",
    )?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ply);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("PLY Point Cloud"));
    assert!(svg_text.contains("data-semantic-role=\"ply:point-cloud\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 150 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    assert!(
        red_pixels > 20,
        "only {red_pixels} red PLY point pixels rendered"
    );
    let green_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 100 && pixel[1] > 150 && pixel[2] < 100)
        .count();
    assert!(
        green_pixels > 20,
        "only {green_pixels} green PLY point pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_las_rgb_point_cloud() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_rgb.las");
    let output = temp.path().join("las-point-cloud-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Las);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("LAS/LAZ Point Cloud"));
    assert!(svg_text.contains("data-semantic-role=\"las:point-cloud\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible_rgb = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && ((pixel[0] > 180 && pixel[1] < 90 && pixel[2] < 90)
                    || (pixel[0] < 90 && pixel[1] > 180 && pixel[2] < 90)
                    || (pixel[0] < 90 && pixel[1] < 90 && pixel[2] > 180))
        })
        .count();
    assert!(
        visible_rgb > 50,
        "only {visible_rgb} RGB point pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_multiscan_ptx_clouds() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ptx");
    let output = temp.path().join("ptx-point-cloud-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ptx);
    assert_eq!(report.page_count, 2);
    let first_svg = fs::read(output.join("page-0001.svg"))?;
    let first_text = std::str::from_utf8(&first_svg)?;
    assert!(first_text.contains("PTX scan 1"));
    assert!(first_text.contains("ptx:point-cloud"));
    assert!(first_text.contains("fill=\"#FF009B\""));
    assert!(first_text.contains("fill=\"#00FF9B\""));
    assert!(first_text.contains("fill=\"#2563EB\""));
    let first = render_svg_with_resvg(&first_svg)?;
    let colored = first
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            let minimum = pixel[0].min(pixel[1]).min(pixel[2]);
            let maximum = pixel[0].max(pixel[1]).max(pixel[2]);
            pixel[3] > 0 && maximum.saturating_sub(minimum) > 80
        })
        .count();
    assert!(
        colored > 20,
        "only {colored} transformed PTX RGB pixels rendered"
    );

    let second_svg = fs::read(output.join("page-0002.svg"))?;
    let second_text = std::str::from_utf8(&second_svg)?;
    assert!(second_text.contains("PTX scan 2"));
    let second = render_svg_with_resvg(&second_svg)?;
    let gray = second
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0 && pixel[0] == pixel[1] && pixel[1] == pixel[2] && pixel[0] < 240
        })
        .count();
    assert!(gray > 10, "only {gray} grayscale intensity pixels rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_leica_pts_rgb_cloud() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pts");
    let output = temp.path().join("pts-point-cloud-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Pts);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("pts:point-cloud"));
    assert!(svg_text.contains("fill=\"#2563EB\""));
    let rendered = render_svg_with_resvg(&svg)?;
    let colored = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && pixel[0].max(pixel[1]).max(pixel[2]) - pixel[0].min(pixel[1]).min(pixel[2]) > 80
        })
        .count();
    assert!(colored > 10, "only {colored} colored PTS pixels rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_real_e57_bunny_cloud() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_e57_bunny.e57");
    let output = temp.path().join("e57-bunny-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::E57);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("e57:point-cloud"));
    let rendered = render_svg_with_resvg(&svg)?;
    let blue_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 150 && pixel[0] < 120 && pixel[1] < 180)
        .count();
    assert!(
        blue_pixels > 10_000,
        "only {blue_pixels} E57 points rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_whitespace_xyz_rgb_cloud() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xyz");
    let output = temp.path().join("xyz-point-cloud-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xyz);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("xyz:point-cloud"));
    assert!(svg_text.contains("fill=\"#000000\""));
    let rendered = render_svg_with_resvg(&svg)?;
    let colored = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && pixel[0].max(pixel[1]).max(pixel[2]) - pixel[0].min(pixel[1]).min(pixel[2]) > 80
        })
        .count();
    assert!(colored > 10, "only {colored} colored XYZ pixels rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_esri_ascii_grid_heatmap() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_elevation.asc");
    let output = temp.path().join("ascii-grid-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::EsriAsciiGrid);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("esri:ascii-grid-raster"));
    assert!(svg_text.contains("esri:ascii-grid-legend"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0)
        .count();
    let chromatic = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && pixel[0].max(pixel[1]).max(pixel[2]) - pixel[0].min(pixel[1]).min(pixel[2]) > 30
        })
        .count();
    assert!(
        visible > 1000,
        "only {visible} visible grid pixels rendered"
    );
    assert!(
        chromatic > 100,
        "only {chromatic} colored grid pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_dbase_attribute_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_attributes.dbf");
    let output = temp.path().join("dbase-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Dbf);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("deleted dBASE record"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Café Moreno"));
    assert!(!svg_text.contains("Willow Farm"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 500, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_toml_configuration_values() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_config.toml");
    let output = temp.path().join("toml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Toml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("TOML configuration"));
    assert!(text.contains("$.service.listen"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 500, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_generic_xml_configuration() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_config.xml");
    let output = temp.path().join("xml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("namespace ns1 = \"urn:example:application\""));
    assert!(text.contains("/ns1:application/service[1]/@id = \"catalog-api\""));
    assert!(text.contains("full-text &amp; faceted"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 500, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_java_properties() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_properties.properties");
    let output = temp.path().join("properties-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Properties);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("duplicate"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("Café"));
    assert!(text.contains("${HOME} is displayed as text"));
    assert!(text.contains("release.channel") && text.contains("stable"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 500, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_bpmn_process_diagram() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_bpmn.bpmn");
    let output = temp.path().join("bpmn-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Bpmn);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("Validate order"));
    assert!(text.contains("Notify customer"));
    assert!(text.contains("bpmn:sequence-flow-arrow"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 1_000, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_dmn_decision_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_dmn.dmn");
    let output = temp.path().join("dmn-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Dmn);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("Loan approval"));
    assert!(text.contains("Age"));
    assert!(text.contains("manual review"));
    assert!(text.contains("FEEL is"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 1_000, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_cmmn_case_plan() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_cmmn.cmmn");
    let output = temp.path().join("cmmn-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Cmmn);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("Claims file"));
    assert!(text.contains("Review documents"));
    assert!(text.contains("cmmn:connector"));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not evaluated"))
    );
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 1_000, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_reqif_requirements() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_requirements.reqif");
    let output = temp.path().join("reqif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Reqif);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("XHTML"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("Vehicle braking requirements"));
    assert!(text.contains("Requirement details"));
    assert!(text.contains("Stopping distance"));
    assert!(text.contains("Derives"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 1_000, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_xmi_model() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_model.xmi");
    let output = temp.path().join("xmi-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xmi);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("XMI model"));
    assert!(text.contains("BrakeController"));
    assert!(text.contains("WheelSensor"));
    assert!(text.contains("type=double"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let ink = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
        .count();
    assert!(ink > 1_000, "only {ink} non-white pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_paginated_quoted_csv() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("records.csv");
    let output = temp.path().join("csv-out");
    let mut source =
        String::from("Name,Note,Score\r\n\"Acme, Inc.\",\"She said \"\"yes\"\"\",12\r\n");
    for row in 1..=100 {
        source.push_str(&format!("item{row},detail{row},{row}\r\n"));
    }
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Csv);
    assert_eq!(report.page_count, 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not embedded"))
    );
    for page_number in 1..=2 {
        let svg = fs::read(output.join(format!("page-{page_number:04}.svg")))?;
        let svg_text = std::str::from_utf8(&svg)?;
        if page_number == 1 {
            assert!(svg_text.contains("Acme, Inc."));
            assert!(svg_text.contains("She said \"yes\""));
            assert!(svg_text.contains("item99"));
            assert!(!svg_text.contains("item100"));
        } else {
            assert!(svg_text.contains("item100"));
        }
        let pixmap = render_svg_with_resvg(&svg)?;
        let ink = pixmap
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && pixel[..3] != [255, 255, 255])
            .count();
        assert!(
            ink > 500,
            "page {page_number} has only {ink} non-white pixels"
        );
    }
    Ok(())
}

#[test]
fn test_visual_rendering_legacy_xls_worksheet() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy_sample.xls");
    let output = temp.path().join("legacy-xls-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xls);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Alpha"));
    assert!(svg_text.contains("99.75"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let dark_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 150 && pixel[1] < 150 && pixel[2] < 150)
        .count();
    assert!(
        dark_pixels > 100,
        "only {dark_pixels} dark Excel table pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_xlsb_worksheet() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/calamine_any_sheets.xlsb");
    let output = temp.path().join("xlsb-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xlsb);
    assert_eq!(report.page_count, 3);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Visible — rows 1–5"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let dark_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 150 && pixel[1] < 150 && pixel[2] < 150)
        .count();
    assert!(
        dark_pixels > 100,
        "only {dark_pixels} dark XLSB table pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_binary_dxf_geometry() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/binary_line.dxf");
    let output = temp.path().join("binary-dxf-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Dxf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("data-source-format=\"dxf\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let dark_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 160 && pixel[1] < 160 && pixel[2] < 160)
        .count();
    assert!(
        dark_pixels > 20,
        "only {dark_pixels} binary DXF geometry pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_geojson_map() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.geojson");
    let output = temp.path().join("geojson-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::GeoJson);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("geojson:polygon"));
    assert!(svg_text.contains("geojson:line"));
    assert!(svg_text.contains("geojson:point"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 180 && pixel[0] < 180)
        .count();
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100)
        .count();
    assert!(
        blue_pixels > 500,
        "only {blue_pixels} blue map pixels rendered"
    );
    assert!(red_pixels > 20, "only {red_pixels} red map pixels rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_rfc8142_geojson_sequence() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.geojsons");
    let output = temp.path().join("geojson-sequence-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 180 && pixel[0] < 180)
        .count();
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100)
        .count();
    assert_eq!(report.source_format, SourceFormat::GeoJsonSeq);
    assert!(svg_text.contains("geojsonseq:polygon"));
    assert!(svg_text.contains("geojsonseq:line"));
    assert!(svg_text.contains("geojsonseq:point"));
    assert!(
        blue_pixels > 500,
        "only {blue_pixels} sequence map pixels rendered"
    );
    assert!(
        red_pixels > 10,
        "only {red_pixels} sequence point pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_generic_json_text_sequence() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsons");
    let output = temp.path().join("json-sequence-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let dark_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 90 && pixel[1] < 90 && pixel[2] < 90)
        .count();
    assert_eq!(report.source_format, SourceFormat::JsonSeq);
    assert!(svg_text.contains("JSON Text Sequence"));
    assert!(svg_text.contains("JSON sequence record 4"));
    assert!(
        dark_pixels > 100,
        "only {dark_pixels} JSON sequence text pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_topojson_shared_polygon_arcs() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.topojson");
    let output = temp.path().join("topojson-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 180 && pixel[0] < 180)
        .count();
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100)
        .count();
    assert_eq!(report.source_format, SourceFormat::TopoJson);
    assert!(svg_text.contains("topojson:polygon"));
    assert!(svg_text.contains("topojson:point"));
    assert!(
        blue_pixels > 1000,
        "only {blue_pixels} TopoJSON map pixels rendered"
    );
    assert!(
        red_pixels > 10,
        "only {red_pixels} TopoJSON point pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_shapefile_polygon_hole_and_disjoint_shells()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_polygon.shp");
    let output = temp.path().join("shapefile-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && pixel[2] > pixel[0].saturating_add(25)
                && pixel[2] > pixel[1].saturating_add(15)
        })
        .count();
    assert_eq!(report.source_format, SourceFormat::Shapefile);
    assert!(svg_text.contains("shapefile:polygon"));
    assert!(
        blue_pixels > 1000,
        "only {blue_pixels} polygon pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_geopackage_polygon_hole() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_features.gpkg");
    let output = temp.path().join("geopackage-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && pixel[2] > pixel[0].saturating_add(25)
                && pixel[2] > pixel[1].saturating_add(15)
        })
        .count();
    let hole_x = (600.0 * f64::from(pixmap.width()) / 1200.0).round() as usize;
    let hole_y = (402.0 * f64::from(pixmap.height()) / 800.0).round() as usize;
    let pixel_start = (hole_y * pixmap.width() as usize + hole_x) * 4;
    let pixel = &pixmap.data()[pixel_start..pixel_start + 4];
    assert_eq!(report.source_format, SourceFormat::Geopackage);
    assert!(svg_text.contains("geopackage:polygon"));
    assert!(
        blue_pixels > 10_000,
        "only {blue_pixels} polygon pixels rendered"
    );
    assert!(
        !(pixel[3] > 0
            && pixel[2] > pixel[0].saturating_add(25)
            && pixel[2] > pixel[1].saturating_add(15)),
        "polygon hole unexpectedly filled at center pixel {pixel:?}"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_geopackage_raster_tiles() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_tile_mixed.gpkg");
    let output = temp.path().join("geopackage-tile-visual");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    let svg = fs::read(output.join("page-0002.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| {
            pixel[3] > 0
                && pixel[2] > pixel[0].saturating_add(20)
                && pixel[1] > pixel[0].saturating_add(20)
        })
        .count();
    assert_eq!(report.source_format, SourceFormat::Geopackage);
    assert_eq!(report.page_count, 2);
    assert!(svg_text.contains("data-semantic-role=\"geopackage:tile\""));
    assert!(
        blue_pixels > 1000,
        "only {blue_pixels} blue map tile pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_gpx_waypoints_routes_and_tracks() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gpx");
    let output = temp.path().join("gpx-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Gpx);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("gpx:line"));
    assert!(svg_text.contains("gpx:point"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 180 && pixel[0] < 180)
        .count();
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100)
        .count();
    assert!(
        blue_pixels > 100,
        "only {blue_pixels} blue GPX route/track pixels rendered"
    );
    assert!(
        red_pixels > 5,
        "only {red_pixels} red GPX waypoint pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_georss_simple_feed() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.georss");
    let output = temp.path().join("georss-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::GeoRss);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("georss:polygon"));
    assert!(svg_text.contains("georss:line"));
    assert!(svg_text.contains("georss:point"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 180 && pixel[0] < 180)
        .count();
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100)
        .count();
    assert!(
        blue_pixels > 100,
        "only {blue_pixels} blue GeoRSS geometry pixels rendered"
    );
    assert!(
        red_pixels > 5,
        "only {red_pixels} red GeoRSS point pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_gml_feature_geometries() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gml");
    let output = temp.path().join("gml-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Gml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("gml:polygon"));
    assert!(svg_text.contains("gml:line"));
    assert!(svg_text.contains("gml:point"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 180 && pixel[0] < 180)
        .count();
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100)
        .count();
    assert!(
        blue_pixels > 100,
        "only {blue_pixels} blue GML geometry pixels rendered"
    );
    assert!(
        red_pixels > 5,
        "only {red_pixels} red GML point pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_wkt_simple_features() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wkt");
    let output = temp.path().join("wkt-visual-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Wkt);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("wkt:polygon"));
    assert!(svg_text.contains("wkt:line"));
    assert!(svg_text.contains("wkt:point"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 180 && pixel[0] < 180)
        .count();
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100)
        .count();
    assert!(
        blue_pixels > 100,
        "only {blue_pixels} blue WKT geometry pixels rendered"
    );
    assert!(
        red_pixels > 5,
        "only {red_pixels} red WKT point pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_kml_and_kmz_maps() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (filename, expected_format) in [
        ("sample.kml", SourceFormat::Kml),
        ("sample.kmz", SourceFormat::Kmz),
    ] {
        let input = fixture_dir.join(filename);
        let output = temp
            .path()
            .join(format!("{}-visual-out", filename.replace('.', "-")));
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.source_format, expected_format);
        let svg = fs::read(output.join("page-0001.svg"))?;
        let svg_text = std::str::from_utf8(&svg)?;
        assert!(svg_text.contains("kml:point") || svg_text.contains("kmz:point"));
        let pixmap = render_svg_with_resvg(&svg)?;
        let blue_pixels = pixmap
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && pixel[2] > 180 && pixel[0] < 180)
            .count();
        let red_pixels = pixmap
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100)
            .count();
        if expected_format == SourceFormat::Kml {
            assert!(
                blue_pixels > 100,
                "{filename}: only {blue_pixels} blue map pixels rendered"
            );
        }
        assert!(
            red_pixels > 5,
            "{filename}: only {red_pixels} red map pixels rendered"
        );
    }
    Ok(())
}

#[test]
fn test_visual_rendering_pcd_compressed_point_cloud() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("cloud.pcd");
    let output = temp.path().join("pcd-visual-out");
    let header = "# .PCD v0.7\nVERSION .7\nFIELDS x y z rgb\nSIZE 4 4 4 4\nTYPE F F F F\nCOUNT 1 1 1 1\nWIDTH 4\nHEIGHT 1\nVIEWPOINT 0 0 0 1 0 0 0\nPOINTS 4\nDATA ";
    let points = [
        [0.0f32, 0.0, 0.0],
        [10.0, 0.0, 0.0],
        [0.0, 10.0, 0.0],
        [0.0, 0.0, 10.0],
    ];
    let colors = [0x00ff0000u32, 0x0000ff00, 0x000000ff, 0x00ffffff];
    let mut soa = Vec::new();
    for axis in 0..3 {
        for point in points {
            soa.extend_from_slice(&point[axis].to_le_bytes());
        }
    }
    for color in colors {
        soa.extend_from_slice(&color.to_le_bytes());
    }
    let mut compressed = Vec::new();
    for chunk in soa.chunks(32) {
        compressed.push(u8::try_from(chunk.len() - 1)?);
        compressed.extend_from_slice(chunk);
    }
    let mut message = format!("{header}binary_compressed\n").into_bytes();
    message.extend_from_slice(&u32::try_from(compressed.len())?.to_le_bytes());
    message.extend_from_slice(&u32::try_from(soa.len())?.to_le_bytes());
    message.extend_from_slice(&compressed);
    fs::write(&input, message)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Pcd);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("PCD Point Cloud"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 150 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    assert!(
        red_pixels > 20,
        "only {red_pixels} red PCD point pixels rendered"
    );
    let green_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 100 && pixel[1] > 150 && pixel[2] < 100)
        .count();
    assert!(
        green_pixels > 20,
        "only {green_pixels} green PCD points rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_vsdx_page_shape_and_text() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("flow.vsdx");
    let output = temp.path().join("vsdx_visual_out");
    let parts = [
        (
            "_rels/.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/visio/2010/relationships/visioDocument" Target="visio/document.xml"/></Relationships>"#,
        ),
        (
            "visio/document.xml",
            r#"<VisioDocument xmlns="urn:visio"/>"#,
        ),
        (
            "visio/_rels/document.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rIdPages" Type="http://schemas.microsoft.com/visio/2010/relationships/pages" Target="pages/pages.xml"/></Relationships>"#,
        ),
        (
            "visio/pages/pages.xml",
            r#"<Pages xmlns="urn:visio" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><Page ID="0" NameU="Visual check" r:id="rId1"/></Pages>"#,
        ),
        (
            "visio/pages/_rels/pages.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/visio/2010/relationships/page" Target="page1.xml"/></Relationships>"#,
        ),
        (
            "visio/pages/page1.xml",
            r##"<PageContents xmlns="urn:visio"><PageSheet><Cell N="PageWidth" V="5"/><Cell N="PageHeight" V="4"/></PageSheet><Shapes><Shape ID="1" NameU="Process" Type="Shape"><Cell N="PinX" V="2.5"/><Cell N="PinY" V="2"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="LocPinX" V="1"/><Cell N="LocPinY" V="0.5"/><Cell N="FillForegnd" V="#E02020"/><Cell N="LineColor" V="#112233"/><Section N="Geometry"><Row T="RelMoveTo"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row><Row T="RelLineTo"><Cell N="X" V="0"/><Cell N="Y" V="1"/></Row></Section><Text>Visible shape</Text></Shape></Shapes></PageContents>"##,
        ),
    ];
    let mut archive = ZipWriter::new(fs::File::create(&input)?);
    for (name, contents) in parts {
        archive.start_file(name, SimpleFileOptions::default())?;
        archive.write_all(contents.as_bytes())?;
    }
    archive.finish()?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Visio);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data-semantic-role=\"office:visio-shape\""));
    assert!(svg_text.contains("Visible shape"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 150 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    assert!(red_pixels > 1_000, "only {red_pixels} red pixels rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_legacy_vdx_page_shape() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("legacy.vdx");
    let output = temp.path().join("vdx_visual_out");
    let xml = r##"<?xml version="1.0"?><VisioDocument xmlns="urn:visio"><Pages><Page ID="1" NameU="Legacy"><PageSheet><Cell N="PageWidth" V="4"/><Cell N="PageHeight" V="3"/></PageSheet><Shapes><Shape ID="2" NameU="Process" Type="Shape"><Cell N="PinX" V="2"/><Cell N="PinY" V="1.5"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Cell N="FillForegnd" V="#22AA44"/><Text>Legacy VDX</Text></Shape></Shapes></Page></Pages></VisioDocument>"##;
    fs::write(&input, xml)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Visio);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Legacy VDX"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let green_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[1] > 100 && pixel[0] < 100 && pixel[2] < 100)
        .count();
    assert!(
        green_pixels > 1_000,
        "only {green_pixels} green pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_eml_message_as_a_document_page() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = temp.path().join("message.eml");
    let output = temp.path().join("eml_visual_out");
    fs::write(
        &input,
        "From: Alice <alice@example.test>\r\nSubject: Visual email\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nThis message is rendered as a clean document page.\r\n",
    )?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Eml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Visual email"));
    assert!(svg_text.contains("This message is rendered as a clean document page."));
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(
        visible_pixels > 500,
        "only {visible_pixels} visible pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_eml_cid_inline_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("inline-image.eml");
    let output = temp.path().join("eml_inline_visual_out");
    let png =
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"))?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png);
    let message = format!(
        "From: Alice <alice@example.test>\r\nSubject: Inline image\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=eml; type=\"text/html\"\r\n\r\n--eml\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>Before image.</p><img src=\"cid:eml-logo\" alt=\"brand logo\"><img src=\"https://example.invalid/eml-logo.png\" alt=\"location logo\"><img src=\"https://example.invalid/tracker.png\"></body></html>\r\n--eml\r\nContent-Type: image/png\r\nContent-ID: <eml-logo>\r\nContent-Location: https://example.invalid/eml-logo.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n--eml--\r\n"
    );
    fs::write(&input, message)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Eml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert_eq!(svg_text.matches("data:image/png;base64,").count(), 2);
    assert!(!svg_text.contains("example.invalid"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 80 && pixel[3] > 0)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[2] > 180 && pixel[0] < 80 && pixel[1] < 80 && pixel[3] > 0)
        .count();
    assert!(red_pixels > 0, "EML CID red pixels were not rendered");
    assert!(blue_pixels > 0, "EML CID blue pixels were not rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_mbox_archive_pages() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("archive.mbox");
    let output = temp.path().join("mbox_visual_out");
    let mbox = concat!(
        "From alice@example.test Sat Sep 14 10:00:00 2024\r\n",
        "From: Alice <alice@example.test>\r\nSubject: First visual message\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n\r\nFirst visible body.\r\n\r\n",
        "From bob@example.test Sun Sep 15 11:30:00 2024\r\n",
        "From: Bob <bob@example.test>\r\nSubject: Second visual message\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n\r\nSecond visible body.\r\n",
    );
    fs::write(&input, mbox)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mbox);
    assert_eq!(report.page_count, 2);
    for (index, label) in ["First visual message", "Second visual message"]
        .into_iter()
        .enumerate()
    {
        let svg = fs::read(output.join(format!("page-{:04}.svg", index + 1)))?;
        let svg_text = std::str::from_utf8(&svg)?;
        assert!(svg_text.contains(label));
        assert!(svg_text.contains("visible body"));
        let pixmap = render_svg_with_resvg(&svg)?;
        let non_white_pixels = pixmap
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
            .count();
        assert!(
            non_white_pixels > 100,
            "only {non_white_pixels} marked pixels rendered"
        );
    }
    Ok(())
}

#[test]
fn test_visual_rendering_mhtml_saved_web_page() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("saved.mhtml");
    let output = temp.path().join("mhtml_visual_out");
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 20, 10);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        let pixels = [220u8, 25, 20].repeat(20 * 10);
        writer.write_image_data(&pixels)?;
    }
    let encoded_png = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    let message = format!(
        "MIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=page; type=\"text/html\"; start=\"<root-page>\"\r\nContent-Base: https://example.test/archive/\r\n\r\n--page\r\nContent-Type: text/html; charset=utf-8\r\nContent-ID: <decoy-page>\r\n\r\n<html><body><h1>Wrong decoy root</h1></body></html>\r\n--page\r\nContent-Type: text/html; charset=utf-8\r\nContent-ID: <root-page>\r\nContent-Location: page.html\r\n\r\n<html><head><base href=\"assets/\"></head><body><h1>Saved web page</h1><p>Local archived text is visible.</p><img src=\"cid:hero-image\" alt=\"red hero\"><img src=\"hero.png\" alt=\"relative red hero\"></body></html>\r\n--page\r\nContent-Type: image/png\r\nContent-ID: <hero-image>\r\nContent-Location: assets/hero.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{encoded_png}\r\n--page--\r\n"
    );
    fs::write(&input, message)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mhtml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(
        svg_text.contains("Saved web page"),
        "saved={}, decoy={}, warnings={:?}",
        svg_text.contains("Saved web page"),
        svg_text.contains("Wrong decoy root"),
        report.warnings
    );
    assert!(!svg_text.contains("Wrong decoy root"));
    assert!(svg_text.contains("Local archived text is visible."));
    assert_eq!(svg_text.matches("data:image/png;base64,").count(), 2);
    let pixmap = render_svg_with_resvg(&svg)?;
    let non_white_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(
        non_white_pixels > 500,
        "only {non_white_pixels} marked pixels rendered"
    );
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 150 && pixel[1] < 80 && pixel[2] < 80)
        .count();
    assert!(
        red_pixels > 100,
        "only {red_pixels} CID image pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_ical_event_page() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("event.ics");
    let output = temp.path().join("ical_visual_out");
    fs::write(
        &input,
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:event-1\r\nDTSTART:20241012T090000Z\r\nDTEND:20241012T100000Z\r\nSUMMARY:Visual calendar event\r\nLOCATION:Meeting room\r\nDESCRIPTION:Calendar details are rendered as a document.\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
    )?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ical);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Visual calendar event"));
    assert!(svg_text.contains("Calendar details are rendered as a document."));
    let pixmap = render_svg_with_resvg(&svg)?;
    let non_white_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(
        non_white_pixels > 500,
        "only {non_white_pixels} marked pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_vcalendar_10_legacy_event() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("meeting.vcs");
    let output = temp.path().join("vcalendar_visual_out");
    let source = fs::read_to_string("tests/fixtures/meeting.vcs")?;
    fs::write(&input, source.replace('\n', "\r\n"))?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Vcalendar);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("Legacy vCalendar meeting"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let non_white_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(
        non_white_pixels > 300,
        "legacy calendar rendered only {non_white_pixels} non-white pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_vcard_contacts() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("contacts.vcf");
    let output = temp.path().join("vcard_visual_out");
    let source = fs::read_to_string("tests/fixtures/contact.vcf")?;
    fs::write(&input, source.replace('\n', "\r\n"))?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Vcard);
    assert_eq!(report.page_count, 2);
    for (page, expected) in [(1, "山田花子"), (2, "Ren Tanaka")] {
        let svg = fs::read(output.join(format!("page-{page:04}.svg")))?;
        let svg_text = std::str::from_utf8(&svg)?;
        assert!(svg_text.contains(expected));
        let pixmap = render_svg_with_resvg(&svg)?;
        let non_white_pixels = pixmap
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
            .count();
        assert!(
            non_white_pixels > 300,
            "contact page {page} rendered only {non_white_pixels} non-white pixels"
        );
    }
    Ok(())
}

#[test]
fn test_visual_rendering_vcard_21_quoted_printable() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("legacy-contact.vcf");
    let output = temp.path().join("vcard21_visual_out");
    let source = fs::read_to_string("tests/fixtures/legacy_contact_21.vcf")?;
    fs::write(&input, source.replace('\n', "\r\n"))?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Vcard);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("Hanako 山田花子"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let non_white_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(non_white_pixels > 300);
    Ok(())
}

#[test]
fn test_visual_rendering_outlook_msg_email() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("message.msg");
    let output = temp.path().join("msg_visual_out");
    fs::write(&input, common::outlook_msg_bytes())?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Msg);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Outlook preview"));
    assert!(svg_text.contains("Rendered"));
    assert!(svg_text.contains("HTML"));
    assert!(svg_text.contains("日本語表示"));
    assert!(!svg_text.contains("window.alert"));
    assert!(!svg_text.contains("example.invalid"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let non_white_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(
        non_white_pixels > 500,
        "only {non_white_pixels} message pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_vtk_xml_rectilinear_grid() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("pressure.vtr");
    let output = temp.path().join("vtr_out");
    let source = r#"<?xml version="1.0"?>
<VTKFile type="RectilinearGrid" version="1.0" byte_order="LittleEndian">
  <RectilinearGrid WholeExtent="0 2 0 1 0 0">
    <Piece Extent="0 2 0 1 0 0">
      <CellData Scalars="pressure"><DataArray type="Float32" Name="pressure" format="ascii">0 1</DataArray></CellData>
      <Coordinates>
        <DataArray type="Float32" format="ascii">0 40 100</DataArray>
        <DataArray type="Float32" format="ascii">0 100</DataArray>
        <DataArray type="Float32" format="ascii">0</DataArray>
      </Coordinates>
    </Piece>
  </RectilinearGrid>
</VTKFile>"#;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg_text.contains("colorbar-max"));

    let rendered = render_svg_with_resvg(&svg)?;
    assert_colored_mesh_fills_page_width(&rendered);
    let distinct_colors = rendered
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct_colors.len() > 24,
        "VTK RectilinearGrid field should render its grid and colorbar"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_binary_legacy_vtk() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("stress-binary.vtk");
    let output = temp.path().join("binary-vtk-out");
    fs::write(&input, binary_legacy_vtk_sample())?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    assert!(svg_text.contains("colorbar-max"));

    let rendered = render_svg_with_resvg(&svg)?;
    assert_colored_mesh_fills_page_width(&rendered);
    let distinct_colors = rendered
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct_colors.len() > 24,
        "binary Legacy VTK scalar values should be visible"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_legacy_vtk_polydata_cell_scalars() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = temp.path().join("polydata.vtk");
    let output = temp.path().join("legacy_vtk_out");
    let source = r#"# vtk DataFile Version 3.0
Legacy PolyData cell scalar check
ASCII
DATASET POLYDATA
POINTS 4 float
0 0 0
100 0 0
100 100 0
0 100 0
POLYGONS 2 8
3 0 1 2
3 0 2 3
CELL_DATA 2
SCALARS stress float 1
LOOKUP_TABLE default
2 8
"#;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("colorbar-max"));
    assert!(svg_text.contains("fill=\"#000080\""));
    assert!(svg_text.contains("fill=\"#600000\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let distinct_colors = pixmap
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(distinct_colors.len() > 20);
    Ok(())
}

#[test]
fn test_visual_rendering_gmsh_v41_block_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("block_mesh.msh");
    let output = temp.path().join("gmsh_out");
    let source = r#"$MeshFormat
4.1 0 8
$EndMeshFormat
$Nodes
1 6 1 6
2 1 0 6
1
2
3
4
5
6
0 0 0
1 0 0
1 1 0
0 1 0
2 0 0
2 1 0
$EndNodes
$Elements
1 2 1 2
2 1 3 2
1 1 2 3 4
2 2 5 6 3
$EndElements
$NodeData
1
"Temperature"
1
0.0
3
0
1
6
1 10.0
2 20.0
3 30.0
4 40.0
5 50.0
6 60.0
$EndNodeData
$ElementData
1
"Strain"
1
0.0
3
0
1
2
1 2.0
2 8.0
$EndElementData
"#;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("color map uses cell values"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("colorbar-max"));
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.data().iter().any(|&component| component != 0));
    Ok(())
}

#[test]
fn test_visual_rendering_gmsh_v40_sparse_block_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("mesh_v40.msh");
    let output = temp.path().join("gmsh40_out");
    let source = r#"$MeshFormat
4.0 0 8
$EndMeshFormat
$Nodes
1 4
1 2 0 4
10 0 0 0
20 100 0 0
30 100 100 0
40 0 100 0
$EndNodes
$Elements
1 2
1 2 2 2
100 10 20 30
200 10 30 40
$EndElements
$ElementData
1
"Stress"
1
0.0
3
0
1
2
100 2.0
200 8.0
$EndElementData
"#;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Gmsh MSH 4.0 FEA Mesh: Stress"));
    assert!(svg_text.contains("colorbar-max"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let unique_colors = pixmap
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(unique_colors.len() > 16);
    Ok(())
}

#[test]
fn test_visual_rendering_gmsh_volume_cell_scalar_wireframe()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("volume_cells.msh");
    let output = temp.path().join("volume_cells_out");
    let source = r#"$MeshFormat
2.2 0 8
$EndMeshFormat
$Nodes
5
1 0 0 0
2 100 0 0
3 0 100 0
4 0 0 100
5 100 100 100
$EndNodes
$Elements
2
1 4 0 1 2 3 4
2 4 0 2 3 4 5
$EndElements
$ElementData
1
"Stress"
1
0.0
3
0
1
2
1 0.0
2 10.0
$EndElementData
"#;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("colorbar-max"));
    assert!(svg_text.contains("stroke=\"#000080\""));
    assert!(svg_text.contains("stroke=\"#600000\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let distinct_colors = pixmap
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(distinct_colors.len() > 16);
    Ok(())
}

#[test]
fn test_visual_rendering_open_document_text() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let temp = tempdir()?;
    let input = temp.path().join("report.odt");
    let output = temp.path().join("odt_out");
    let mut package = ZipWriter::new(fs::File::create(&input)?);
    package.start_file("mimetype", SimpleFileOptions::default())?;
    package.write_all(b"application/vnd.oasis.opendocument.text")?;
    package.start_file("content.xml", SimpleFileOptions::default())?;
    package.write_all(
        br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"><office:body><office:text><text:h text:outline-level="1">ODT visual proof</text:h><text:p>Heading, prose, list and table.</text:p><text:list><text:list-item><text:p>First</text:p></text:list-item></text:list><table:table><table:table-header-rows><table:table-row><table:table-cell><text:p>Metric</text:p></table:table-cell><table:table-cell><text:p>Value</text:p></table:table-cell></table:table-row></table:table-header-rows><table:table-row><table:table-cell><text:p>Temperature</text:p></table:table-cell><table:table-cell><text:p>24</text:p></table:table-cell></table:table-row></table:table></office:text></office:body></office:document-content>"#,
    )?;
    package.finish()?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("ODT visual proof"));
    assert!(svg_text.contains("Temperature"));
    assert_eq!(report.page_count, 1);
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.data().chunks_exact(4).any(|pixel| pixel[3] > 0));
    Ok(())
}

#[test]
fn test_visual_rendering_opendocument_text_styles() -> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odt_styles.odt");
    let temp = tempdir()?;
    let output = temp.path().join("odt-styles-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("font-weight=\"700\""));
    assert!(svg_text.contains("font-style=\"italic\""));
    assert!(svg_text.contains("#1D4ED8"));
    assert!(svg_text.contains("#DC2626"));
    assert!(svg_text.contains("#16A34A"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 120 && pixel[0] < 120)
        .count();
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 120 && pixel[1] < 120)
        .count();
    let green_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[1] > 90 && pixel[0] < 120 && pixel[2] < 120)
        .count();
    assert!(
        blue_pixels > 20,
        "only {blue_pixels} ODT blue text pixels rendered"
    );
    assert!(
        red_pixels > 5,
        "only {red_pixels} ODT red text pixels rendered"
    );
    assert!(
        green_pixels > 5,
        "only {green_pixels} ODT green text pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_open_document_text_embedded_image()
-> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/odt_image.odt");
    let temp = tempdir()?;
    let output = temp.path().join("odt_image_out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 80 && pixel[3] > 0)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[2] > 180 && pixel[0] < 80 && pixel[1] < 80 && pixel[3] > 0)
        .count();
    assert!(red_pixels > 0, "embedded ODT red pixels were not rendered");
    assert!(
        blue_pixels > 0,
        "embedded ODT blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_flat_fodt_inline_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("inline-image.fodt");
    let output = temp.path().join("inline-image-out");
    let xml = r#"<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:text><text:p>Before<draw:frame draw:name="inline"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame>After</text:p></office:text></office:body></office:document>"#;
    fs::write(&input, xml)?;
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Odt);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "flat FODT inline image red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "flat FODT inline image blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_epub_package_image() -> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/epub_image.epub");
    let temp = tempdir()?;
    let output = temp.path().join("epub_image_out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 80 && pixel[3] > 0)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[2] > 180 && pixel[0] < 80 && pixel[1] < 80 && pixel[3] > 0)
        .count();
    assert!(red_pixels > 0, "embedded EPUB red pixels were not rendered");
    assert!(
        blue_pixels > 0,
        "embedded EPUB blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_ods_package_image() -> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ods_image.ods");
    let temp = tempdir()?;
    let output = temp.path().join("ods_image_out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 80 && pixel[3] > 0)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[2] > 180 && pixel[0] < 80 && pixel[1] < 80 && pixel[3] > 0)
        .count();
    assert!(red_pixels > 0, "embedded ODS red pixels were not rendered");
    assert!(
        blue_pixels > 0,
        "embedded ODS blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_flat_fods_inline_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("inline.fods");
    let output = temp.path().join("inline-fods-out");
    let xml = r#"<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:spreadsheet><table:table table:name="Sheet"><table:table-row><table:table-cell><text:p>Cell</text:p><draw:frame draw:name="inline"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document>"#;
    fs::write(&input, xml)?;
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ods);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "flat FODS inline image red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "flat FODS inline image blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_local_html_image() -> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local_image.html");
    let temp = tempdir()?;
    let output = temp.path().join("html_image_out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 80 && pixel[3] > 0)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[2] > 180 && pixel[0] < 80 && pixel[1] < 80 && pixel[3] > 0)
        .count();
    assert!(red_pixels > 0, "local HTML red pixels were not rendered");
    assert!(blue_pixels > 0, "local HTML blue pixels were not rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_markdown_local_image() -> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/markdown_image.md");
    let temp = tempdir()?;
    let output = temp.path().join("markdown_image_out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 80 && pixel[3] > 0)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[2] > 180 && pixel[0] < 80 && pixel[1] < 80 && pixel[3] > 0)
        .count();
    assert!(
        red_pixels > 0,
        "local Markdown red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "local Markdown blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_3mf_build_item_transform() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("transformed.3mf");
    let output = temp.path().join("3mf_out");
    let mut archive = ZipWriter::new(fs::File::create(&input)?);
    archive.start_file("3D/3dmodel.model", SimpleFileOptions::default())?;
    archive.write_all(
        br#"<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02"><resources>
<object id="1"><mesh><vertices><vertex x="0" y="0" z="0"/><vertex x="10" y="0" z="0"/><vertex x="0" y="10" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
<object id="2"><mesh><vertices><vertex x="20" y="0" z="0"/><vertex x="30" y="0" z="0"/><vertex x="20" y="10" z="0"/></vertices><triangles><triangle v1="0" v2="1" v3="2"/></triangles></mesh></object>
</resources><build><item objectid="2" transform="2 0 0 0 1 0 0 0 1 5 0 0"/></build></model>"#,
    )?;
    archive.finish()?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("id=\"face_0\""));
    assert!(!svg_text.contains("id=\"face_1\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 240 || pixel[1] < 240 || pixel[2] < 240))
        .count();
    assert!(
        visible_pixels > 500,
        "transformed 3MF build item was not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_glb_mesh_instances() -> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triangle.glb");
    let temp = tempdir()?;
    let output = temp.path().join("gltf_out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 240 || pixel[1] < 240 || pixel[2] < 240))
        .count();
    assert!(
        visible_pixels > 500,
        "glTF triangle instances were not visibly rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_rtf_png_picture() -> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rtf_image.rtf");
    let temp = tempdir()?;
    let output = temp.path().join("rtf_picture_out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 80 && pixel[3] > 0)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[2] > 180 && pixel[0] < 80 && pixel[1] < 80 && pixel[3] > 0)
        .count();
    assert!(red_pixels > 0, "RTF embedded red pixels were not rendered");
    assert!(
        blue_pixels > 0,
        "RTF embedded blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_open_document_spreadsheet() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let temp = tempdir()?;
    let input = temp.path().join("metrics.ods");
    let output = temp.path().join("ods_out");
    let mut package = ZipWriter::new(fs::File::create(&input)?);
    package.start_file("mimetype", SimpleFileOptions::default())?;
    package.write_all(b"application/vnd.oasis.opendocument.spreadsheet")?;
    package.start_file("content.xml", SimpleFileOptions::default())?;
    package.write_all(
        br#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:spreadsheet><table:table table:name="Service Metrics"><table:table-header-rows><table:table-row><table:table-cell><text:p>Metric</text:p></table:table-cell><table:table-cell><text:p>Value</text:p></table:table-cell></table:table-row></table:table-header-rows><table:table-row><table:table-cell><text:p>Requests</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="1200"><text:p>1200</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#,
    )?;
    package.finish()?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert_eq!(report.source_format, document_svg::SourceFormat::Ods);
    assert!(svg_text.contains("Service Metrics"));
    assert!(svg_text.contains("Requests"));
    assert!(svg_text.contains("1200"));
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.data().iter().any(|&component| component != 0));
    Ok(())
}

#[test]
fn test_visual_rendering_open_document_presentation() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let temp = tempdir()?;
    let input = temp.path().join("slides.fodp");
    let output = temp.path().join("slides_out");
    let source = r##"<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
 xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"
 xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0">
 <office:automatic-styles>
  <style:page-layout style:name="wide"><style:page-layout-properties fo:page-width="20cm" fo:page-height="11.25cm"/></style:page-layout>
  <style:style style:name="accent" style:family="graphic">
   <style:graphic-properties draw:fill="solid" draw:fill-color="#1D4ED8" draw:stroke="solid" svg:stroke-color="#0F172A" svg:stroke-width="1pt"/>
   <style:text-properties fo:font-size="18pt" fo:font-weight="bold" fo:color="#FFFFFF"/>
  </style:style>
 </office:automatic-styles>
 <office:body><office:presentation>
  <draw:page draw:name="Overview">
   <draw:rect draw:name="title-box" draw:style-name="accent" svg:x="1cm" svg:y="2cm" svg:width="12cm" svg:height="3cm">
    <text:p>Slide one: <text:span>editable title</text:span></text:p>
   </draw:rect>
   <draw:ellipse svg:x="15cm" svg:y="2cm" svg:width="3cm" svg:height="3cm" draw:fill-color="#F97316"/>
  </draw:page>
  <draw:page draw:name="Closing"><draw:line svg:x1="1cm" svg:y1="2cm" svg:x2="18cm" svg:y2="9cm"/></draw:page>
 </office:presentation></office:body>
</office:document>"##;
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Odp);
    assert_eq!(report.page_count, 2);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("ODP text wrapping"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Slide one: editable title"));
    assert!(svg_text.contains("#1D4ED8"));
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    let distinct_colors = pixmap
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct_colors.len() > 12,
        "ODP shapes and text should render visibly"
    );
    let second_page = fs::read(output.join("page-0002.svg"))?;
    assert!(std::str::from_utf8(&second_page)?.contains("M 28.346 56.693 L 510.236 255.118"));

    let package_path = temp.path().join("slides.odp");
    let package_output = temp.path().join("slides_package_out");
    let mut package = ZipWriter::new(fs::File::create(&package_path)?);
    package.start_file("mimetype", SimpleFileOptions::default())?;
    package.write_all(b"application/vnd.oasis.opendocument.presentation")?;
    package.start_file("content.xml", SimpleFileOptions::default())?;
    package.write_all(source.as_bytes())?;
    package.start_file("styles.xml", SimpleFileOptions::default())?;
    package.write_all(source.as_bytes())?;
    package.finish()?;
    let package_report = convert_path(&package_path, &package_output, &ConvertOptions::default())?;
    assert_eq!(package_report.source_format, SourceFormat::Odp);
    assert_eq!(package_report.page_count, 2);
    assert!(
        fs::read(package_output.join("page-0001.svg"))?
            .windows("editable title".len())
            .any(|window| window == b"editable title")
    );
    Ok(())
}

#[test]
fn test_visual_rendering_odp_embedded_raster_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("embedded-image.odp");
    let output = temp.path().join("odp_embedded_image_out");
    fs::write(&input, common::odp_presentation_with_embedded_image())?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Odp);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("data:image/png;base64,"));
    assert!(!svg_text.contains("ODP embedded images"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 150 && pixel[1] < 80 && pixel[2] < 80)
        .count();
    assert!(
        red_pixels > 100,
        "only {red_pixels} embedded image pixels rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_flat_fodp_inline_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("inline.fodp");
    let output = temp.path().join("inline-fodp-out");
    let xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:presentation><draw:page draw:name="Inline"><draw:frame draw:name="image" svg:x="1cm" svg:y="1cm" svg:width="4cm" svg:height="2cm"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame></draw:page></office:presentation></office:body></office:document-content>"#;
    fs::write(&input, xml)?;
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Odp);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "flat FODP inline image red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "flat FODP inline image blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_open_document_drawing() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let temp = tempdir()?;
    let flat_input = temp.path().join("drawing.fodg");
    let flat_output = temp.path().join("drawing_flat_out");
    let source = r##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
 xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0"
 xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
 xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0"
 xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0"
 xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0">
 <office:automatic-styles>
  <style:page-layout style:name="drawing-page"><style:page-layout-properties fo:page-width="21cm" fo:page-height="14cm"/></style:page-layout>
  <style:style style:name="base" style:family="graphic"><style:graphic-properties draw:fill="solid" draw:fill-color="#F97316" draw:stroke="solid" svg:stroke-color="#7C2D12" svg:stroke-width="1pt"/></style:style>
 </office:automatic-styles>
 <office:body><office:drawing>
  <draw:page draw:name="Diagram page">
   <draw:rect draw:style-name="base" svg:x="1cm" svg:y="1cm" svg:width="8cm" svg:height="4cm"><text:p>Editable drawing label</text:p></draw:rect>
   <draw:ellipse svg:x="11cm" svg:y="2cm" svg:width="3cm" svg:height="3cm" draw:fill-color="#2563EB"/>
  </draw:page>
 </office:drawing></office:body>
</office:document-content>"##;
    fs::write(&flat_input, source)?;

    let flat_report = convert_path(&flat_input, &flat_output, &ConvertOptions::default())?;
    assert_eq!(flat_report.source_format, SourceFormat::Odg);
    assert_eq!(flat_report.page_count, 1);
    let flat_svg = fs::read(flat_output.join("page-0001.svg"))?;
    let flat_text = std::str::from_utf8(&flat_svg)?;
    assert!(flat_text.contains("Editable drawing label"));
    assert!(flat_text.contains("#F97316"));
    let flat_pixmap = render_svg_with_resvg(&flat_svg)?;
    assert!(flat_pixmap.data().chunks_exact(4).any(|pixel| pixel[3] > 0));

    let package_input = temp.path().join("drawing.odg");
    let package_output = temp.path().join("drawing_package_out");
    let mut package = ZipWriter::new(fs::File::create(&package_input)?);
    package.start_file("mimetype", SimpleFileOptions::default())?;
    package.write_all(b"application/vnd.oasis.opendocument.graphics")?;
    package.start_file("content.xml", SimpleFileOptions::default())?;
    package.write_all(source.as_bytes())?;
    package.finish()?;
    let package_report = convert_path(&package_input, &package_output, &ConvertOptions::default())?;
    assert_eq!(package_report.source_format, SourceFormat::Odg);
    assert_eq!(package_report.page_count, 1);
    assert!(fs::read_to_string(package_output.join("page-0001.svg"))?.contains("Diagram page"));
    Ok(())
}

#[test]
fn test_visual_rendering_flat_fodg_inline_image() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("inline.fodg");
    let output = temp.path().join("inline-fodg-out");
    let xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:drawing><draw:page draw:name="Inline"><draw:frame draw:name="image" svg:x="1cm" svg:y="1cm" svg:width="4cm" svg:height="2cm"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame></draw:page></office:drawing></office:body></office:document-content>"#;
    fs::write(&input, xml)?;
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Odg);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "flat FODG inline image red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "flat FODG inline image blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_extended_chart_types() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    // 1. Doughnut chart
    let donut_path = temp.path().join("browser_share.chart.json");
    let donut_out = temp.path().join("donut_out");
    let donut_json = r#"{
        "type": "doughnut",
        "title": "Browser Distribution",
        "labels": ["Chrome", "Safari", "Firefox", "Edge"],
        "series": [
            {"name": "Market Share", "data": [65.0, 19.0, 8.0, 8.0]}
        ]
    }"#;
    fs::write(&donut_path, donut_json)?;
    let report_donut = convert_path(&donut_path, &donut_out, &ConvertOptions::default())?;
    assert_eq!(report_donut.page_count, 1);
    let donut_svg = fs::read(donut_out.join("page-0001.svg"))?;
    let donut_pixmap = render_svg_with_resvg(&donut_svg)?;
    assert!(donut_pixmap.width() > 0 && donut_pixmap.height() > 0);
    assert!(donut_pixmap.data().iter().any(|&b| b > 0));

    // 2. Area chart
    let area_path = temp.path().join("bandwidth.chart.json");
    let area_out = temp.path().join("area_out");
    let area_json = r#"{
        "type": "area",
        "title": "Bandwidth Usage",
        "labels": ["00:00", "06:00", "12:00", "18:00"],
        "series": [
            {"name": "Throughput Gbps", "data": [12.5, 45.2, 88.7, 52.1]}
        ]
    }"#;
    fs::write(&area_path, area_json)?;
    let report_area = convert_path(&area_path, &area_out, &ConvertOptions::default())?;
    assert_eq!(report_area.page_count, 1);
    let area_svg = fs::read(area_out.join("page-0001.svg"))?;
    let area_str = std::str::from_utf8(&area_svg)?;
    assert!(area_str.contains("Bandwidth Usage"));
    let area_pixmap = render_svg_with_resvg(&area_svg)?;
    assert!(area_pixmap.width() > 0 && area_pixmap.height() > 0);

    // 3. Scatter chart
    let scatter_path = temp.path().join("clusters.chart.json");
    let scatter_out = temp.path().join("scatter_out");
    let scatter_json = r#"{
        "type": "scatter",
        "title": "Experimental Clusters",
        "labels": ["P1", "P2", "P3", "P4"],
        "series": [
            {"name": "Cluster Alpha", "data": [10.0, 30.0, 20.0, 45.0]}
        ]
    }"#;
    fs::write(&scatter_path, scatter_json)?;
    let report_scatter = convert_path(&scatter_path, &scatter_out, &ConvertOptions::default())?;
    assert_eq!(report_scatter.page_count, 1);
    let scatter_svg = fs::read(scatter_out.join("page-0001.svg"))?;
    let scatter_pixmap = render_svg_with_resvg(&scatter_svg)?;
    assert!(scatter_pixmap.width() > 0 && scatter_pixmap.height() > 0);

    Ok(())
}

fn create_1bpp_bmp(width: usize, height: usize, bits: &[u8]) -> Vec<u8> {
    let row_stride = (width.div_ceil(8) + 3) & !3;
    let pixel_data_size = row_stride * height;
    let file_size = 14 + 40 + 8 + pixel_data_size;
    let mut bmp = Vec::with_capacity(file_size);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&[0, 0, 0, 0]);
    bmp.extend_from_slice(&(14u32 + 40 + 8).to_le_bytes());

    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(width as i32).to_le_bytes());
    bmp.extend_from_slice(&(height as i32).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&(pixel_data_size as u32).to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&2u32.to_le_bytes());
    bmp.extend_from_slice(&2u32.to_le_bytes());

    // Palette: 0 = black (0,0,0), 1 = white (255,255,255)
    bmp.extend_from_slice(&[0, 0, 0, 0]);
    bmp.extend_from_slice(&[255, 255, 255, 0]);

    for row in 0..height {
        let mut row_bytes = vec![0u8; row_stride];
        for col in 0..width {
            let bit = bits.get(row * width + col).copied().unwrap_or(0);
            if bit != 0 {
                row_bytes[col / 8] |= 1 << (7 - (col % 8));
            }
        }
        bmp.extend_from_slice(&row_bytes);
    }
    bmp
}

#[test]
fn test_visual_rendering_bmp_palette_and_monochrome() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;

    // 1. 1-bit monochrome scanned line drawing (8x8 checkboard pattern)
    let bmp_1bit_path = temp.path().join("drawing_1bit.bmp");
    let bmp_1bit_out = temp.path().join("bmp_1bit_out");
    let mut bits = vec![0u8; 64];
    // Fill diagonal pattern with 1s (white) and 0s (black)
    for y in 0..8 {
        for x in 0..8 {
            if (x + y) % 2 == 0 {
                bits[y * 8 + x] = 1;
            }
        }
    }
    let bmp_bytes = create_1bpp_bmp(8, 8, &bits);
    fs::write(&bmp_1bit_path, bmp_bytes)?;

    let report_1bit = convert_path(&bmp_1bit_path, &bmp_1bit_out, &ConvertOptions::default())?;
    assert_eq!(report_1bit.page_count, 1);
    let svg_bytes = fs::read(bmp_1bit_out.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;
    assert!(svg_str.contains("<svg"));
    assert!(svg_str.contains("vectorized_path"));

    assert_eq!(report_1bit.pages[0].width_points, 8.0);
    assert_eq!(report_1bit.pages[0].height_points, 8.0);

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_webp_raster() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("sample.webp");
    let output = temp.path().join("webp_out");
    fs::write(&input, common::webp_sample())?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Raster);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("vectorized_path"));
    let pixmap = render_svg_with_resvg(&svg)?;
    let black_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 32 && pixel[1] < 32 && pixel[2] < 32)
        .count();
    assert!(black_pixels > 0, "no dark WebP image pixels were rendered");
    Ok(())
}

#[test]
fn test_visual_rendering_animated_gif_first_frame() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("animation.gif");
    let output = temp.path().join("gif_out");
    fs::write(&input, common::animated_gif_sample())?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Raster);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let black_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] < 32 && pixel[1] < 32 && pixel[2] < 32)
        .count();
    assert!(
        black_pixels > 0,
        "the first GIF frame rendered no dark pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_cmyk_jpeg_approximation() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("cmyk.jpg");
    let output = temp.path().join("cmyk_jpeg_out");
    fs::write(&input, common::cmyk_jpeg_sample())?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("CMYK JPEG"))
    );
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(
        pixmap
            .data()
            .chunks_exact(4)
            .any(|pixel| { pixel[3] > 0 && pixel[0] < 32 && pixel[1] < 32 && pixel[2] < 32 }),
        "CMYK JPEG should produce vectorized dark pixels"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_mermaid_chained_edges_and_shapes() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let mmd_path = temp.path().join("pipeline.mmd");
    let out_dir = temp.path().join("pipeline_out");

    let mmd_content = r#"flowchart LR
    A["Raw Input"] -->|parse| B["Syntax Tree"] -->|optimize| C["Intermediate IR"] --> D["Target Code"]
    S[["Compiler Subroutine"]] --> F>Emitted Flag] --> E(((Output Binary)))
"#;
    fs::write(&mmd_path, mmd_content)?;

    let report = convert_path(&mmd_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Verify all chained nodes exist
    assert!(svg_str.contains("Raw Input"));
    assert!(svg_str.contains("Syntax Tree"));
    assert!(svg_str.contains("Intermediate IR"));
    assert!(svg_str.contains("Target Code"));
    assert!(svg_str.contains("Compiler Subroutine"));
    assert!(svg_str.contains("Emitted Flag"));
    assert!(svg_str.contains("Output Binary"));

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_plantuml_architectural_shapes() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let puml_path = temp.path().join("architecture.puml");
    let out_dir = temp.path().join("arch_out");

    let puml_content = r#"@startuml
actor "Customer Client" as User
rectangle "API Gateway & Router" as GW
cloud "AWS Cloud VPC" as Cloud
database "PostgreSQL Cluster" as DB
storage "Asset Storage S3" as S3

User -> GW : HTTPS Request
GW -> Cloud : Forward
Cloud -> DB : Query SQL
Cloud -> S3 : Fetch Object
@enduml
"#;
    fs::write(&puml_path, puml_content)?;

    let report = convert_path(&puml_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    assert!(svg_str.contains("Customer Client"));
    assert!(
        svg_str.contains("API Gateway &amp; Router") || svg_str.contains("API Gateway & Router")
    );
    assert!(svg_str.contains("AWS Cloud VPC"));
    assert!(svg_str.contains("PostgreSQL Cluster"));
    assert!(svg_str.contains("Asset Storage S3"));

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_html_nested_lists_and_dl() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let html_path = temp.path().join("nested.html");
    let out_dir = temp.path().join("nested_out");

    let html_content = r#"<!DOCTYPE html>
<html>
<body>
  <h1>Specification Overview</h1>
  <ol>
    <li>Core Protocols
      <ol start="10">
        <li>Transport Layer</li>
        <li>Network Layer</li>
      </ol>
    </li>
    <li>Application Layer</li>
  </ol>
  <dl>
    <dt>SVG</dt>
    <dd>Scalable Vector Graphics format</dd>
    <dt>CAD</dt>
    <dd>Computer Aided Design drawing</dd>
  </dl>
</body>
</html>"#;
    fs::write(&html_path, html_content)?;

    let report = convert_path(&html_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Verify outer numbering resumed after inner list
    assert!(svg_str.contains("1. Core Protocols"));
    assert!(svg_str.contains("10. Transport Layer"));
    assert!(svg_str.contains("11. Network Layer"));
    assert!(svg_str.contains("2. Application Layer"));

    // Verify definition list terms
    assert!(svg_str.contains("SVG"));
    assert!(svg_str.contains("Scalable Vector Graphics format"));
    assert!(svg_str.contains("CAD"));
    assert!(svg_str.contains("Computer Aided Design drawing"));

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_markdown_nested_outlines_and_images()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("outline.md");
    let out_dir = temp.path().join("outline_out");

    let md_content = r#"# Technical Roadmap

- [x] Phase 1: Core Engine
  - [x] Parser Architecture
  - [ ] Memory Management
- [ ] Phase 2: User Experience
  - [ ] Desktop UI
  - [ ] Web Assembly Port

![Architecture Diagram](diagram.png)

Statement with footnote reference[^1]. Autolink at <https://example.com/docs>.

[^1]: Footnote details explaining internal invariants.
"#;
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(out_dir.join("page-0001.svg"))?;
    let svg_str = std::str::from_utf8(&svg_bytes)?;

    // Verify task checkboxes and hierarchy
    assert!(svg_str.contains("☑ Phase 1: Core Engine"));
    assert!(svg_str.contains("☑ Parser Architecture"));
    assert!(svg_str.contains("☐ Memory Management"));
    assert!(svg_str.contains("☐ Phase 2: User Experience"));

    // Verify image alt text without raw exclamation mark prefix
    assert!(svg_str.contains("Architecture Diagram"));
    assert!(!svg_str.contains("!Architecture Diagram"));

    // Verify footnote reference
    assert!(svg_str.contains("[1]"));

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_binary_gmsh_v22_mesh_and_element_data()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("binary_mesh.msh");
    let output = temp.path().join("binary_mesh_out");
    let mut source = b"$MeshFormat\n2.2 1 8\n".to_vec();
    source.extend_from_slice(&1i32.to_le_bytes());
    source.extend_from_slice(b"\n$EndMeshFormat\n$Nodes\n4\n");
    for (tag, x, y) in [
        (1i32, 0.0f64, 0.0f64),
        (2, 100.0, 0.0),
        (3, 100.0, 100.0),
        (4, 0.0, 100.0),
    ] {
        source.extend_from_slice(&tag.to_le_bytes());
        source.extend_from_slice(&x.to_le_bytes());
        source.extend_from_slice(&y.to_le_bytes());
        source.extend_from_slice(&0.0f64.to_le_bytes());
    }
    source.extend_from_slice(b"\n$EndNodes\n$Elements\n2\n");
    for value in [2i32, 2, 2, 1, 7, 1, 1, 2, 3, 2, 7, 1, 1, 3, 4] {
        source.extend_from_slice(&value.to_le_bytes());
    }
    source.extend_from_slice(b"\n$EndElements\n$ElementData\n1\n\"Stress\"\n1\n0.0\n3\n0\n1\n2\n");
    for (tag, value) in [(1i32, 3.0f64), (2, 12.5)] {
        source.extend_from_slice(&tag.to_le_bytes());
        source.extend_from_slice(&value.to_le_bytes());
    }
    source.extend_from_slice(b"\n$EndElementData\n");
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Gmsh MSH 2.2 Binary FEA Mesh: Stress"));
    assert!(svg_text.contains("colorbar-max"));

    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    let distinct_colors = pixmap
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct_colors.len() > 16,
        "binary Gmsh mesh and scalar legend should render visibly"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_binary_gmsh_v41_blocks() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("binary_v41_mesh.msh");
    let output = temp.path().join("binary_v41_mesh_out");
    let mut source = b"$MeshFormat\n4.1 1 8\n".to_vec();
    source.extend_from_slice(&1i32.to_le_bytes());
    source.extend_from_slice(b"\n$EndMeshFormat\n$Nodes\n");
    for value in [1u64, 4, 1, 4] {
        source.extend_from_slice(&value.to_le_bytes());
    }
    for value in [2i32, 1, 0] {
        source.extend_from_slice(&value.to_le_bytes());
    }
    source.extend_from_slice(&4u64.to_le_bytes());
    for tag in 1..=4u64 {
        source.extend_from_slice(&tag.to_le_bytes());
    }
    for [x, y, z] in [
        [0.0f64, 0.0, 0.0],
        [100.0, 0.0, 0.0],
        [100.0, 100.0, 0.0],
        [0.0, 100.0, 0.0],
    ] {
        for value in [x, y, z] {
            source.extend_from_slice(&value.to_le_bytes());
        }
    }
    source.extend_from_slice(b"\n$EndNodes\n$Elements\n");
    for value in [1u64, 2, 1, 2] {
        source.extend_from_slice(&value.to_le_bytes());
    }
    for value in [2i32, 1, 2] {
        source.extend_from_slice(&value.to_le_bytes());
    }
    source.extend_from_slice(&2u64.to_le_bytes());
    for (element, nodes) in [(1u64, [1u64, 2, 3]), (2, [1, 3, 4])] {
        source.extend_from_slice(&element.to_le_bytes());
        for node in nodes {
            source.extend_from_slice(&node.to_le_bytes());
        }
    }
    source.extend_from_slice(b"\n$EndElements\n");
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Simulation);
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Gmsh MSH 4.1 Binary FEA Mesh"));
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    let colors = pixmap
        .data()
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        .collect::<std::collections::HashSet<_>>();
    assert!(
        colors.len() > 16,
        "binary Gmsh 4.1 wireframe should render clearly"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_binary_gmsh_v40_mixed_node_blocks()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("binary_v40_mesh.msh");
    let output = temp.path().join("binary_v40_mesh_out");
    let mut source = b"$MeshFormat\n4.0 1 8\n".to_vec();
    let push_i32 = |bytes: &mut Vec<u8>, value: i32| bytes.extend_from_slice(&value.to_le_bytes());
    let push_ulong =
        |bytes: &mut Vec<u8>, value: u64| bytes.extend_from_slice(&value.to_le_bytes());
    let push_f64 = |bytes: &mut Vec<u8>, value: f64| bytes.extend_from_slice(&value.to_le_bytes());
    push_i32(&mut source, 1);
    source.extend_from_slice(b"\n$EndMeshFormat\n$Entities\n");
    for count in [1u64, 0, 1, 0] {
        push_ulong(&mut source, count);
    }
    push_i32(&mut source, 1);
    for value in [0.0, 0.0, 0.0, 0.0, 0.0, 0.0] {
        push_f64(&mut source, value);
    }
    push_ulong(&mut source, 0);
    push_i32(&mut source, 1);
    for value in [0.0, 0.0, 0.0, 100.0, 100.0, 0.0] {
        push_f64(&mut source, value);
    }
    push_ulong(&mut source, 0);
    push_ulong(&mut source, 0);
    source.extend_from_slice(b"\n$EndEntities\n$Nodes\n");
    push_ulong(&mut source, 1);
    push_ulong(&mut source, 4);
    for value in [1, 2, 0] {
        push_i32(&mut source, value);
    }
    push_ulong(&mut source, 4);
    for (tag, [x, y, z]) in [
        (1, [0.0, 0.0, 0.0]),
        (2, [100.0, 0.0, 0.0]),
        (3, [100.0, 100.0, 0.0]),
        (4, [0.0, 100.0, 0.0]),
    ] {
        push_i32(&mut source, tag);
        for coordinate in [x, y, z] {
            push_f64(&mut source, coordinate);
        }
    }
    source.extend_from_slice(b"\n$EndNodes\n$Elements\n");
    push_ulong(&mut source, 1);
    push_ulong(&mut source, 2);
    for value in [1, 2, 2] {
        push_i32(&mut source, value);
    }
    push_ulong(&mut source, 2);
    for (tag, [first, second, third]) in [(1, [1, 2, 3]), (2, [1, 3, 4])] {
        for value in [tag, first, second, third] {
            push_i32(&mut source, value);
        }
    }
    source.extend_from_slice(b"\n$EndElements\n");
    fs::write(&input, source)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(report.warnings.is_empty());
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Gmsh MSH 4.0 Binary FEA Mesh"));
    assert!(svg_text.contains("data-semantic-role=\"simulation:mesh\""));
    let pixmap = render_svg_with_resvg(&svg)?;
    let visible_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 240 || pixel[1] < 240 || pixel[2] < 240))
        .count();
    assert!(
        visible_pixels > 500,
        "binary Gmsh 4.0 mesh should render clearly"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_complete_latex_document_with_local_image()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("sample.tex");
    let assets = temp.path().join("assets");
    fs::create_dir_all(&assets)?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_latex.tex"),
        &input,
    )?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/assets/red-blue.png"),
        assets.join("red-blue.png"),
    )?;
    let output = temp.path().join("latex-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Tex);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "complete LaTeX image red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "complete LaTeX image blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_fictionbook_embedded_png() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fb2");
    let output = temp.path().join("fb2-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Fb2);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    let red_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 180 && pixel[1] < 100 && pixel[2] < 100)
        .count();
    let blue_pixels = pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[2] > 140 && pixel[0] < 120 && pixel[1] < 160)
        .count();
    assert!(
        red_pixels > 0,
        "FictionBook image red pixels were not rendered"
    );
    assert!(
        blue_pixels > 0,
        "FictionBook image blue pixels were not rendered"
    );
    Ok(())
}

#[test]
fn test_visual_rendering_mobi_palmdoc_text() -> Result<(), Box<dyn std::error::Error>> {
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mobi");
    let temp = tempdir()?;
    let output = temp.path().join("mobi-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mobi);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let pixmap = render_svg_with_resvg(&svg)?;
    assert!(pixmap.data().iter().any(|byte| *byte > 0));
    let compressed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_mobi_compressed.mobi");
    let compressed_output = temp.path().join("mobi-compressed-out");
    let compressed_report =
        convert_path(&compressed, &compressed_output, &ConvertOptions::default())?;
    assert_eq!(compressed_report.source_format, SourceFormat::Mobi);
    let compressed_svg = fs::read(compressed_output.join("page-0001.svg"))?;
    let compressed_pixmap = render_svg_with_resvg(&compressed_svg)?;
    assert!(compressed_pixmap.data().iter().any(|byte| *byte > 0));
    Ok(())
}

#[test]
fn test_visual_rendering_latex_math_matrix_and_cases() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("formula.tex");
    let output = temp.path().join("math_out");

    let math_content = r"\begin{pmatrix} 1 & 0 \\ 0 & 1 \end{pmatrix} + \begin{cases} x^2 & x \ge 0 \\ -x & x < 0 \end{cases}";
    fs::write(&input, math_content)?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_bytes = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg_bytes)?;
    assert!(svg_text.contains("<svg"));
    assert!(svg_text.contains("<path"));
    assert!(svg_text.contains("<text"));

    let pixmap = render_svg_with_resvg(&svg_bytes)?;
    assert!(pixmap.width() > 0 && pixmap.height() > 0);
    assert!(pixmap.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn test_visual_rendering_cad_gerber_and_obj_enhancements() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;

    // 1. Gerber with Obround and Polygon pads
    let gerber_input = temp.path().join("board.gbr");
    let gerber_output = temp.path().join("gbr_out");
    let gerber_content = r#"%FSLAX24Y24*%
%MOMM*%
G04 Layer Top Copper Altium Export*
%ADD10O,3.0000X1.5000X0.6000*%
%ADD11P,2.5000X6X30.0000X0.8000*%
D10*
X010000Y010000D03*
D11*
X025000Y025000D03*
M02*
"#;
    fs::write(&gerber_input, gerber_content)?;
    let rep = convert_path(&gerber_input, &gerber_output, &ConvertOptions::default())?;
    assert_eq!(rep.page_count, 1);
    let gbr_svg = fs::read(gerber_output.join("page-0001.svg"))?;
    let gbr_pix = render_svg_with_resvg(&gbr_svg)?;
    assert!(gbr_pix.width() > 0 && gbr_pix.height() > 0);
    assert!(gbr_pix.data().iter().any(|&b| b > 0));

    // 2. OBJ with negative relative indices and non-UTF-8 comment
    let obj_input = temp.path().join("mesh.obj");
    let obj_output = temp.path().join("obj_out");
    let mut obj_bytes = Vec::new();
    obj_bytes.extend_from_slice(b"# Sample model with \xB0C symbols\n");
    obj_bytes.extend_from_slice(b"v 0 0 0\nv 10 0 0\nv 10 10 0\nv 0 10 0\n");
    obj_bytes.extend_from_slice(b"f -4 -3 -2 -1\n");
    fs::write(&obj_input, obj_bytes)?;
    let rep_obj = convert_path(&obj_input, &obj_output, &ConvertOptions::default())?;
    assert_eq!(rep_obj.page_count, 1);
    let obj_svg = fs::read(obj_output.join("page-0001.svg"))?;
    let obj_pix = render_svg_with_resvg(&obj_svg)?;
    assert!(obj_pix.width() > 0 && obj_pix.height() > 0);
    assert!(obj_pix.data().iter().any(|&b| b > 0));

    // 3. STL with BOM and umlauts
    let stl_input = temp.path().join("part.stl");
    let stl_output = temp.path().join("stl_out");
    let mut stl_bytes = Vec::new();
    stl_bytes.extend_from_slice(b"\xEF\xBB\xBFsolid Geh\xE4use\n");
    stl_bytes.extend_from_slice(b"  facet normal 0 0 1\n    outer loop\n");
    stl_bytes.extend_from_slice(b"      vertex 0 0 0\n      vertex 20 0 0\n      vertex 10 15 0\n");
    stl_bytes.extend_from_slice(b"    endloop\n  endfacet\nendsolid Geh\xE4use\n");
    fs::write(&stl_input, stl_bytes)?;
    let rep_stl = convert_path(&stl_input, &stl_output, &ConvertOptions::default())?;
    assert_eq!(rep_stl.page_count, 1);
    let stl_svg = fs::read(stl_output.join("page-0001.svg"))?;
    let stl_pix = render_svg_with_resvg(&stl_svg)?;
    assert!(stl_pix.width() > 0 && stl_pix.height() > 0);
    assert!(stl_pix.data().iter().any(|&b| b > 0));

    Ok(())
}

#[test]
fn subtitle_srt_and_webvtt_render_as_visible_timestamped_text()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.vtt");
    let output = temp.path().join("subtitle-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Vtt);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("00:00:01.000–00:00:03.000"));
    assert!(svg_text.contains("Narrator: Welcome aboard."));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 100,
        "subtitle page should render readable text"
    );
    Ok(())
}

#[test]
fn ttml_text_profile_renders_visible_cues_with_timecodes() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ttml");
    let output = temp.path().join("ttml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ttml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("00:00:01.250–00:00:03.500"));
    assert!(svg_text.contains("Welcome aboard."));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "TTML subtitle page should render visible text"
    );
    Ok(())
}

#[test]
fn xliff_source_and_target_pairs_render_as_visible_translation_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xliff");
    let output = temp.path().join("xliff-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xliff);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("Source (en): Hello"));
    assert!(svg_text.contains("Target (ja):"));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "XLIFF page should render visible localization text"
    );
    Ok(())
}

#[test]
fn unv_shell_mesh_renders_visible_geometry() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.unv");
    let output = temp.path().join("unv-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Unv);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("UNV Universal FEA mesh"));
    assert!(svg_text.contains("data-source-format=\"unv\""));
    let rendered = render_svg_with_resvg(&svg)?;
    let outline_pixels = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && pixel[0] > 100 && pixel[1] > 110 && pixel[2] > 125)
        .count();
    assert!(
        outline_pixels > 100,
        "UNV mesh should render visible shell edges"
    );
    Ok(())
}

#[test]
fn general_json_preview_renders_visible_path_value_rows() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.json");
    let output = temp.path().join("json-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Json);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("$.service.ports[0] (number): 8080"));
    assert!(svg_text.contains("$.message (string):"));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JSON path/value preview should render visible text"
    );
    Ok(())
}

#[test]
fn strict_ooxml_word_spreadsheet_and_presentation_pages_render_visible_content()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (filename, expected_format, expected_pages, expected_text) in [
        (
            "sample_strict.docx",
            SourceFormat::Docx,
            2,
            "document-svg sample",
        ),
        (
            "sample_strict.xlsx",
            SourceFormat::Xlsx,
            4,
            "Quarterly sales by region",
        ),
        (
            "sample_strict.pptx",
            SourceFormat::Pptx,
            2,
            "PDF and Office documents as self-contained SVG pages",
        ),
    ] {
        let input = fixture_dir.join(filename);
        let output = temp.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.source_format, expected_format, "{filename}");
        assert_eq!(report.page_count, expected_pages, "{filename}");
        assert!(
            report.warnings.is_empty(),
            "{filename}: {:?}",
            report.warnings
        );
        let svg = fs::read(output.join("page-0001.svg"))?;
        let svg_text = std::str::from_utf8(&svg)?;
        assert!(svg_text.contains(expected_text), "{filename}");
        let rendered = render_svg_with_resvg(&svg)?;
        let visible_ink = rendered
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
            .count();
        assert!(
            visible_ink > 500,
            "{filename} did not render visible content"
        );
    }
    Ok(())
}

#[test]
fn microsoft_project_xml_renders_a_visible_gantt_timeline() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.project.xml");
    let output = temp.path().join("project-xml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::ProjectXml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let svg_text = std::str::from_utf8(&svg)?;
    assert!(svg_text.contains("2026-09-01"));
    assert!(svg_text.contains("office:project-dependency-arrow"));
    assert!(svg_text.contains("office:project-milestone"));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Gantt schedule should render visible rows and bars"
    );
    Ok(())
}

#[test]
fn arff_dataset_renders_visible_table_rows() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("dataset.arff");
    fs::write(
        &input,
        "@relation weather\n@attribute outlook {sunny,overcast,rainy}\n@attribute temperature numeric\n@attribute play {yes,no}\n@data\nsunny,25,yes\n{1 18, 2 no}\n",
    )?;
    let output = temp.path().join("arff-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Arff);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "ARFF table should render visible rows");
    Ok(())
}

#[test]
fn jsonld_named_graph_renders_visible_statement_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonld");
    let output = temp.path().join("jsonld-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::JsonLd);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JSON-LD statement table should render visible rows"
    );
    Ok(())
}

#[test]
fn graphml_nodes_and_edges_render_as_visible_diagram() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.graphml");
    let output = temp.path().join("graphml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Graphml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "GraphML diagram should render visible nodes"
    );
    Ok(())
}

#[test]
fn gexf_nodes_and_edges_render_as_visible_diagram() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gexf");
    let output = temp.path().join("gexf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Gexf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "GEXF diagram should render visible nodes"
    );
    Ok(())
}

#[test]
fn netcdf_classic_renders_visible_dimensions_and_values() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_netcdf.nc");
    let output = temp.path().join("netcdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Netcdf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "NetCDF table should render visible values"
    );
    Ok(())
}

#[test]
fn xgmml_nodes_and_edges_render_as_visible_diagram() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xgmml");
    let output = temp.path().join("xgmml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xgmml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "XGMML diagram should render visible nodes"
    );
    Ok(())
}

#[test]
fn graph_gml_nodes_and_edges_render_as_visible_diagram() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_graph.gml");
    let output = temp.path().join("graph-gml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::GraphGml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Graph GML diagram should render visible nodes"
    );
    Ok(())
}

#[test]
fn standalone_jpeg2000_rgb_renders_visible_png_backed_image()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_jpeg2000_rgb.jp2");
    let output = temp.path().join("jp2-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Jpeg2000);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(std::str::from_utf8(&svg)?.contains("data:image/png;base64,"));
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 245 || pixel[1] < 245 || pixel[2] < 245))
        .count();
    assert!(
        visible_ink > 10,
        "standalone JPEG 2000 should render visible pixels"
    );
    Ok(())
}

#[test]
fn tecplot_finite_element_zone_renders_visible_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_tecplot.dat");
    let output = temp.path().join("tecplot-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Tecplot);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Tecplot mesh should render visible geometry"
    );
    Ok(())
}

#[test]
fn ensight_gold_case_renders_visible_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_ensight.case");
    let output = temp.path().join("ensight-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ensight);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "EnSight mesh should render visible geometry"
    );
    Ok(())
}

#[test]
fn plot3d_ascii_grid_renders_visible_structured_cells() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_plot3d.p3d");
    let output = temp.path().join("plot3d-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Plot3d);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "PLOT3D grid should render visible geometry"
    );
    Ok(())
}

#[test]
fn vrml97_indexed_mesh_renders_visible_faces() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wrl");
    let output = temp.path().join("vrml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Vrml);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "VRML mesh should render visible geometry"
    );
    Ok(())
}

#[test]
fn netpbm_raster_variants_render_visible_ink() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    for filename in [
        "sample_rgb.ppm",
        "sample_gray16.pgm",
        "sample_alpha.pam",
        "sample_bitmap.pbm",
    ] {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(filename);
        let output = temp.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.source_format, SourceFormat::Raster);
        assert_eq!(report.page_count, 1);
        let svg = fs::read(output.join("page-0001.svg"))?;
        let rendered = render_svg_with_resvg(&svg)?;
        let visible_ink = rendered
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
            .count();
        assert!(visible_ink > 2, "{filename} should render visible pixels");
    }
    Ok(())
}

#[test]
fn sylk_spreadsheet_renders_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.slk");
    let output = temp.path().join("sylk-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sylk);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "SYLK table should render visible cells"
    );
    Ok(())
}

#[test]
fn dif_spreadsheet_renders_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dif");
    let output = temp.path().join("dif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Dif);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "DIF table should render visible cells");
    Ok(())
}

#[test]
fn fasta_and_fastq_tables_render_visible_records() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    for (filename, format) in [
        ("sample.fasta", SourceFormat::Fasta),
        ("sample.fastq", SourceFormat::Fastq),
    ] {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(filename);
        let output = temp.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.source_format, format);
        assert_eq!(report.page_count, 1);
        let svg = fs::read(output.join("page-0001.svg"))?;
        let rendered = render_svg_with_resvg(&svg)?;
        let visible_ink = rendered
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
            .count();
        assert!(
            visible_ink > 1_000,
            "{filename} should render visible sequence rows"
        );
    }
    Ok(())
}

#[test]
fn gff3_and_gtf_annotations_render_visible_tables() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    for (filename, format) in [
        ("sample.gff3", SourceFormat::Gff3),
        ("sample.gtf", SourceFormat::Gtf),
    ] {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(filename);
        let output = temp.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.source_format, format);
        assert_eq!(report.page_count, 1);
        let svg = fs::read(output.join("page-0001.svg"))?;
        let rendered = render_svg_with_resvg(&svg)?;
        let visible_ink = rendered
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
            .count();
        assert!(
            visible_ink > 1_000,
            "{filename} should render visible feature rows"
        );
    }
    Ok(())
}

#[test]
fn bed_and_bedgraph_render_visible_interval_tables() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    for (filename, format) in [
        ("sample.bed", SourceFormat::Bed),
        ("sample.bedgraph", SourceFormat::BedGraph),
    ] {
        let input = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(filename);
        let output = temp.path().join(filename);
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.source_format, format);
        assert_eq!(report.page_count, 1);
        let svg = fs::read(output.join("page-0001.svg"))?;
        let rendered = render_svg_with_resvg(&svg)?;
        let visible_ink = rendered
            .data()
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
            .count();
        assert!(
            visible_ink > 1_000,
            "{filename} should render visible interval rows"
        );
    }
    Ok(())
}

#[test]
fn vcf_variants_render_visible_annotation_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_variants.vcf");
    let output = temp.path().join("vcf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Vcf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "VCF table should render visible variant rows"
    );
    Ok(())
}

#[test]
fn sam_alignment_records_render_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sam");
    let output = temp.path().join("sam-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sam);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "SAM table should render visible alignment rows"
    );
    Ok(())
}

#[test]
fn wig_fixed_and_variable_signals_render_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wig");
    let output = temp.path().join("wig-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Wig);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "WIG table should render visible signal rows"
    );
    Ok(())
}

#[test]
fn maf_alignment_blocks_render_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.maf");
    let output = temp.path().join("maf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Maf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "MAF table should render visible alignment rows"
    );
    Ok(())
}

#[test]
fn newick_phylogenetic_tree_renders_visible_graph() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nwk");
    let output = temp.path().join("newick-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Newick);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Newick tree should render visible graph"
    );
    Ok(())
}

#[test]
fn stockholm_alignments_render_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sto");
    let output = temp.path().join("stockholm-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Stockholm);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Stockholm table should render visible rows"
    );
    Ok(())
}

#[test]
fn clustal_alignment_renders_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.aln");
    let output = temp.path().join("clustal-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Clustal);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "CLUSTAL table should render visible rows"
    );
    Ok(())
}

#[test]
fn nexus_tree_renders_visible_graph() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nex");
    let output = temp.path().join("nexus-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Nexus);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "NEXUS tree should render visible graph"
    );
    Ok(())
}

#[test]
fn genbank_records_render_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gb");
    let output = temp.path().join("genbank-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Genbank);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "GenBank table should render visible rows"
    );
    Ok(())
}

#[test]
fn embl_records_render_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.embl");
    let output = temp.path().join("embl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Embl);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "EMBL table should render visible rows");
    Ok(())
}

#[test]
fn uniprot_records_render_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_uniprot.dat");
    let output = temp.path().join("uniprot-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Uniprot);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "UniProt table should render visible rows"
    );
    Ok(())
}

#[test]
fn ris_bibliography_renders_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ris");
    let output = temp.path().join("ris-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ris);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "RIS table should render visible rows");
    Ok(())
}

#[test]
fn spice_netlist_renders_visible_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cir");
    let output = temp.path().join("spice-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Spice);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "SPICE table should render visible rows"
    );
    Ok(())
}

#[test]
fn legacy_kicad_schematic_renders_visible_drawing() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_legacy.sch");
    let output = temp.path().join("kicad-sch-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::KicadSchLegacy);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 100,
        "KiCad legacy schematic should render visible drawing"
    );
    Ok(())
}

#[test]
fn modern_kicad_schematic_renders_visible_drawing() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.kicad_sch");
    let output = temp.path().join("kicad-modern-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::KicadSch);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 500,
        "modern KiCad schematic should render visible drawing"
    );
    Ok(())
}

#[test]
fn ltspice_ascii_schematic_renders_visible_drawing() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_ltspice.asc");
    let output = temp.path().join("ltspice-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::LtspiceAsc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 100,
        "LTspice schematic should render visible drawing"
    );
    Ok(())
}

#[test]
fn eagle_xml_schematic_renders_visible_drawing() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_eagle.sch");
    let output = temp.path().join("eagle-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::EagleSch);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 100,
        "EAGLE schematic should render visible drawing"
    );
    Ok(())
}

#[test]
fn openapi_json_renders_visible_operation_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.openapi.json");
    let output = temp.path().join("openapi-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Openapi);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OpenAPI table should render visible rows"
    );
    Ok(())
}

#[test]
fn asyncapi_json_renders_visible_channel_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.asyncapi.json");
    let output = temp.path().join("asyncapi-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Asyncapi);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "AsyncAPI table should render visible rows"
    );
    Ok(())
}

#[test]
fn json_schema_renders_visible_schema_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.schema.json");
    let output = temp.path().join("schema-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::JsonSchema);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JSON Schema table should render visible rows"
    );
    Ok(())
}

#[test]
fn ansys_cdb_mesh_renders_visible_fea_wireframe() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cdb");
    let output = temp.path().join("cdb-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Cdb);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 500,
        "ANSYS CDB mesh should render visible wireframe"
    );
    Ok(())
}

#[test]
fn har_renders_visible_request_response_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.har");
    let output = temp.path().join("har-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Har);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "HAR table should render visible rows");
    Ok(())
}

#[test]
fn warc_renders_visible_record_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.warc");
    let output = temp.path().join("warc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Warc);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "WARC table should render visible rows");
    Ok(())
}

#[test]
fn wacz_renders_visible_manifest_page_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wacz");
    let output = temp.path().join("wacz-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Wacz);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "WACZ metadata table should render visible rows"
    );
    Ok(())
}

#[test]
fn postman_collection_renders_visible_request_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.postman_collection.json");
    let output = temp.path().join("postman-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Postman);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Postman table should render visible rows"
    );
    Ok(())
}

#[test]
fn graphql_sdl_renders_visible_type_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.graphql");
    let output = temp.path().join("graphql-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Graphql);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "GraphQL table should render visible rows"
    );
    Ok(())
}

#[test]
fn protobuf_schema_renders_visible_declaration_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.proto");
    let output = temp.path().join("protobuf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Protobuf);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Protobuf table should render visible rows"
    );
    Ok(())
}

#[test]
fn kubernetes_manifest_renders_visible_resource_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.k8s.yaml");
    let output = temp.path().join("k8s-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Kubernetes);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Kubernetes table should render visible rows"
    );
    Ok(())
}

#[test]
fn compose_manifest_renders_visible_service_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.compose.yaml");
    let output = temp.path().join("compose-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Compose);
    assert_eq!(report.page_count, 1);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Compose table should render visible rows"
    );
    Ok(())
}

#[test]
fn github_actions_workflow_renders_visible_job_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.github.workflow.yml");
    let output = temp.path().join("workflow-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format.to_string(), "GITHUB-ACTIONS");
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "workflow table should render visible rows"
    );
    Ok(())
}

#[test]
fn junit_report_renders_visible_suite_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.junit.xml");
    let output = temp.path().join("junit-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Junit);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JUnit table should render visible rows"
    );
    Ok(())
}

#[test]
fn sarif_report_renders_visible_rule_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sarif");
    let output = temp.path().join("sarif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sarif);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "SARIF table should render visible rows"
    );
    Ok(())
}

#[test]
fn terraform_plan_renders_visible_resource_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.tfplan.json");
    let output = temp.path().join("terraform-plan-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::TerraformPlan);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Terraform plan table should render visible rows"
    );
    Ok(())
}

#[test]
fn cyclonedx_bom_renders_visible_component_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cdx.json");
    let output = temp.path().join("cdx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::CycloneDx);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "CycloneDX table should render visible rows"
    );
    Ok(())
}

#[test]
fn spdx_report_renders_visible_package_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.spdx.json");
    let output = temp.path().join("spdx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Spdx);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "SPDX table should render visible rows");
    Ok(())
}

#[test]
fn coverage_report_renders_visible_package_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jacoco.xml");
    let output = temp.path().join("coverage-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Coverage);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "coverage table should render visible rows"
    );
    Ok(())
}

#[test]
fn lcov_tracefile_renders_visible_file_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.lcov.info");
    let output = temp.path().join("lcov-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Lcov);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "LCOV table should render visible rows");
    Ok(())
}

#[test]
fn json_patch_renders_visible_operation_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonpatch");
    let output = temp.path().join("jsonpatch-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::JsonPatch);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JSON Patch table should render visible rows"
    );
    Ok(())
}

#[test]
fn json_merge_patch_renders_visible_path_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mergepatch");
    let output = temp.path().join("mergepatch-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::JsonMergePatch);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JSON Merge Patch table should render visible rows"
    );
    Ok(())
}

#[test]
fn openfoam_field_renders_visible_statistics_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.foamfield");
    let output = temp.path().join("foam-field-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format.to_string(), "OPENFOAM-FIELD");
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OpenFOAM field table should render visible rows"
    );
    Ok(())
}

#[test]
fn csl_json_renders_visible_bibliography_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.csl.json");
    let output = temp.path().join("csl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format.to_string(), "CSL-JSON");
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "CSL-JSON table should render visible rows"
    );
    Ok(())
}

#[test]
fn json_feed_renders_visible_item_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonfeed");
    let output = temp.path().join("jsonfeed-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::JsonFeed);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JSON Feed table should render visible rows"
    );
    Ok(())
}

#[test]
fn cloud_events_render_visible_metadata_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cloudevent.json");
    let output = temp.path().join("cloudevents-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::CloudEvents);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "CloudEvents metadata table should render visible rows"
    );
    Ok(())
}

#[test]
fn fhir_json_renders_visible_resource_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fhir.json");
    let output = temp.path().join("fhir-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::FhirJson);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "FHIR JSON resource table should render visible rows"
    );
    Ok(())
}

#[test]
fn avro_schema_renders_visible_field_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.avsc");
    let output = temp.path().join("avro-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Avro);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Avro schema field table should render visible rows"
    );
    Ok(())
}

#[test]
fn otlp_json_renders_visible_signal_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.otlp.json");
    let output = temp.path().join("otlp-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OtlpJson);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OTLP signal table should render visible rows"
    );
    Ok(())
}

#[test]
fn ocel_json_renders_visible_event_object_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonocel");
    let output = temp.path().join("ocel-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OcelJson);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OCEL event/object table should render visible rows"
    );
    Ok(())
}

#[test]
fn json_api_renders_visible_resource_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonapi");
    let output = temp.path().join("jsonapi-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::JsonApi);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JSON:API resource table should render visible rows"
    );
    Ok(())
}

#[test]
fn opendrive_renders_visible_plan_view_lines() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xodr");
    let output = temp.path().join("opendrive-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OpenDrive);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OpenDRIVE plan view should render visible lines"
    );
    Ok(())
}

#[test]
fn openscenario_renders_visible_structure_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xosc");
    let output = temp.path().join("openscenario-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OpenScenario);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OpenSCENARIO structure table should render visible rows"
    );
    Ok(())
}

#[test]
fn openlabel_renders_visible_collection_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.openlabel.json");
    let output = temp.path().join("openlabel-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OpenLabel);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OpenLABEL collection table should render visible rows"
    );
    Ok(())
}

#[test]
fn citygml_renders_visible_thematic_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.citygml");
    let output = temp.path().join("citygml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::CityGml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "CityGML thematic table should render visible rows"
    );
    Ok(())
}

#[test]
fn cityjson_renders_visible_city_object_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cityjson");
    let output = temp.path().join("cityjson-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::CityJson);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "CityJSON object table should render visible rows"
    );
    Ok(())
}

#[test]
fn stix_json_renders_visible_object_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.stix.json");
    let output = temp.path().join("stix-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::StixJson);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "STIX object table should render visible rows"
    );
    Ok(())
}

#[test]
fn opencrg_renders_visible_header_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.crg");
    let output = temp.path().join("opencrg-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OpenCrg);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OpenCRG header table should render visible rows"
    );
    Ok(())
}

#[test]
fn taxii_json_renders_visible_manifest_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.taxii.json");
    let output = temp.path().join("taxii-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::TaxiiJson);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "TAXII manifest table should render visible rows"
    );
    Ok(())
}

#[test]
fn wsdl_renders_visible_service_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wsdl");
    let output = temp.path().join("wsdl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Wsdl);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "WSDL service table should render visible rows"
    );
    Ok(())
}

#[test]
fn opml_renders_visible_outline_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.opml");
    let output = temp.path().join("opml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Opml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OPML outline table should render visible rows"
    );
    Ok(())
}

#[test]
fn rss_renders_visible_entry_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rss");
    let output = temp.path().join("rss-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Feed);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "RSS entry table should render visible rows"
    );
    Ok(())
}

#[test]
fn plist_renders_visible_key_value_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.plist");
    let output = temp.path().join("plist-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Plist);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Property List table should render visible rows"
    );
    Ok(())
}

#[test]
fn tei_renders_visible_scholarly_text_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.tei");
    let output = temp.path().join("tei-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Tei);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "TEI scholarly text table should render visible rows"
    );
    Ok(())
}

#[test]
fn alto_renders_visible_ocr_layout_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.alto");
    let output = temp.path().join("alto-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Alto);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "ALTO OCR table should render visible rows"
    );
    Ok(())
}

#[test]
fn mets_renders_visible_structure_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mets");
    let output = temp.path().join("mets-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mets);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "METS structure table should render visible rows"
    );
    Ok(())
}

#[test]
fn marcxml_renders_visible_record_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.marcxml");
    let output = temp.path().join("marcxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Marcxml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "MARCXML table should render visible rows"
    );
    Ok(())
}

#[test]
fn mods_renders_visible_bibliographic_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mods");
    let output = temp.path().join("mods-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mods);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "MODS table should render visible rows");
    Ok(())
}

#[test]
fn premis_renders_visible_entity_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.premis");
    let output = temp.path().join("premis-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Premis);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "PREMIS table should render visible rows"
    );
    Ok(())
}

#[test]
fn iiif_renders_visible_manifest_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.iiif.json");
    let output = temp.path().join("iiif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Iiif);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "IIIF manifest table should render visible rows"
    );
    Ok(())
}

#[test]
fn ead_renders_visible_finding_aid_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ead");
    let output = temp.path().join("ead-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ead);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "EAD finding-aid table should render visible rows"
    );
    Ok(())
}

#[test]
fn eac_cpf_renders_visible_authority_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.eac-cpf");
    let output = temp.path().join("eac-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::EacCpf);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "EAC-CPF table should render visible rows"
    );
    Ok(())
}

#[test]
fn dublin_core_renders_visible_metadata_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dc.xml");
    let output = temp.path().join("dc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::DublinCore);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "Dublin Core metadata table should render visible rows"
    );
    Ok(())
}

#[test]
fn s1000d_renders_visible_technical_text_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.s1000d");
    let output = temp.path().join("s1000d-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::S1000d);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "S1000D table should render visible rows"
    );
    Ok(())
}

#[test]
fn dicom_sr_renders_visible_content_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample_sr.dcm");
    let output = temp.path().join("dicom-sr-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::DicomSr);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "DICOM SR table should render visible rows"
    );
    Ok(())
}

#[test]
fn spreadsheetml_renders_visible_sheet_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.spreadsheetml");
    let output = temp.path().join("xmlss-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Spreadsheetml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "SpreadsheetML table should render visible rows"
    );
    Ok(())
}

#[test]
fn rdfxml_renders_visible_graph_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rdf");
    let output = temp.path().join("rdfxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::RdfXml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "RDF/XML summary should render visible rows"
    );
    Ok(())
}

#[test]
fn bcfzip_renders_visible_issue_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.bcfzip");
    let output = temp.path().join("bcf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Bcfzip);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "BCFZIP summary should render visible rows"
    );
    Ok(())
}

#[test]
fn flat_opc_renders_visible_office_text() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.flatopc");
    let output = temp.path().join("flat-opc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::FlatOpc);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 500, "Flat OPC text should render visible ink");
    Ok(())
}

#[test]
fn aasx_renders_visible_asset_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.aasx");
    let output = temp.path().join("aasx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Aasx);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "AASX summary should render visible rows"
    );
    Ok(())
}

#[test]
fn openscad_renders_visible_source_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.scad");
    let output = temp.path().join("openscad-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OpenScad);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "OpenSCAD summary should render visible rows"
    );
    Ok(())
}

#[test]
fn amf_renders_visible_shaded_mesh() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.amf");
    let output = temp.path().join("amf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Amf);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 10_000,
        "AMF mesh should render visible shaded geometry"
    );
    Ok(())
}

#[test]
fn plmxml_renders_visible_product_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.plmxml");
    let output = temp.path().join("plmxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::PlmXml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "PLMXML summary should render visible rows"
    );
    Ok(())
}

#[test]
fn step_xml_renders_visible_product_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.stepxml");
    let output = temp.path().join("stepxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::StepXml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "STEP-XML summary should render visible rows"
    );
    Ok(())
}

#[test]
fn qif_renders_visible_inspection_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.qif");
    let output = temp.path().join("qif-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Qif);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "QIF summary should render visible rows"
    );
    Ok(())
}

#[test]
fn b2mml_renders_visible_manufacturing_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.b2mml");
    let output = temp.path().join("b2mml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::B2mml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "B2MML summary should render visible rows"
    );
    Ok(())
}

#[test]
fn jdf_renders_visible_job_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jdf");
    let output = temp.path().join("jdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Jdf);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "JDF summary should render visible rows"
    );
    Ok(())
}

#[test]
fn xjdf_renders_visible_job_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xjdf");
    let output = temp.path().join("xjdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xjdf);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "XJDF summary should render visible rows"
    );
    Ok(())
}

#[test]
fn cml_renders_visible_chemical_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cml");
    let output = temp.path().join("cml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Cml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "CML summary should render visible rows"
    );
    Ok(())
}

#[test]
fn xdp_renders_visible_package_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xdp");
    let output = temp.path().join("xdp-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xdp);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "XDP summary should render visible rows"
    );
    Ok(())
}

#[test]
fn xmp_renders_visible_metadata_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xmp");
    let output = temp.path().join("xmp-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xmp);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "XMP table should render visible rows");
    Ok(())
}

#[test]
fn mathml_renders_visible_formula_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.mathml");
    let output = temp.path().join("mathml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Mathml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "MathML summary should render visible rows"
    );
    Ok(())
}

#[test]
fn landxml_renders_visible_civil_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.landxml");
    let output = temp.path().join("landxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::LandXml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "LandXML summary should render visible rows"
    );
    Ok(())
}

#[test]
fn xfdf_renders_visible_form_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xfdf");
    let output = temp.path().join("xfdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xfdf);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "XFDF table should render visible rows");
    Ok(())
}

#[test]
fn fdf_renders_visible_form_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fdf");
    let output = temp.path().join("fdf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Fdf);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "FDF table should render visible rows");
    Ok(())
}

#[test]
fn xbrl_renders_visible_fact_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xbrl.xml");
    let output = temp.path().join("xbrl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xbrl);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "XBRL table should render visible rows");
    Ok(())
}

#[test]
fn ubl_renders_visible_business_document_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.ubl.xml");
    let output = temp.path().join("ubl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Ubl);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(visible_ink > 1_000, "UBL table should render visible rows");
    Ok(())
}

#[test]
fn iso19115_renders_visible_metadata_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.iso19115.xml");
    let output = temp.path().join("iso19115-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Iso19115);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "ISO 19115 table should render visible rows"
    );
    Ok(())
}

#[test]
fn marc21_renders_visible_iso2709_table() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.marc");
    let output = temp.path().join("marc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Marc);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    let visible_ink = rendered
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count();
    assert!(
        visible_ink > 1_000,
        "MARC21 ISO 2709 table should render visible rows"
    );
    Ok(())
}

#[test]
fn tmx_renders_visible_translation_memory_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.tmx");
    let output = temp.path().join("tmx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Tmx);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    assert!(visible_ink(&rendered) > 1_000);
    Ok(())
}

#[test]
fn tbx_renders_visible_terminology_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.tbx");
    let output = temp.path().join("tbx-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Tbx);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    assert!(visible_ink(&rendered) > 1_000);
    Ok(())
}

#[test]
fn gbxml_renders_visible_building_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.gbxml");
    let output = temp.path().join("gbxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::GbXml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    assert!(visible_ink(&rendered) > 1_000);
    Ok(())
}

#[test]
fn fhir_xml_renders_visible_resource_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fhir.xml");
    let output = temp.path().join("fhir-xml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::FhirXml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    assert!(visible_ink(&rendered) > 1_000);
    Ok(())
}

#[test]
fn idml_renders_visible_package_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.idml");
    let output = temp.path().join("idml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Idml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    assert!(visible_ink(&rendered) > 1_000);
    Ok(())
}

#[test]
fn xpdl_renders_visible_workflow_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xpdl");
    let output = temp.path().join("xpdl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xpdl);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    assert!(visible_ink(&rendered) > 1_000);
    Ok(())
}

#[test]
fn onix_renders_visible_publishing_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.onix");
    let output = temp.path().join("onix-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Onix);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    assert!(visible_ink(&rendered) > 1_000);
    Ok(())
}

#[test]
fn oaipmh_renders_visible_harvest_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.oaipmh");
    let output = temp.path().join("oaipmh-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OaiPmh);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let rendered = render_svg_with_resvg(&svg)?;
    assert!(visible_ink(&rendered) > 1_000);
    Ok(())
}

#[test]
fn cda_renders_visible_clinical_structure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cda");
    let output = temp.path().join("cda-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Cda);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn iso20022_renders_visible_financial_structure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.iso20022.xml");
    let output = temp.path().join("iso20022-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Iso20022);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn sbml_renders_visible_model_structure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sbml");
    let output = temp.path().join("sbml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sbml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn cellml_renders_visible_model_structure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.cellml");
    let output = temp.path().join("cellml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Cellml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn ocel_xml_renders_visible_event_log_structure() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xmlocel");
    let output = temp.path().join("ocel-xml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OcelXml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn energyplus_idf_renders_visible_object_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.idf");
    let output = temp.path().join("idf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::EnergyPlusIdf);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn energyplus_epw_renders_visible_weather_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.epw");
    let output = temp.path().join("epw-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::EnergyPlusEpw);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn rinex_renders_visible_gnss_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.rnx");
    let output = temp.path().join("rinex-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Rinex);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn sat_renders_visible_entity_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sat");
    let output = temp.path().join("sat-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sat);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn sedml_renders_visible_experiment_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sedml");
    let output = temp.path().join("sedml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sedml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn sbgnml_renders_visible_map_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.sbgnml");
    let output = temp.path().join("sbgnml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Sbgnml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn omex_renders_visible_archive_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.omex");
    let output = temp.path().join("omex-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Omex);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn xdmf_renders_visible_mesh_metadata_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xdmf");
    let output = temp.path().join("xdmf-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xdmf);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn pvd_renders_visible_collection_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pvd");
    let output = temp.path().join("pvd-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Pvd);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn fds_renders_visible_input_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fds");
    let output = temp.path().join("fds-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Fds);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn abiword_renders_visible_document_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.abw");
    let output = temp.path().join("abw-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Abiword);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn neuroml_renders_visible_model_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.nml");
    let output = temp.path().join("neuroml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Neuroml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn biopax_renders_visible_pathway_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.biopax.xml");
    let output = temp.path().join("biopax-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Biopax);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn xsd_renders_visible_schema_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xsd");
    let output = temp.path().join("xsd-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xsd);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn xslt_renders_visible_stylesheet_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xsl");
    let output = temp.path().join("xslt-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xslt);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn xsl_fo_renders_visible_layout_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.fo");
    let output = temp.path().join("xslfo-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::XslFo);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn xproc_renders_visible_pipeline_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xproc");
    let output = temp.path().join("xproc-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xproc);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn wadl_renders_visible_rest_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.wadl");
    let output = temp.path().join("wadl-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Wadl);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn opensearch_renders_visible_descriptor_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.osdd");
    let output = temp.path().join("opensearch-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::OpenSearch);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn saml_renders_visible_metadata_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.saml.xml");
    let output = temp.path().join("saml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Saml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn xacml_renders_visible_policy_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.xacml");
    let output = temp.path().join("xacml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Xacml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn legacy_openoffice_packages_render_visible_odf_content() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for extension in ["sxw", "sxc", "sxi"] {
        let output = temp.path().join(format!("{extension}-out"));
        let input = root.join(format!("sample.{extension}"));
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert!(report.page_count >= 1, "{extension}");
        let svg = fs::read(output.join("page-0001.svg"))?;
        assert!(
            visible_ink(&render_svg_with_resvg(&svg)?) > 1_000,
            "{extension}"
        );
    }
    Ok(())
}

#[test]
fn legacy_visio_binary_renders_visible_container_summary() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.vsd");
    let output = temp.path().join("vsd-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Vsd);
    let svg = fs::read(output.join("page-0001.svg"))?;
    let text = std::str::from_utf8(&svg)?;
    assert!(text.contains("Legacy Visio binary document"));
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn hdf5_and_cgns_render_visible_superblock_summaries() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for extension in ["h5", "cgns", "exo"] {
        let input = root.join(format!("sample.{extension}"));
        let output = temp.path().join(format!("{extension}-out"));
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.page_count, 1, "{extension}");
        let svg = fs::read(output.join("page-0001.svg"))?;
        assert!(
            visible_ink(&render_svg_with_resvg(&svg)?) > 1_000,
            "{extension}"
        );
    }
    Ok(())
}

#[test]
fn iwork_packages_render_visible_structure_summaries() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for extension in ["pages", "numbers", "key"] {
        let input = root.join(format!("sample.{extension}"));
        let output = temp.path().join(format!("{extension}-out"));
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.source_format, SourceFormat::Iwork, "{extension}");
        let svg = fs::read(output.join("page-0001.svg"))?;
        assert!(
            visible_ink(&render_svg_with_resvg(&svg)?) > 1_000,
            "{extension}"
        );
    }
    Ok(())
}

#[test]
fn dwg_header_renders_visible_version_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.dwg");
    let output = temp.path().join("dwg-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Dwg);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn rhino_3dm_header_renders_visible_marker_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.3dm");
    let output = temp.path().join("3dm-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::Rhino3dm);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

#[test]
fn access_headers_render_visible_engine_summaries() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for extension in ["accdb", "mdb"] {
        let input = root.join(format!("sample.{extension}"));
        let output = temp.path().join(format!("{extension}-out"));
        let report = convert_path(&input, &output, &ConvertOptions::default())?;
        assert_eq!(report.source_format, SourceFormat::Access, "{extension}");
        let svg = fs::read(output.join("page-0001.svg"))?;
        assert!(
            visible_ink(&render_svg_with_resvg(&svg)?) > 1_000,
            "{extension}"
        );
    }
    Ok(())
}

#[test]
fn threedxml_product_structure_renders_visible_summary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.3dxml");
    let output = temp.path().join("3dxml-out");
    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, SourceFormat::ThreeDXml);
    let svg = fs::read(output.join("page-0001.svg"))?;
    assert!(visible_ink(&render_svg_with_resvg(&svg)?) > 1_000);
    Ok(())
}

fn visible_ink(pixmap: &resvg::tiny_skia::Pixmap) -> usize {
    pixmap
        .data()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] < 235 || pixel[1] < 235 || pixel[2] < 235))
        .count()
}
