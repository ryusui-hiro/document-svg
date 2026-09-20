use std::fs;
use tempfile::tempdir;

use document_svg::{
    ConvertOptions, ReverseOptions, TransformOptions, convert_path, svg_to_document, transform_svg,
};

#[test]
fn test_qr_generation_and_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let qr_input = temp.path().join("link.qr");
    let out_dir = temp.path().join("qr_out");

    fs::write(&qr_input, "https://example.com/svg-hub")?;

    let report = convert_path(&qr_input, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    assert!(svg_path.exists());
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("<svg"));
    assert!(svg_str.contains("modules"));

    Ok(())
}

#[test]
fn test_svg_to_code_outputs() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let svg_path = temp.path().join("icon.svg");
    let jsx_out = temp.path().join("AppIcon.jsx");
    let tsx_out = temp.path().join("AppIcon.tsx");
    let vue_out = temp.path().join("AppIcon.vue");
    let datauri_out = temp.path().join("icon.datauri");

    let svg_content = r##"<!-- Generator: test -->
<svg width="24" height="24" viewBox="0 0 24 24" fill="none" xmlns:xlink="http://www.w3.org/1999/xlink">
  <path d="M12 2L2 22h20L12 2z" fill="#2563eb" stroke-width="2" pointer-events="none" flood-color="#000" />
</svg>"##;
    fs::write(&svg_path, svg_content)?;

    // 1. SVG -> JSX
    svg_to_document(&svg_path, &jsx_out, &ReverseOptions::default())?;
    let jsx_code = fs::read_to_string(&jsx_out)?;
    assert!(jsx_code.contains("export const AppIcon"));
    assert!(jsx_code.contains("strokeWidth=")); // Kebab to camelCase
    assert!(jsx_code.contains("pointerEvents="));
    assert!(jsx_code.contains("floodColor="));
    assert!(jsx_code.contains("xmlnsXlink="));
    assert!(!jsx_code.contains("<!--"));

    // 2. SVG -> TSX
    svg_to_document(&svg_path, &tsx_out, &ReverseOptions::default())?;
    let tsx_code = fs::read_to_string(&tsx_out)?;
    assert!(tsx_code.contains("React.FC<React.SVGProps<SVGSVGElement>>"));

    // 3. SVG -> Vue
    svg_to_document(&svg_path, &vue_out, &ReverseOptions::default())?;
    let vue_code = fs::read_to_string(&vue_out)?;
    assert!(vue_code.contains("<template>"));
    assert!(vue_code.contains("<script setup>"));

    // 4. SVG -> DataURI
    svg_to_document(&svg_path, &datauri_out, &ReverseOptions::default())?;
    let datauri_str = fs::read_to_string(&datauri_out)?;
    assert!(datauri_str.starts_with("data:image/svg+xml;base64,"));

    // 5. SVG -> HTML
    let html_out = temp.path().join("AppIcon.html");
    svg_to_document(&svg_path, &html_out, &ReverseOptions::default())?;
    let html_str = fs::read_to_string(&html_out)?;
    assert!(html_str.contains("<!DOCTYPE html>"));
    assert!(html_str.contains("<title>AppIcon</title>"));
    assert!(html_str.contains("<svg"));
    assert!(html_str.contains("class=\"svg-container\""));

    // 6. SVG -> WebP
    let webp_out = temp.path().join("AppIcon.webp");
    svg_to_document(&svg_path, &webp_out, &ReverseOptions::default())?;
    let webp_bytes = fs::read(&webp_out)?;
    assert!(webp_bytes.len() >= 12);
    assert_eq!(&webp_bytes[0..4], b"RIFF");
    assert_eq!(&webp_bytes[8..12], b"WEBP");

    Ok(())
}

#[test]
fn test_svg_transform_minify_monochrome_responsive() -> Result<(), Box<dyn std::error::Error>> {
    let raw_svg = br##"<!-- Comment to be removed -->
<svg width="100px" height="80px">
  <path d="M 0 0 L 50 50 Z" fill="#ff0000" stroke="#00ff00" />
</svg>"##;

    // 1. Minify + Responsive
    let res1 = transform_svg(
        raw_svg,
        &TransformOptions {
            minify: true,
            responsive: true,
            ..Default::default()
        },
    )?;
    let res1_str = std::str::from_utf8(&res1)?;
    assert!(!res1_str.contains("Comment to be removed"));
    assert!(res1_str.contains("viewBox=\"0 0 100 80\""));
    assert!(!res1_str.contains("width=\"100px\""));

    // 2. Monochrome
    let res2 = transform_svg(
        raw_svg,
        &TransformOptions {
            monochrome: Some("#000000".to_string()),
            ..Default::default()
        },
    )?;
    let res2_str = std::str::from_utf8(&res2)?;
    assert!(res2_str.contains("fill=\"#000000\""));
    assert!(res2_str.contains("stroke=\"#000000\""));

    // 2b. Monochrome with inline style and stop-color
    let gradient_svg = br##"<svg viewBox="0 0 10 10">
  <defs>
    <linearGradient id="g1">
      <stop offset="0%" stop-color="#ff0000" />
      <stop offset="100%" stop-color="#0000ff" />
    </linearGradient>
  </defs>
  <rect width="10" height="10" style="fill: #123456; stroke: #654321" />
</svg>"##;
    let res_mono = transform_svg(
        gradient_svg,
        &TransformOptions {
            monochrome: Some("#333333".to_string()),
            ..Default::default()
        },
    )?;
    let res_mono_str = std::str::from_utf8(&res_mono)?;
    assert!(res_mono_str.contains("stop-color=\"#333333\""));
    assert!(res_mono_str.contains("style=\"fill: #333333; stroke: #333333\""));

    // 3. Precision rounding
    let coord_svg = br##"<svg viewBox="0 0 100 100">
  <path d="M 12.34567 89.12345 L 45.67891 99.99999 Z" stroke-width="1.2345" />
</svg>"##;
    let res3 = transform_svg(
        coord_svg,
        &TransformOptions {
            precision: Some(2),
            ..Default::default()
        },
    )?;
    let res3_str = std::str::from_utf8(&res3)?;
    assert!(res3_str.contains("12.35"));
    assert!(res3_str.contains("89.12"));
    assert!(res3_str.contains("45.68"));
    assert!(res3_str.contains("100"));
    assert!(res3_str.contains("stroke-width=\"1.23\""));

    let sci_svg = br##"<svg viewBox='0 0 10 20'>
  <path d='M 1.23456e-1 -2.500E+1' stroke-width='1.2300e0' />
</svg>"##;
    let res4 = transform_svg(
        sci_svg,
        &TransformOptions {
            precision: Some(2),
            ..Default::default()
        },
    )?;
    let res4_str = std::str::from_utf8(&res4)?;
    assert!(!res4_str.contains("1.23456e-1"));
    assert!(!res4_str.contains("-2.500E+1"));
    assert!(!res4_str.contains("1.2300e0"));
    assert!(res4_str.contains("-25"));
    assert!(res4_str.contains("stroke-width=\"1.23\""));

    // 4. Remove metadata
    let meta_svg = br##"<svg viewBox="0 0 10 10" data-source="docsvg">
  <metadata><author>Test Author</author></metadata>
  <desc>A test diagram description</desc>
  <rect x="0" y="0" width="10" height="10" data-element-id="r1" />
</svg>"##;
    let res5 = transform_svg(
        meta_svg,
        &TransformOptions {
            remove_metadata: true,
            ..Default::default()
        },
    )?;
    let res5_str = std::str::from_utf8(&res5)?;
    assert!(!res5_str.contains("Test Author"));
    assert!(!res5_str.contains("metadata"));
    assert!(!res5_str.contains("A test diagram description"));
    assert!(!res5_str.contains("desc"));
    assert!(!res5_str.contains("data-source"));
    assert!(!res5_str.contains("data-element-id"));
    assert!(res5_str.contains("<rect"));

    Ok(())
}

#[test]
fn test_raster_vectorization() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let png_path = temp.path().join("tiny_icon.png");
    let out_dir = temp.path().join("vec_out");

    // Create a tiny 4x4 PNG image in memory
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 4, 4);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        // Center 2x2 is black (0), outer border is white (255)
        let pixels: [u8; 16] = [
            255, 255, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255,
        ];
        writer.write_image_data(&pixels)?;
    }
    fs::write(&png_path, png_bytes)?;

    let report = convert_path(&png_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    assert!(svg_path.exists());
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("<svg"));
    assert!(svg_str.contains("vectorized_path"));

    Ok(())
}

#[test]
fn test_svg_to_svelte_and_path_data() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let svg_path = temp.path().join("icon.svg");
    let svelte_out = temp.path().join("AppIcon.svelte");
    let path_out = temp.path().join("icon.path");

    let svg_content = r##"<svg viewBox="0 0 24 24" width="24" height="24">
  <path d="M12 2L2 22h20L12 2z" fill="#2563eb" />
</svg>"##;
    fs::write(&svg_path, svg_content)?;

    // SVG -> Svelte
    svg_to_document(&svg_path, &svelte_out, &ReverseOptions::default())?;
    let svelte_code = fs::read_to_string(&svelte_out)?;
    assert!(svelte_code.contains("<script>"));
    assert!(svelte_code.contains("{...$$restProps}"));
    assert!(svelte_code.contains("M12 2L2 22h20L12 2z"));

    // SVG -> PathData (.path / .icon)
    svg_to_document(&svg_path, &path_out, &ReverseOptions::default())?;
    let path_data = fs::read_to_string(&path_out)?;
    assert_eq!(path_data.trim(), "M12 2L2 22h20L12 2z");

    Ok(())
}

#[test]
fn test_compound_extensions_and_content_sniffing() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;

    let temp = tempdir()?;

    // 1. Compound extension .chart.json
    let chart_file = temp.path().join("metrics.chart.json");
    fs::write(
        &chart_file,
        r#"{"type":"bar","title":"Stats","labels":["Q1","Q2"],"series":[{"name":"Q1","data":[10.0,20.0],"color":null}]}"#,
    )?;
    let chart_out = temp.path().join("chart_out");
    let report = convert_path(&chart_file, &chart_out, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert!(chart_out.join("page-0001.svg").exists());

    // 2. Extensionless DXF file -> content sniffing
    let dxf_no_ext = temp.path().join("raw_cad_model");
    fs::write(
        &dxf_no_ext,
        "0\nSECTION\n2\nENTITIES\n0\nLINE\n8\n0\n10\n0.0\n20\n0.0\n11\n10.0\n21\n10.0\n0\nENDSEC\n0\nEOF\n",
    )?;
    let dxf_out = temp.path().join("dxf_out");
    let report_dxf = convert_path(&dxf_no_ext, &dxf_out, &ConvertOptions::default())?;
    assert_eq!(report_dxf.page_count, 1);
    assert!(dxf_out.join("page-0001.svg").exists());

    // 3. Extensionless DOT file -> content sniffing
    let dot_no_ext = temp.path().join("graph_pipeline");
    fs::write(&dot_no_ext, "digraph Workflow {\n  A -> B -> C;\n}\n")?;
    let dot_out = temp.path().join("dot_out");
    let report_dot = convert_path(&dot_no_ext, &dot_out, &ConvertOptions::default())?;
    assert_eq!(report_dot.page_count, 1);
    assert!(dot_out.join("page-0001.svg").exists());

    // 4. Extensionless CSV table -> content sniffing
    let csv_no_ext = temp.path().join("table_data");
    fs::write(
        &csv_no_ext,
        "Name,Role,Status\nAlice,Admin,Active\nBob,User,Pending\n",
    )?;
    let csv_out = temp.path().join("csv_out");
    let report_csv = convert_path(&csv_no_ext, &csv_out, &ConvertOptions::default())?;
    assert_eq!(report_csv.page_count, 1);
    // 5. SVG file input & extensionless SVG content sniffing
    let svg_file = temp.path().join("vector_art.svg");
    fs::write(
        &svg_file,
        r##"<svg width="200" height="100" viewBox="0 0 200 100"><rect x="10" y="10" width="80" height="40" fill="#ff0000"/><circle cx="150" cy="50" r="30" fill="#0000ff"/></svg>"##,
    )?;
    let svg_out = temp.path().join("svg_out");
    let report_svg = convert_path(&svg_file, &svg_out, &ConvertOptions::default())?;
    assert_eq!(report_svg.page_count, 1);
    assert_eq!(report_svg.source_format.to_string(), "SVG");
    assert!(svg_out.join("page-0001.svg").exists());

    let svg_no_ext = temp.path().join("untyped_svg_drawing");
    fs::write(
        &svg_no_ext,
        r##"<svg viewBox="0 0 50 50"><line x1="0" y1="0" x2="50" y2="50" stroke="#000"/></svg>"##,
    )?;
    let svg_no_ext_out = temp.path().join("svg_no_ext_out");
    let report_no_ext = convert_path(&svg_no_ext, &svg_no_ext_out, &ConvertOptions::default())?;
    assert_eq!(report_no_ext.page_count, 1);
    assert_eq!(report_no_ext.source_format.to_string(), "SVG");
    assert!(svg_no_ext_out.join("page-0001.svg").exists());

    // A generic ZIP with a mimetype file is not sufficient evidence for EPUB.
    let archive_path = temp.path().join("unrelated.zip");
    let archive = fs::File::create(&archive_path)?;
    let mut zip = zip::ZipWriter::new(archive);
    zip.start_file("mimetype", zip::write::SimpleFileOptions::default())?;
    zip.write_all(b"application/example")?;
    zip.start_file("content.txt", zip::write::SimpleFileOptions::default())?;
    zip.write_all(b"not an EPUB package")?;
    zip.finish()?;
    let zip_error = convert_path(
        &archive_path,
        temp.path().join("zip_out"),
        &ConvertOptions::default(),
    )
    .unwrap_err();
    assert!(zip_error.to_string().contains("cannot determine format"));

    Ok(())
}

#[test]
fn test_grayscale_alpha_and_indexed_png_vectorization() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let png_path = temp.path().join("alpha_icon.png");
    let out_dir = temp.path().join("vec_alpha_out");

    // Create a 2x2 GrayscaleAlpha PNG
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, 2, 2);
        encoder.set_color(png::ColorType::GrayscaleAlpha);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        // Pixel 0: black (0), opaque (255)
        // Pixel 1: white (255), opaque (255)
        // Pixel 2: black (0), transparent (0) -> should be treated as white/background
        // Pixel 3: black (0), opaque (255)
        let pixels: [u8; 8] = [0, 255, 255, 255, 0, 0, 0, 255];
        writer.write_image_data(&pixels)?;
    }
    fs::write(&png_path, png_bytes)?;

    let report = convert_path(&png_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    assert!(svg_path.exists());
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("<svg"));
    assert!(svg_str.contains("vectorized_path"));

    Ok(())
}

#[test]
fn test_tiff_vectorization() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let tiff_path = temp.path().join("sample.tif");

    let mut tiff_bytes = Vec::new();
    {
        let mut cursor = std::io::Cursor::new(&mut tiff_bytes);
        let mut encoder = tiff::encoder::TiffEncoder::new(&mut cursor)?;
        let pixels: [u8; 4] = [0, 255, 255, 0]; // 2x2 grayscale image
        let image = encoder.new_image::<tiff::encoder::colortype::Gray8>(2, 2)?;
        image.write_data(&pixels)?;
    }
    fs::write(&tiff_path, &tiff_bytes)?;

    let (w, h, gray) = document_svg::vectorize::decode_tiff(&tiff_bytes)?;
    assert_eq!(w, 2);
    assert_eq!(h, 2);
    assert_eq!(gray.len(), 4);
    assert_eq!(gray[0], 0);
    assert_eq!(gray[1], 255);

    let page = document_svg::vectorize::vectorize_grayscale(w, h, &gray, 128)?;
    assert_eq!(page.nodes.len(), 1);
    Ok(())
}

#[test]
fn test_svg_transform_style_tag_monochrome() -> Result<(), Box<dyn std::error::Error>> {
    let svg_with_css = br##"<svg viewBox="0 0 100 100">
  <style>
    .st0 { fill: #ff0000; stroke: #00ff00; }
    .st1 { stop-color: #0000ff; color: #ffff00; }
  </style>
  <rect class="st0" width="50" height="50" />
</svg>"##;

    let res = transform_svg(
        svg_with_css,
        &TransformOptions {
            monochrome: Some("#222222".to_string()),
            ..Default::default()
        },
    )?;
    let res_str = std::str::from_utf8(&res)?;
    assert!(res_str.contains("fill: #222222"));
    assert!(res_str.contains("stroke: #222222"));
    assert!(res_str.contains("stop-color: #222222"));
    assert!(res_str.contains("color: #222222"));
    assert!(!res_str.contains("#ff0000"));
    assert!(!res_str.contains("#00ff00"));
    assert!(!res_str.contains("#0000ff"));

    Ok(())
}

#[test]
fn test_svg_transform_clean_paths_and_empty_groups() -> Result<(), Box<dyn std::error::Error>> {
    let svg_with_redundant = br##"<svg viewBox="0 0 100 100">
  <g id="empty-wrapper">
    <g id="nested-empty"></g>
  </g>
  <g id="content-wrapper">
    <path d="M 10 20 L 10 20 L 30 40 Z Z" stroke="#000" />
    <path d="M 0 0 L 0 0" stroke="#fff" />
  </g>
</svg>"##;

    let res = transform_svg(
        svg_with_redundant,
        &TransformOptions {
            clean_paths: true,
            strip_empty_groups: true,
            ..Default::default()
        },
    )?;
    let res_str = std::str::from_utf8(&res)?;

    // empty groups should be stripped
    assert!(!res_str.contains("empty-wrapper"));
    assert!(!res_str.contains("nested-empty"));

    // content wrapper should remain
    assert!(res_str.contains("content-wrapper"));

    // redundant L 10 20 and duplicate Z should be cleaned
    assert!(res_str.contains("M 10 20 L 30 40 Z"));
    assert!(!res_str.contains("L 10 20 L 10 20"));
    assert!(!res_str.contains("Z Z"));

    // zero-length path M 0 0 L 0 0 was stripped
    assert!(!res_str.contains("stroke=\"#fff\""));

    Ok(())
}

#[test]
fn test_svg_to_jsx_inline_style_conversion() -> Result<(), Box<dyn std::error::Error>> {
    let svg_content = br##"<svg viewBox="0 0 24 24">
  <path style="fill: #2563eb; stroke-width: 2px; stroke-linecap: round;" d="M 0 0 L 10 10" />
</svg>"##;

    let jsx = document_svg::code::svg_to_jsx(svg_content, "StyledIcon", true)?;
    assert!(jsx.contains("export const StyledIcon: React.FC<React.SVGProps<SVGSVGElement>>"));
    // Inline style string must be converted to JSX object
    assert!(jsx.contains("style={{"));
    assert!(jsx.contains("fill: '#2563eb'"));
    assert!(jsx.contains("strokeWidth: '2px'"));
    assert!(jsx.contains("strokeLinecap: 'round'"));
    assert!(!jsx.contains("style=\"fill:"));

    Ok(())
}
