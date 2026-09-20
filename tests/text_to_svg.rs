use std::fs;
use tempfile::tempdir;

use document_svg::{ConvertOptions, ReverseOptions, SourceFormat, convert_path, svg_to_document};

#[test]
fn test_dot_diagram_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let dot_path = temp.path().join("sample.dot");
    let out_dir = temp.path().join("dot_svg_out");
    let reverse_out = temp.path().join("restored.dot");

    let dot_content = r#"digraph Architecture {
  Client -> Gateway [label="HTTPS"];
  Gateway -> AuthService;
  Gateway -> BackendAPI;
}"#;
    fs::write(&dot_path, dot_content)?;

    // 1. Convert DOT to SVG
    let report = convert_path(&dot_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    assert!(svg_path.exists());
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("<svg"));
    assert!(svg_str.contains("Client"));
    assert!(svg_str.contains("Gateway"));
    assert!(svg_str.contains("AuthService"));

    // 2. Reverse SVG to DOT
    let rev_report = svg_to_document(&svg_path, &reverse_out, &ReverseOptions::default())?;
    assert_eq!(rev_report.page_count, 1);
    assert!(reverse_out.exists());
    let restored_dot = fs::read_to_string(&reverse_out)?;
    assert!(restored_dot.contains("Client"));
    assert!(restored_dot.contains("Gateway"));

    Ok(())
}

#[test]
fn test_mermaid_diagram_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let mmd_path = temp.path().join("flow.mmd");
    let out_dir = temp.path().join("mmd_svg_out");

    let mmd_content = r#"graph TD
  A[Start Process] -->|Approve| B(Run Worker)
  B --> C{Success?}
"#;
    fs::write(&mmd_path, mmd_content)?;

    let report = convert_path(&mmd_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("Start Process"));
    assert!(svg_str.contains("Run Worker"));
    assert!(svg_str.contains("Success?"));

    Ok(())
}

#[test]
fn test_markdown_table_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("table.md");
    let out_dir = temp.path().join("md_svg_out");
    let reverse_out = temp.path().join("restored.md");

    let md_content = r#"| Service | Status | Replicas |
| :--- | :---: | ---: |
| API Gateway | Healthy | 3 |
| Worker | Busy | 12 |
"#;
    fs::write(&md_path, md_content)?;

    // 1. Convert Markdown table to SVG
    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("API Gateway"));
    assert!(svg_str.contains("Worker"));
    assert!(svg_str.contains("Healthy"));

    // 2. Reverse SVG to Markdown
    let rev_report = svg_to_document(&svg_path, &reverse_out, &ReverseOptions::default())?;
    assert_eq!(rev_report.page_count, 1);
    let restored_md = fs::read_to_string(&reverse_out)?;
    assert!(restored_md.contains("Service"));
    assert!(restored_md.contains("API Gateway"));
    assert!(restored_md.contains("Worker"));

    Ok(())
}

#[test]
fn test_chart_to_svg_and_csv_reverse() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let chart_path = temp.path().join("sales.chart.json");
    let out_dir = temp.path().join("chart_svg_out");
    let csv_out = temp.path().join("sales.csv");

    let chart_json = r##"{
  "type": "bar",
  "title": "Quarterly Revenue",
  "labels": ["Q1", "Q2", "Q3", "Q4"],
  "series": [
    {
      "name": "Revenue",
      "data": [150.0, 220.0, 310.0, 280.0],
      "color": "#2563eb"
    }
  ]
}"##;
    fs::write(&chart_path, chart_json)?;

    // 1. Convert Chart JSON to SVG
    let report = convert_path(&chart_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("Quarterly Revenue"));
    assert!(svg_str.contains("Q1"));
    assert!(svg_str.contains("Q4"));

    // 2. Reverse SVG to CSV
    let rev_report = svg_to_document(&svg_path, &csv_out, &ReverseOptions::default())?;
    assert_eq!(rev_report.page_count, 1);
    let csv_str = fs::read_to_string(&csv_out)?;
    assert!(csv_str.contains("Quarterly Revenue") || csv_str.contains("Q1"));

    Ok(())
}

#[test]
fn test_chart_extension_case_insensitive() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let chart_path = temp.path().join("sales.CHART.JSON");
    let out_dir = temp.path().join("chart_svg_out_case");

    let chart_json = r##"{
  "type": "bar",
  "title": "Case Insensitive Chart",
  "labels": ["A", "B"],
  "series": [
    {
      "name": "Revenue",
      "data": [100.0, 200.0],
      "color": "#10b981"
    }
  ]
}"##;
    fs::write(&chart_path, chart_json)?;

    let report = convert_path(&chart_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("Case Insensitive Chart"));

    Ok(())
}

#[test]
fn test_latex_math_to_svg_and_reverse() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let tex_path = temp.path().join("formula.tex");
    let out_dir = temp.path().join("math_svg_out");
    let tex_out = temp.path().join("restored.tex");

    let formula = r#"\frac{a + b}{\sqrt{c^2 + 1}}"#;
    fs::write(&tex_path, formula)?;

    // 1. Convert LaTeX math to SVG
    let report = convert_path(&tex_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("<svg"));
    assert!(svg_str.contains("a"));
    assert!(svg_str.contains("b"));
    assert!(svg_str.contains("c"));

    // 2. Reverse SVG to LaTeX
    let rev_report = svg_to_document(&svg_path, &tex_out, &ReverseOptions::default())?;
    assert_eq!(rev_report.page_count, 1);
    let restored_tex = fs::read_to_string(&tex_out)?;
    assert!(!restored_tex.is_empty());

    Ok(())
}

#[test]
fn test_line_chart_to_svg_and_csv_reverse() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let chart_path = temp.path().join("latency.chart.json");
    let out_dir = temp.path().join("line_chart_out");
    let csv_out = temp.path().join("restored.csv");

    let chart_json = r##"{
  "type": "line",
  "title": "API Request Latency (ms)",
  "labels": ["00:00", "04:00", "08:00", "12:00", "16:00", "20:00"],
  "series": [
    {
      "name": "p50",
      "data": [12.0, 14.0, 28.0, 35.0, 22.0, 15.0],
      "color": "#3b82f6"
    },
    {
      "name": "p99",
      "data": [45.0, 52.0, 95.0, 110.0, 78.0, 48.0],
      "color": "#ef4444"
    }
  ]
}"##;
    fs::write(&chart_path, chart_json)?;

    let report = convert_path(&chart_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("API Request Latency"));
    assert!(svg_str.contains("p50"));
    assert!(svg_str.contains("p99"));

    let rev_report = svg_to_document(&svg_path, &csv_out, &ReverseOptions::default())?;
    assert_eq!(rev_report.page_count, 1);
    let csv_str = fs::read_to_string(&csv_out)?;
    assert!(csv_str.contains("p50"));
    assert!(csv_str.contains("p99"));
    assert!(csv_str.contains("00:00"));
    assert!(csv_str.contains("110"));

    Ok(())
}

#[test]
fn test_pie_chart_to_svg_and_csv_reverse() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let chart_path = temp.path().join("storage.chart.json");
    let out_dir = temp.path().join("pie_chart_out");
    let csv_out = temp.path().join("restored.csv");

    let chart_json = r##"{
  "type": "pie",
  "title": "Cloud Storage Breakdown",
  "labels": ["Hot Storage", "Cold Archive", "Snapshots", "Backups"],
  "series": [
    {
      "name": "Usage (TB)",
      "data": [120.0, 450.0, 85.0, 230.0],
      "color": null
    }
  ]
}"##;
    fs::write(&chart_path, chart_json)?;

    let report = convert_path(&chart_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);

    let svg_path = out_dir.join("page-0001.svg");
    let svg_str = fs::read_to_string(&svg_path)?;
    assert!(svg_str.contains("Cloud Storage Breakdown"));
    assert!(svg_str.contains("Hot Storage"));
    assert!(svg_str.contains("Cold Archive"));

    let rev_report = svg_to_document(&svg_path, &csv_out, &ReverseOptions::default())?;
    assert_eq!(rev_report.page_count, 1);
    let csv_str = fs::read_to_string(&csv_out)?;
    assert!(csv_str.contains("Hot Storage"));
    assert!(csv_str.contains("Cold Archive"));
    assert!(csv_str.contains("450"));

    Ok(())
}

#[test]
fn test_csv_and_tsv_table_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let csv_path = temp.path().join("data.csv");
    let tsv_path = temp.path().join("data.tsv");
    let out_csv_dir = temp.path().join("csv_svg_out");
    let out_tsv_dir = temp.path().join("tsv_svg_out");

    let csv_content = "Name,Role,Score\nAlice,Engineer,95\nBob,Designer,88\nCharlie,Product,92\n";
    fs::write(&csv_path, csv_content)?;

    let tsv_content = "Item\tCategory\tPrice\nApple\tFruit\t150\nCarrot\tVegetable\t100\n";
    fs::write(&tsv_path, tsv_content)?;

    // 1. Convert CSV to SVG
    let csv_report = convert_path(&csv_path, &out_csv_dir, &ConvertOptions::default())?;
    assert_eq!(csv_report.page_count, 1);
    let csv_svg = fs::read_to_string(out_csv_dir.join("page-0001.svg"))?;
    assert!(csv_svg.contains("Alice"));
    assert!(csv_svg.contains("Engineer"));
    assert!(csv_svg.contains("95"));

    // 2. Convert TSV to SVG
    let tsv_report = convert_path(&tsv_path, &out_tsv_dir, &ConvertOptions::default())?;
    assert_eq!(tsv_report.page_count, 1);
    let tsv_svg = fs::read_to_string(out_tsv_dir.join("page-0001.svg"))?;
    assert!(tsv_svg.contains("Apple"));
    assert!(tsv_svg.contains("Fruit"));
    assert!(tsv_svg.contains("150"));

    Ok(())
}

#[test]
fn test_csv_quote_roundtrip_and_bounded_row_column_pagination()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let source = "Name,Note\r\n\" Acme, Inc. \",\"She said \"\"hello\"\"\"\r\n";
    let csv_path = temp.path().join("quoted.csv");
    let output = temp.path().join("quoted-svg");
    fs::write(&csv_path, source)?;
    let report = convert_path(&csv_path, &output, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg"))?;
    assert!(svg.contains(" Acme, Inc. "));
    assert!(svg.contains("She said \"hello\""));

    let restored = temp.path().join("roundtrip.csv");
    svg_to_document(
        output.join("page-0001.svg"),
        &restored,
        &ReverseOptions::default(),
    )?;
    assert_eq!(fs::read_to_string(restored)?, source);

    let mut headers = (0..33)
        .map(|index| format!("Field{index}"))
        .collect::<Vec<_>>();
    let mut lines = vec![headers.join(",")];
    for row in 1..=101 {
        headers.iter_mut().enumerate().for_each(|(column, value)| {
            *value = format!("row{row}-col{column}");
        });
        lines.push(headers.join(","));
    }
    let wide_path = temp.path().join("wide.csv");
    let wide_output = temp.path().join("wide-svg");
    fs::write(&wide_path, lines.join("\n"))?;
    let wide_report = convert_path(&wide_path, &wide_output, &ConvertOptions::default())?;
    assert_eq!(wide_report.page_count, 4);
    assert!(
        wide_report
            .warnings
            .iter()
            .any(|warning| warning.contains("not embedded"))
    );
    assert!(fs::read_to_string(wide_output.join("page-0001.svg"))?.contains("row100-col0"));
    assert!(fs::read_to_string(wide_output.join("page-0002.svg"))?.contains("row1-col32"));
    assert!(fs::read_to_string(wide_output.join("page-0003.svg"))?.contains("row101-col0"));
    assert!(fs::read_to_string(wide_output.join("page-0004.svg"))?.contains("row101-col32"));

    let rejected_output = temp.path().join("wide-one-page");
    let one_page = ConvertOptions {
        max_pages: 3,
        ..ConvertOptions::default()
    };
    assert!(convert_path(&wide_path, &rejected_output, &one_page).is_err());
    assert!(!rejected_output.join("page-0001.svg").exists());
    Ok(())
}

#[test]
fn test_excalidraw_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let excalidraw_path = temp.path().join("drawing.excalidraw");
    let out_dir = temp.path().join("excalidraw_out");

    let excalidraw_json = r##"{
  "type": "excalidraw",
  "version": 2,
  "source": "https://excalidraw.com",
  "elements": [
    {
      "id": "box1",
      "type": "rectangle",
      "x": 100,
      "y": 100,
      "width": 200,
      "height": 120,
      "strokeColor": "#1e1e1e",
      "backgroundColor": "#a5d8ff",
      "strokeWidth": 2
    },
    {
      "id": "text1",
      "type": "text",
      "x": 120,
      "y": 140,
      "width": 160,
      "height": 40,
      "text": "System Architecture",
      "fontSize": 20,
      "strokeColor": "#000000"
    }
  ]
}"##;
    fs::write(&excalidraw_path, excalidraw_json)?;

    let report = convert_path(&excalidraw_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert_eq!(report.source_format, document_svg::SourceFormat::Excalidraw);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("id=\"box1\""));
    assert!(svg.contains("System Architecture"));
    Ok(())
}

#[test]
fn test_plantuml_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let puml_path = temp.path().join("sequence.puml");
    let out_dir = temp.path().join("puml_out");

    let puml_content = r#"@startuml
participant Client
participant "Auth Server" as Auth
Client -> Auth : Login Request
Auth --> Client : JWT Token
@enduml
"#;
    fs::write(&puml_path, puml_content)?;

    let report = convert_path(&puml_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert_eq!(report.source_format, document_svg::SourceFormat::PlantUml);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Client"));
    assert!(svg.contains("Login Request"));
    assert!(svg.contains("JWT Token"));
    Ok(())
}

#[test]
fn test_d2_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let d2_path = temp.path().join("arch.d2");
    let out_dir = temp.path().join("d2_out");

    let d2_content = r#"
client -> load_balancer: HTTPS traffic
load_balancer -> backend: route request
backend -> db: SQL query
"#;
    fs::write(&d2_path, d2_content)?;

    let report = convert_path(&d2_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert_eq!(report.source_format, document_svg::SourceFormat::D2);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("client"));
    assert!(svg.contains("load_balancer"));
    assert!(svg.contains("HTTPS traffic"));
    Ok(())
}

#[test]
fn test_html_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let html_path = temp.path().join("article.html");
    let out_dir = temp.path().join("html_out");

    let html_content = r#"<!DOCTYPE html>
<html>
<head><title>Test Article</title></head>
<body>
  <h1>Introduction to Vector Graphics</h1>
  <p>Scalable Vector Graphics is an XML-based format for two-dimensional images.</p>
  <ul>
    <li>Resolution independence</li>
    <li>Interactive scriptability</li>
    <li>Universal browser support</li>
  </ul>
  <pre><code>let svg = document.createElement('svg');</code></pre>
  <hr />
</body>
</html>
"#;
    fs::write(&html_path, html_content)?;

    let report = convert_path(&html_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert_eq!(report.source_format, document_svg::SourceFormat::Html);

    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Introduction to Vector Graphics"));
    assert!(svg.contains("Scalable Vector Graphics is an XML-based format"));
    assert!(svg.contains("Resolution independence"));
    assert!(svg.contains("let svg = document.createElement"));
    Ok(())
}

#[test]
fn test_epub_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let temp = tempdir()?;
    let epub_path = temp.path().join("book.epub");
    let out_dir = temp.path().join("epub_out");

    let file = fs::File::create(&epub_path)?;
    let mut zip = ZipWriter::new(file);

    // 1. mimetype
    zip.start_file("mimetype", SimpleFileOptions::default())?;
    zip.write_all(b"application/epub+zip")?;

    // 2. META-INF/container.xml
    zip.start_file("META-INF/container.xml", SimpleFileOptions::default())?;
    let container_xml = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;
    zip.write_all(container_xml.as_bytes())?;

    // 3. OEBPS/content.opf
    zip.start_file("OEBPS/content.opf", SimpleFileOptions::default())?;
    let opf_xml = r#"<?xml version="1.0"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf" unique-identifier="pub-id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Sample eBook</dc:title>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="missing" href="missing.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
    <itemref idref="missing"/>
  </spine>
</package>"#;
    zip.write_all(opf_xml.as_bytes())?;

    // 4. OEBPS/ch1.xhtml
    zip.start_file("OEBPS/ch1.xhtml", SimpleFileOptions::default())?;
    let ch1_html = r#"<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Chapter 1</title></head>
<body>
  <h1>Chapter 1: The Beginning</h1>
  <p>Once upon a time in the digital realm of vector rendering...</p>
</body>
</html>"#;
    zip.write_all(ch1_html.as_bytes())?;

    zip.finish()?;

    let report = convert_path(&epub_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    assert_eq!(report.source_format, document_svg::SourceFormat::Epub);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("missing.xhtml"))
    );

    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Chapter 1: The Beginning"));
    assert!(svg.contains("Once upon a time in the digital realm"));

    let bounded_options = ConvertOptions {
        max_xml_events: 2,
        ..ConvertOptions::default()
    };
    let bounded_error = convert_path(
        &epub_path,
        temp.path().join("epub_limited_out"),
        &bounded_options,
    )
    .unwrap_err();
    assert!(matches!(
        bounded_error,
        document_svg::Error::LimitExceeded(_)
    ));

    Ok(())
}

#[test]
fn test_odt_to_svg_preserves_headings_lists_tables_and_reports_omitted_images()
-> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let temp = tempdir()?;
    let input = temp.path().join("report.odt");
    let output = temp.path().join("odt_out");
    let file = fs::File::create(&input)?;
    let mut zip = ZipWriter::new(file);
    zip.start_file("mimetype", SimpleFileOptions::default())?;
    zip.write_all(b"application/vnd.oasis.opendocument.text")?;
    zip.start_file("content.xml", SimpleFileOptions::default())?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0">
  <office:body><office:text>
    <text:h text:outline-level="1">OpenDocument Report</text:h>
    <text:p>Research &amp; engineering text with<text:s text:c="2"/>spacing.</text:p>
    <text:list><text:list-item><text:p>First item</text:p></text:list-item><text:list-item><text:p>Second item</text:p></text:list-item></text:list>
    <table:table table:name="Metrics"><table:table-header-rows><table:table-row><table:table-cell><text:p>Metric</text:p></table:table-cell><table:table-cell><text:p>Value</text:p></table:table-cell></table:table-row></table:table-header-rows><table:table-row><table:table-cell><text:p>Temperature</text:p></table:table-cell><table:table-cell><text:p>24</text:p></table:table-cell></table:table-row></table:table>
    <text:p>Photo <draw:image xlink:href="Pictures/photo.png" xmlns:xlink="http://www.w3.org/1999/xlink"/></text:p>
  </office:text></office:body>
</office:document-content>"#,
    )?;
    zip.finish()?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, document_svg::SourceFormat::Odt);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("missing ODT image parts were omitted"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg"))?;
    assert!(svg.contains("OpenDocument Report"));
    assert!(svg.contains("Research &amp; engineering"));
    assert!(svg.contains("First item"));
    assert!(svg.contains("Temperature"));

    let bounded = ConvertOptions {
        max_xml_events: 1,
        ..ConvertOptions::default()
    };
    let error = convert_path(&input, temp.path().join("odt_limited_out"), &bounded).unwrap_err();
    assert!(matches!(error, document_svg::Error::LimitExceeded(_)));
    Ok(())
}

#[test]
fn test_fodt_flat_xml_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("notes.fodt");
    let output = temp.path().join("fodt_out");
    fs::write(
        &input,
        r#"<?xml version="1.0"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:text><text:h text:outline-level="2">Flat OpenDocument</text:h><text:p>One page without a ZIP container.</text:p></office:text></office:body></office:document-content>"#,
    )?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, document_svg::SourceFormat::Odt);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg"))?;
    assert!(svg.contains("Flat OpenDocument"));
    assert!(svg.contains("One page without a ZIP container"));
    Ok(())
}

#[test]
fn test_ods_to_svg_uses_cached_cells_and_warns_about_formula_calculation()
-> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let temp = tempdir()?;
    let input = temp.path().join("sales.ods");
    let output = temp.path().join("ods_out");
    let mut package = ZipWriter::new(fs::File::create(&input)?);
    package.start_file("mimetype", SimpleFileOptions::default())?;
    package.write_all(b"application/vnd.oasis.opendocument.spreadsheet")?;
    package.start_file("content.xml", SimpleFileOptions::default())?;
    package.write_all(
        br#"<?xml version="1.0"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
 <office:body><office:spreadsheet><table:table table:name="Quarterly Sales">
  <table:table-header-rows><table:table-row><table:table-cell office:value-type="string" office:string-value="Product"><text:p>Product</text:p></table:table-cell><table:table-cell office:value-type="string" office:string-value="Units"><text:p>Units</text:p></table:table-cell></table:table-row></table:table-header-rows>
  <table:table-row><table:table-cell office:value-type="string" office:string-value="Widget"><text:p>Widget</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="42"><text:p>42</text:p></table:table-cell></table:table-row>
  <table:table-row><table:table-cell office:value-type="string" office:string-value="Formula"><text:p>Formula</text:p></table:table-cell><table:table-cell table:formula="of:=SUM([.B2:.B2])" office:value-type="float" office:value="42"><text:p>42</text:p></table:table-cell></table:table-row>
 </table:table></office:spreadsheet></office:body>
</office:document-content>"#,
    )?;
    package.finish()?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, document_svg::SourceFormat::Ods);
    assert_eq!(report.page_count, 1);
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("not recalculated"))
    );
    let svg = fs::read_to_string(output.join("page-0001.svg"))?;
    assert!(svg.contains("Quarterly Sales"));
    assert!(svg.contains("Widget"));
    assert!(svg.contains("42"));
    Ok(())
}

#[test]
fn test_fods_flat_spreadsheet_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let input = temp.path().join("metrics.fods");
    let output = temp.path().join("fods_out");
    fs::write(
        &input,
        r#"<?xml version="1.0"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:spreadsheet><table:table table:name="Flat Metrics"><table:table-row><table:table-cell office:value-type="string"><text:p>Metric</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Value</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell office:value-type="string"><text:p>Nodes</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="6"><text:p>6</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#,
    )?;

    let report = convert_path(&input, &output, &ConvertOptions::default())?;
    assert_eq!(report.source_format, document_svg::SourceFormat::Ods);
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(output.join("page-0001.svg"))?;
    assert!(svg.contains("Flat Metrics"));
    assert!(svg.contains("Nodes"));
    assert!(svg.contains("6"));
    Ok(())
}

#[test]
fn test_markdown_table_escaped_pipe_cells() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("pipes.md");
    let out_dir = temp.path().join("pipes_out");
    let md_content = "| Operation | Syntax | Description |\n| :--- | :---: | ---: |\n| Bitwise OR | `a \\| b` | Computes bitwise OR |\n| Logical OR | `a \\|\\| b` | Short-circuit boolean OR |\n";
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Bitwise OR"));
    assert!(svg.contains("Logical OR"));
    Ok(())
}

#[test]
fn test_chart_missing_labels_renders_all_points() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let chart_path = temp.path().join("no_labels.chart");
    let out_dir = temp.path().join("no_labels_out");
    let chart_json = r#"{
        "type": "bar",
        "title": "Unlabeled Data",
        "labels": [],
        "series": [
            { "name": "Metric", "data": [10.0, 25.0, 42.0, 31.0] }
        ]
    }"#;
    fs::write(&chart_path, chart_json)?;

    let report = convert_path(&chart_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("<path"));
    Ok(())
}

#[test]
fn test_mermaid_edge_case_tokens() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let mmd_path = temp.path().join("tokens.mmd");
    let out_dir = temp.path().join("tokens_out");
    let mmd_content =
        "flowchart TD\n  A[(Database)] --> B[Process]\n  C{Decision} --- D(End)\n  E) ( --> F] [\n";
    fs::write(&mmd_path, mmd_content)?;

    let report = convert_path(&mmd_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Database"));
    assert!(svg.contains("Process"));
    Ok(())
}

#[test]
fn test_html_table_rendering() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let html_path = temp.path().join("report.html");
    let out_dir = temp.path().join("html_table_out");
    let html_content = r#"<!DOCTYPE html>
<html>
<head><title>System Status</title></head>
<body>
  <h1>System Overview</h1>
  <p>Status report of running microservices.</p>
  <table>
    <thead>
      <tr>
        <th>Service</th>
        <th>Region</th>
        <th>Uptime</th>
      </tr>
    </thead>
    <tbody>
      <tr>
        <td>Auth API</td>
        <td>us-east-1</td>
        <td>99.98%</td>
      </tr>
      <tr>
        <td>Worker Queue</td>
        <td>eu-west-1</td>
        <td>100.0%</td>
      </tr>
    </tbody>
  </table>
</body>
</html>"#;
    fs::write(&html_path, html_content)?;

    let report = convert_path(&html_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("System Overview"));
    assert!(svg.contains("Auth API"));
    assert!(svg.contains("Worker Queue"));
    assert!(svg.contains("us-east-1"));
    assert!(svg.contains("tbl_hdr_bg"));
    assert!(svg.contains("tbl_row_bg"));
    Ok(())
}

#[test]
fn test_cjk_typography_wrapping() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let html_path = temp.path().join("japanese.html");
    let out_dir = temp.path().join("cjk_out");
    let html_content = r#"<!DOCTYPE html>
<html>
<body>
  <h2>日本語ドキュメントの禁則処理テスト</h2>
  <p>DocSVGはRustで開発された高速かつ安全なドキュメントSVG変換エンジンです。日本語の約物や句読点（。や、など）が行頭に来ないように禁則処理を行い、さらにアルファベットや英単語との混在文でも自然で美しいタイポグラフィを実現します。</p>
</body>
</html>"#;
    fs::write(&html_path, html_content)?;

    let report = convert_path(&html_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("日本語ドキュメントの禁則処理テスト"));
    assert!(svg.contains("DocSVG"));
    assert!(svg.contains("Rust"));
    // Verify that multiple text elements were generated (wrapped over multiple lines)
    let p_count = svg.matches("id=\"p_").count();
    assert!(
        p_count >= 2,
        "expected paragraph to wrap into multiple lines, got {p_count}"
    );
    Ok(())
}

#[test]
fn test_rich_markdown_document_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("spec.md");
    let out_dir = temp.path().join("spec_out");
    let md_content = r#"# Product Specification

This is a comprehensive specification document written in Markdown.

## Architecture

> Notice: All endpoints require mTLS authentication.

Here are the key components:
- API Gateway
- Event Bus
- Storage Engine

```rust
fn main() {
    println!("DocSVG typesetter active!");
}
```

### Metrics Table

| Metric | Target | Current |
| :--- | :---: | ---: |
| Latency | < 50ms | 18ms |
| Throughput | 10k rps | 14.5k rps |

---
End of report.
"#;
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Product Specification"));
    assert!(svg.contains("Architecture"));
    assert!(svg.contains("API Gateway"));
    assert!(svg.contains("DocSVG typesetter active!"));
    assert!(svg.contains("Latency"));
    assert!(svg.contains("14.5k rps"));
    assert!(svg.contains("End of report."));
    Ok(())
}

#[test]
fn test_asciidoc_to_svg() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let adoc_path = temp.path().join("manual.adoc");
    let out_dir = temp.path().join("adoc_out");
    let adoc_content = r#"= Architecture Manual

== Introduction

DocSVG converts technical documentation directly to vector SVG pages.

* Fast single-pass pagination
* Zero external system dependencies
* Clean vector font typesetting

. Numbered item 1
. Numbered item 2

[source,rust]
----
pub fn render() -> Result<()> {
    Ok(())
}
----

'''
AsciiDoc rendering complete.
"#;
    fs::write(&adoc_path, adoc_content)?;

    let report = convert_path(&adoc_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Architecture Manual"));
    assert!(svg.contains("Introduction"));
    assert!(svg.contains("Fast single-pass pagination"));
    assert!(svg.contains("Numbered item 1"));
    assert!(svg.contains("pub fn render"));
    assert!(svg.contains("AsciiDoc rendering complete."));
    Ok(())
}

#[test]
fn test_latex_math_accents_and_operators() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let tex_path = temp.path().join("formula.tex");
    let out_dir = temp.path().join("formula_out");
    let formula =
        r#"\vec{F} = m \vec{a} + \mathbb{R} \le \mathbb{C} \land x \equiv y \pm \sqrt{z}"#;
    fs::write(&tex_path, formula)?;

    let report = convert_path(&tex_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("ℝ"));
    assert!(svg.contains("ℂ"));
    assert!(svg.contains("≤"));
    assert!(svg.contains("∧"));
    assert!(svg.contains("≡"));
    assert!(svg.contains("±"));
    Ok(())
}

#[test]
fn test_chart_negative_values_and_zero_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let chart_path = temp.path().join("profit_loss.chart.json");
    let out_dir = temp.path().join("chart_neg_out");
    let chart_json = r##"{
        "type": "bar",
        "title": "Profit / Loss",
        "labels": ["Q1", "Q2", "Q3", "Q4"],
        "series": [
            {
                "name": "EBITDA",
                "data": [-25.0, 40.0, -15.0, 35.0],
                "color": "#ef4444"
            }
        ]
    }"##;
    fs::write(&chart_path, chart_json)?;

    let report = convert_path(&chart_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Profit / Loss"));
    assert!(svg.contains("Q1"));
    assert!(svg.contains("Q4"));
    assert!(svg.contains("-25") || svg.contains("-30") || svg.contains("-20"));
    assert!(svg.contains("<path"));
    Ok(())
}

#[test]
fn test_html_clean_head_and_container_rendering() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let html_path = temp.path().join("document.html");
    let out_dir = temp.path().join("html_clean_out");
    let html_content = r#"<!DOCTYPE html>
<html>
<head>
  <title>Ignore Page Title</title>
  <style>
    body { margin: 0; background: #fafafa; }
    .hidden { display: none; }
  </style>
  <script>
    console.log("ignore script");
    alert("danger");
  </script>
</head>
<body>
  <h1>Executive Summary</h1>
  <div>
    This paragraph is directly inside a div container.
  </div>
  <p>Standard paragraph element.</p>
  <section>
    Section content following paragraph.
  </section>
  <style>
    p { color: #333; }
  </style>
  Trailing loose paragraph text.
</body>
</html>"#;
    fs::write(&html_path, html_content)?;

    let report = convert_path(&html_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    // Verify visual text is present
    assert!(svg.contains("Executive Summary"));
    assert!(svg.contains("This paragraph is directly inside a div container."));
    assert!(svg.contains("Standard paragraph element."));
    assert!(svg.contains("Section content following paragraph."));
    assert!(svg.contains("Trailing loose paragraph text."));

    // Verify head, styles, and scripts are stripped
    assert!(!svg.contains("margin: 0"));
    assert!(!svg.contains("console.log"));
    assert!(!svg.contains("alert("));
    assert!(!svg.contains("Ignore Page Title"));
    assert!(!svg.contains("color: #333"));
    Ok(())
}

#[test]
fn test_html_table_colspan_and_alignments() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let html_path = temp.path().join("table_colspan.html");
    let out_dir = temp.path().join("html_table_out");
    let html_content = r#"<table>
  <thead>
    <tr>
      <th colspan="2">Category & Item</th>
      <th align="right">Amount</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td>Hardware</td>
      <td>Server Chassis</td>
      <td style="text-align: right;">12500</td>
    </tr>
    <tr>
      <td>Cloud</td>
      <td>Virtual Machines</td>
      <td>4820</td>
    </tr>
  </tbody>
</table>"#;
    fs::write(&html_path, html_content)?;

    let report = convert_path(&html_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    assert!(svg.contains("Category &amp; Item") || svg.contains("Category & Item"));
    assert!(svg.contains("Server Chassis"));
    assert!(svg.contains("12500"));
    assert!(svg.contains("4820"));
    // Ensure right alignment is used for numeric or aligned column
    assert!(svg.contains("text-anchor=\"end\""));
    Ok(())
}

#[test]
fn test_markdown_setext_headings_and_embedded_tables() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("setext_and_table.md");
    let out_dir = temp.path().join("md_setext_out");
    let md_content = r#"System Architecture
===================

Component Overview
------------------

Introduction to the system pipeline.

| Module | Expression | Latency |
| :--- | :---: | ---: |
| Core A | `a \| b` | 12 ms |
| Core B | `x \| y` | 24 ms |

Trailing notes for the engineering team.
"#;
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    // Setext headings should be rendered without equal/dash signs
    assert!(svg.contains("System Architecture"));
    assert!(!svg.contains("==================="));
    assert!(svg.contains("Component Overview"));
    assert!(!svg.contains("------------------"));

    // Table with escaped pipes inside code block
    assert!(svg.contains("Core A"));
    assert!(svg.contains("Core B"));
    assert!(svg.contains("12 ms"));

    // Trailing notes
    assert!(svg.contains("Trailing notes for the engineering team."));
    Ok(())
}

#[test]
fn test_d2_container_properties_no_phantom_nodes() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let d2_path = temp.path().join("arch.d2");
    let out_dir = temp.path().join("d2_out");
    let d2_content = r##"api: "Backend API"
db: "PostgreSQL Database" {
  shape: cylinder
  style.fill: "#3b82f6"
}
api -> db: "SQL Query"
"##;
    fs::write(&d2_path, d2_content)?;

    let report = convert_path(&d2_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    // Must contain actual nodes and edge
    assert!(svg.contains("Backend API"));
    assert!(svg.contains("PostgreSQL Database"));
    assert!(svg.contains("SQL Query"));

    // Must NOT contain phantom nodes named "shape" or "style"
    assert!(!svg.contains(">shape<"));
    assert!(!svg.contains(">style.fill<"));
    assert!(!svg.contains(">style<"));
    Ok(())
}

#[test]
fn test_mermaid_state_diagram_and_class_relations() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let mmd_path = temp.path().join("state.mmd");
    let out_dir = temp.path().join("mmd_state_out");
    let mmd_content = r#"stateDiagram-v2
    [*] --> Idle
    Idle --> Processing : submit_job
    Processing --> Success : job_done
    Processing --> [*] : cancel
"#;
    fs::write(&mmd_path, mmd_content)?;

    let report = convert_path(&mmd_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    assert!(svg.contains("Idle"));
    assert!(svg.contains("Processing"));
    assert!(svg.contains("submit_job"));
    assert!(svg.contains("job_done"));
    assert!(svg.contains("●")); // State start/end pseudo-state circle
    assert!(!svg.contains(">stateDiagram-v2<")); // Header must not be a visual node

    // Class diagram with inheritance arrow
    let class_path = temp.path().join("class.mmd");
    let class_out = temp.path().join("mmd_class_out");
    let class_content = r#"classDiagram
    BaseService <|-- AuthServiceImpl
    AuthServiceImpl : authenticate()
"#;
    fs::write(&class_path, class_content)?;
    let report2 = convert_path(&class_path, &class_out, &ConvertOptions::default())?;
    assert_eq!(report2.page_count, 1);
    let svg2 = fs::read_to_string(class_out.join("page-0001.svg"))?;
    assert!(svg2.contains("BaseService"));
    assert!(svg2.contains("AuthServiceImpl"));
    Ok(())
}

#[test]
fn test_table_financial_and_currency_alignment() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let csv_path = temp.path().join("financial.csv");
    let out_dir = temp.path().join("csv_fin_out");
    let csv_content = "Quarter,Revenue,Operating Income,Margin\nQ1,\"$1,250,000.00\",\"$340,500.00\",27.2%\nQ2,\"$1,480,200.00\",\"($50,000.00)\",-3.4%\nQ3,\"¥250,000\",\"¥45,000\",18.0%\n";
    fs::write(&csv_path, csv_content)?;

    let report = convert_path(&csv_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    assert!(svg.contains("$1,250,000.00"));
    assert!(svg.contains("27.2%"));
    assert!(svg.contains("($50,000.00)"));
    assert!(svg.contains("text-anchor=\"end\"")); // Numeric/currency columns auto right-aligned

    // AsciiDoc table auto-alignment
    let adoc_path = temp.path().join("spec_prices.adoc");
    let adoc_out = temp.path().join("adoc_fin_out");
    let adoc_content = r#"= Pricing Specification

|===
| SKU | Item Description | Price | Discount

| A-101 | Cloud Standard | $49.99 | 10%
| B-202 | Enterprise Node | $299.00 | 25%
|===
"#;
    fs::write(&adoc_path, adoc_content)?;
    let report_adoc = convert_path(&adoc_path, &adoc_out, &ConvertOptions::default())?;
    assert_eq!(report_adoc.page_count, 1);
    let svg_adoc = fs::read_to_string(adoc_out.join("page-0001.svg"))?;
    assert!(svg_adoc.contains("Cloud Standard"));
    assert!(svg_adoc.contains("$49.99"));
    assert!(svg_adoc.contains("text-anchor=\"end\""));
    Ok(())
}

#[test]
fn test_plantuml_multi_role_participants() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let puml_path = temp.path().join("roles.puml");
    let out_dir = temp.path().join("puml_roles_out");
    let puml_content = r#"@startuml
boundary WebClient
control Gateway
queue TaskQueue
database PrimaryDB

WebClient -> Gateway : POST /orders
Gateway -> TaskQueue : push(order)
Gateway -> PrimaryDB : SELECT status
@enduml
"#;
    fs::write(&puml_path, puml_content)?;

    let report = convert_path(&puml_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    assert!(svg.contains("WebClient"));
    assert!(svg.contains("Gateway"));
    assert!(svg.contains("TaskQueue"));
    assert!(svg.contains("PrimaryDB"));
    assert!(svg.contains("POST /orders"));
    assert!(svg.contains("push(order)"));
    Ok(())
}

#[test]
fn test_step_circle_and_closed_loop_projection() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let step_path = temp.path().join("cylinder_hole.step");
    let out_dir = temp.path().join("step_hole_out");
    let step_content = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('STEP AP203 Cylinder Hole'),'2;1');
FILE_NAME('cylinder_hole.step','2026-09-12',('Engineer'),('Test'),'','','');
FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));
ENDSEC;
DATA;
#10 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
#11 = DIRECTION('', (0.0, 0.0, 1.0));
#12 = DIRECTION('', (1.0, 0.0, 0.0));
#13 = AXIS2_PLACEMENT_3D('', #10, #11, #12);
#14 = CIRCLE('', #13, 50.0);
#15 = CARTESIAN_POINT('', (50.0, 0.0, 0.0));
#16 = VERTEX_POINT('', #15);
#17 = EDGE_CURVE('', #16, #16, #14, .T.);
ENDSEC;
END-ISO-10303-21;
"#;
    fs::write(&step_path, step_content)?;

    let report = convert_path(&step_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("<svg"));
    assert!(svg.contains("id=\"step-edges\""));
    // Closed circular edge should be sampled into 32 polygonal wireframe segments
    assert!(svg.contains("M "));
    Ok(())
}

#[test]
fn test_chart_doughnut_annular_slices_and_marker_alignment()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let chart_path = temp.path().join("usage_donut.chart.json");
    let out_dir = temp.path().join("donut_out");
    let chart_json = r#"{
        "type": "doughnut",
        "title": "Storage Allocations",
        "labels": ["Block", "Object", "File"],
        "series": [
            {"name": "Capacity", "data": [50.0, 30.0, 20.0]}
        ]
    }"#;
    fs::write(&chart_path, chart_json)?;

    let report = convert_path(&chart_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("Storage Allocations"));
    assert!(svg.contains("Block: 50 (50.0%)"));

    // Line chart negative/positive marker alignment
    let line_path = temp.path().join("delta.chart.json");
    let line_out = temp.path().join("line_marker_out");
    let line_json = r#"{
        "type": "line",
        "title": "Profit Margin",
        "labels": ["Jan", "Feb", "Mar"],
        "series": [
            {"name": "Margin", "data": [-15.0, 0.0, 25.0]}
        ]
    }"#;
    fs::write(&line_path, line_json)?;
    let report_line = convert_path(&line_path, &line_out, &ConvertOptions::default())?;
    assert_eq!(report_line.page_count, 1);
    let line_svg = fs::read_to_string(line_out.join("page-0001.svg"))?;
    assert!(line_svg.contains("Profit Margin"));
    Ok(())
}

#[test]
fn test_metafile_sniffing_and_dimension_units() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    // Extensionless file with EMF magic header
    let emf_bin_path = temp.path().join("sample_metafile_binary");
    let mut emf_header = vec![0u8; 88];
    // Record type: 1 (EMR_HEADER)
    emf_header[0..4].copy_from_slice(&1u32.to_le_bytes());
    // Record size: 88
    emf_header[4..8].copy_from_slice(&88u32.to_le_bytes());
    // Signature: " EMF" at 40..44
    emf_header[40..44].copy_from_slice(b" EMF");
    fs::write(&emf_bin_path, &emf_header)?;

    let detected = SourceFormat::detect(&emf_bin_path)?;
    assert_eq!(detected, SourceFormat::Emf);

    // APM WMF header key
    let wmf_bin_path = temp.path().join("sample_wmf_binary");
    let mut wmf_header = vec![0u8; 22];
    wmf_header[0..4].copy_from_slice(&[0xD7, 0xCD, 0xC6, 0x9A]);
    fs::write(&wmf_bin_path, &wmf_header)?;
    let detected_wmf = SourceFormat::detect(&wmf_bin_path)?;
    assert_eq!(detected_wmf, SourceFormat::Emf);
    Ok(())
}

#[test]
fn test_latex_matrix_and_cases_environments() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let tex_path = temp.path().join("matrix.tex");
    let out_dir = temp.path().join("matrix_out");
    let tex_content = r#"\begin{pmatrix} a & b \\ c & d \end{pmatrix} + \binom{n}{k} = \begin{cases} x & \text{if } x > 0 \\ 0 & \text{otherwise} \end{cases}"#;
    fs::write(&tex_path, tex_content)?;

    let report = convert_path(&tex_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    // Should NOT contain literal "begin" or "pmatrix"
    assert!(!svg.contains(">begin<"));
    assert!(!svg.contains(">pmatrix<"));
    // Cells should be rendered
    assert!(svg.contains(">a<"));
    assert!(svg.contains(">b<"));
    assert!(svg.contains(">c<"));
    assert!(svg.contains(">d<"));
    assert!(svg.contains(">otherwise<"));
    // Delimiter paths should be rendered
    assert!(svg.contains("<path"));

    // Reverse extraction of the exact latex source
    let reverse_out = temp.path().join("restored.tex");
    svg_to_document(
        out_dir.join("page-0001.svg"),
        &reverse_out,
        &ReverseOptions::default(),
    )?;
    let restored = fs::read_to_string(&reverse_out)?;
    assert!(restored.contains(r#"\begin{pmatrix}"#));
    assert!(restored.contains(r#"\binom{n}{k}"#));
    Ok(())
}

#[test]
fn test_epub_multi_chapter_page_breaks() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let temp = tempdir()?;
    let epub_path = temp.path().join("multi_chapter.epub");
    let out_dir = temp.path().join("epub_multi_out");

    let file = fs::File::create(&epub_path)?;
    let mut zip = zip::ZipWriter::new(file);

    zip.start_file(
        "mimetype",
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
    )?;
    zip.write_all(b"application/epub+zip")?;

    zip.start_file("META-INF/container.xml", SimpleFileOptions::default())?;
    let container_xml = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;
    zip.write_all(container_xml.as_bytes())?;

    zip.start_file("OEBPS/content.opf", SimpleFileOptions::default())?;
    let opf_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="pub-id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Multi-Chapter Book</dc:title>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="ch2" href="ch2.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="ch1"/>
    <itemref idref="ch2"/>
  </spine>
</package>"#;
    zip.write_all(opf_xml.as_bytes())?;

    zip.start_file("OEBPS/ch1.xhtml", SimpleFileOptions::default())?;
    let ch1_html = r#"<!DOCTYPE html><html><body><h1>Chapter 1: Dawn</h1><p>The journey begins here.</p></body></html>"#;
    zip.write_all(ch1_html.as_bytes())?;

    zip.start_file("OEBPS/ch2.xhtml", SimpleFileOptions::default())?;
    let ch2_html = r#"<!DOCTYPE html><html><body><h1>Chapter 2: Twilight</h1><p>The journey continues into night.</p></body></html>"#;
    zip.write_all(ch2_html.as_bytes())?;

    zip.finish()?;

    let report = convert_path(&epub_path, &out_dir, &ConvertOptions::default())?;
    // Chapter 2 must start on a new page (Page 2) due to PageBreak separation
    assert_eq!(report.page_count, 2);
    let p1 = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    let p2 = fs::read_to_string(out_dir.join("page-0002.svg"))?;
    assert!(p1.contains("Chapter 1: Dawn"));
    assert!(p2.contains("Chapter 2: Twilight"));
    Ok(())
}

#[test]
fn test_gcode_full_circle_and_major_arcs() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let nc_path = temp.path().join("circles.nc");
    let out_dir = temp.path().join("nc_circles_out");

    // G02 with full 360 circle (from == to) and major arc (> 180 deg)
    let nc_content = r#"G21 G90
G00 X50.0 Y50.0
G02 X50.0 Y50.0 I20.0 J0.0
G00 X0.0 Y50.0
G02 X50.0 Y0.0 I0.0 J-50.0
M02
"#;
    fs::write(&nc_path, nc_content)?;

    let report = convert_path(&nc_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    // Full 360-degree circle must generate two consecutive 180-degree arcs to be visible in SVG
    assert!(svg.contains(" A "));
    // Major arc (> 180 deg) must set large-arc-flag to 1
    assert!(svg.contains(" 0 1 0 ") || svg.contains(" 0 1 1 "));
    Ok(())
}

#[test]
fn test_step_polyline_wireframe() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let step_path = temp.path().join("wireframe.step");
    let out_dir = temp.path().join("step_poly_out");

    let step_content = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('Polyline sample'),'2;1');
FILE_NAME('wire.step','2026-09-12',('User'),(''),'','','');
FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));
ENDSEC;
DATA;
#10 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
#11 = CARTESIAN_POINT('', (100.0, 0.0, 0.0));
#12 = CARTESIAN_POINT('', (100.0, 100.0, 0.0));
#13 = CARTESIAN_POINT('', (0.0, 100.0, 0.0));
#20 = POLYLINE('profile', (#10, #11, #12, #13, #10));
ENDSEC;
END-ISO-10303-21;
"#;
    fs::write(&step_path, step_content)?;

    let report = convert_path(&step_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    // Wireframe polyline segments should be projected
    assert!(svg.contains("<path"));
    assert!(svg.contains("iso:edge") || svg.contains("stroke="));
    Ok(())
}

#[test]
fn test_iges_copious_data_form2() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let igs_path = temp.path().join("planar.igs");
    let out_dir = temp.path().join("igs_out");

    // Standard 80-column ASCII IGES with Form 2 copious data (IP=2, shared Z)
    let s_line = format!(
        "{:<72}S{:>7}\n",
        "Test IGES file with Form 2 Copious Data", 1
    );
    let g_line = format!(
        "{:<72}G{:>7}\n",
        "1H,,1H;,4HTEST,8Htest.igs,1.,1,1,1.,1,1.,1.,0.,1.,4HTEST;", 1
    );
    let d1 = format!(
        "{:<8}{:<8}{:<8}{:<8}{:<8}{:<8}{:<8}{:<8}{:<8}D{:>7}\n",
        "106", "1", "0", "1", "0", "0", "0", "0", "0", 1
    );
    let d2 = format!(
        "{:<8}{:<8}{:<8}{:<8}{:<8}{:<8}{:<8}{:<8}{:<8}D{:>7}\n",
        "106", "0", "0", "1", "0", "0", "0", "0", "0", 2
    );
    // Entity 106, Form 2 (IP=2), N=4 points, Z=10.0, followed by X1,Y1, X2,Y2, X3,Y3, X4,Y4
    let p1 = format!(
        "{:<72}P{:>7}\n",
        "106,2,4,10.0,0.0,0.0,100.0,0.0,100.0,50.0,0.0,50.0;", 1
    );
    let t_line = format!("S{:>7}G{:>7}D{:>7}P{:>7}{:<40}T{:>7}\n", 1, 1, 2, 1, "", 1);

    let mut iges_content = String::new();
    iges_content.push_str(&s_line);
    iges_content.push_str(&g_line);
    iges_content.push_str(&d1);
    iges_content.push_str(&d2);
    iges_content.push_str(&p1);
    iges_content.push_str(&t_line);

    fs::write(&igs_path, iges_content)?;

    let report = convert_path(&igs_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    assert!(svg.contains("<path"));
    assert!(svg.contains("iges_0"));
    Ok(())
}

#[test]
fn test_cjk_table_column_widths_and_escaped_pipes() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("cjk_table.md");
    let out_dir = temp.path().join("cjk_table_out");
    let reverse_out = temp.path().join("restored_cjk.md");

    let table_content = r#"| 品名 | 仕様・構成 | 単価 |
| :--- | :--- | ---: |
| クラウドサーバー | `cpu \| mem` | ¥120,000 |
| データベース | `primary \| replica` | ¥85,000 |
"#;
    fs::write(&md_path, table_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    // Must render Japanese CJK text
    assert!(svg.contains("クラウドサーバー"));
    assert!(svg.contains("仕様・構成"));
    assert!(svg.contains("¥120,000"));

    // Reverse conversion must preserve escaped pipe `\|` inside cell
    svg_to_document(
        out_dir.join("page-0001.svg"),
        &reverse_out,
        &ReverseOptions::default(),
    )?;
    let restored = fs::read_to_string(&reverse_out)?;
    assert!(restored.contains(r#"cpu \| mem"#));
    assert!(restored.contains(r#"primary \| replica"#));
    Ok(())
}

#[test]
fn test_d2_nested_containers_and_title() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let d2_path = temp.path().join("nested.d2");
    let out_dir = temp.path().join("d2_nested_out");

    let d2_content = r#"title: "Microservice Topology"
direction: right
vpc: {
  sub: {
    gateway: "API Gateway"
  }
}
auth: "Auth Service"
gateway -> auth: "verify"
"#;
    fs::write(&d2_path, d2_content)?;

    let report = convert_path(&d2_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    // Verify title and nodes are present
    assert!(svg.contains("Microservice Topology"));
    assert!(svg.contains("API Gateway"));
    assert!(svg.contains("Auth Service"));
    assert!(svg.contains("verify"));

    // Verify direction is not rendered as a node
    assert!(!svg.contains(">direction<"));
    assert!(!svg.contains(">right<"));
    Ok(())
}

#[test]
fn test_transform_monochrome_color_and_unit_viewbox() -> Result<(), Box<dyn std::error::Error>> {
    use document_svg::{TransformOptions, transform_svg};

    // 1. Color property and currentColor monochrome transform
    let input_svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <path d="M 10 10 L 90 90" style="color: #ef4444; fill: currentColor; stroke: none" />
</svg>"#;

    let opts = TransformOptions {
        monochrome: Some("#3b82f6".to_string()),
        ..Default::default()
    };
    let transformed = transform_svg(input_svg.as_bytes(), &opts)?;
    let out_svg = String::from_utf8(transformed)?;
    assert!(out_svg.contains("color: #3b82f6"));

    // 2. Responsive unit-aware viewBox synthesis (from width="100mm" height="50mm")
    let unit_svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100mm" height="50mm">
  <rect x="0" y="0" width="100mm" height="50mm" fill="#22c55e" />
</svg>"##;

    let resp_opts = TransformOptions {
        responsive: true,
        ..Default::default()
    };
    let resp_transformed = transform_svg(unit_svg.as_bytes(), &resp_opts)?;
    let resp_out_svg = String::from_utf8(resp_transformed)?;
    // Should remove width/height from root svg element and synthesize numeric viewBox
    assert!(!resp_out_svg.contains("<svg width="));
    assert!(resp_out_svg.contains("viewBox=\"0 0 "));
    // 100mm is ~377.95 px
    assert!(resp_out_svg.contains("377.95") || resp_out_svg.contains("378"));
    Ok(())
}

#[test]
fn test_latex_mathcal_and_primes() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let math_path = temp.path().join("formula.tex");
    let out_dir = temp.path().join("math_out");
    let tex_content = r"\mathcal{O}(n \log n) \quad f^\prime(x) \longrightarrow y \sup";
    fs::write(&math_path, tex_content)?;

    let report = convert_path(&math_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    // \mathcal{O} maps to Mathematical Script Capital O (𝒪: U+1D4AA)
    assert!(svg.contains("𝒪"));
    // \prime maps to ′ (U+2032)
    assert!(svg.contains("′"));
    // \longrightarrow maps to ⟶ (U+27F6)
    assert!(svg.contains("⟶"));
    // \sup maps to sup operator
    assert!(svg.contains("sup"));
    Ok(())
}

#[test]
fn test_markdown_definition_lists_and_footnotes() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let md_path = temp.path().join("glossary.md");
    let out_dir = temp.path().join("glossary_out");
    let md_content = r#"# Glossary & References

DocSVG[^engine] is a universal vector converter.

Term A
: Definition description of Term A.

Term B
: Definition description of Term B.

[^engine]: High-performance document typesetting engine written in pure Rust.
"#;
    fs::write(&md_path, md_content)?;

    let report = convert_path(&md_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;
    // Heading
    assert!(svg.contains("Glossary &amp; References"));
    // Footnote reference in paragraph
    assert!(svg.contains("DocSVG[engine]"));
    // Definition list items
    assert!(svg.contains("Definition description of Term A."));
    assert!(svg.contains("Definition description of Term B."));
    // Footnote definition block
    assert!(svg.contains("[engine]"));
    assert!(svg.contains("High-performance document typesetting engine"));
    Ok(())
}

#[test]
fn test_mermaid_sequence_diagram_actor_autonumber_and_self_loops()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempdir()?;
    let seq_path = temp.path().join("seq.mmd");
    let out_dir = temp.path().join("seq_out");
    let content = r#"sequenceDiagram
    autonumber
    actor U as User
    participant S as Server
    U->>S: Request Login
    S->>S: Validate Token
    S-->>U: Return Success
"#;
    fs::write(&seq_path, content)?;

    let report = convert_path(&seq_path, &out_dir, &ConvertOptions::default())?;
    assert_eq!(report.page_count, 1);
    let svg = fs::read_to_string(out_dir.join("page-0001.svg"))?;

    // Check actor and participant labels rendered
    assert!(svg.contains("User"));
    assert!(svg.contains("Server"));
    // Check autonumber formatted messages
    assert!(svg.contains("1: Request Login"));
    assert!(svg.contains("2: Validate Token"));
    assert!(svg.contains("3: Return Success"));
    // Check actor stick figure head arc
    assert!(svg.contains("a 6.5 6.5 0 1 0 13 0"));
    // Check self-loopback path (contains right turn h 36.0 and down v 22.0)
    assert!(svg.contains("h 36.0 v 22.0"));

    Ok(())
}
