use std::fs;
use tempfile::tempdir;

use document_svg::{ConvertOptions, ReverseOptions, convert_path, svg_to_document};

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
