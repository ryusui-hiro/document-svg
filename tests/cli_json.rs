use std::fs;
use std::process::Command;

#[test]
fn convert_and_reverse_write_parseable_json_reports() {
    let temporary = tempfile::tempdir().unwrap();
    let input = temporary.path().join("diagram.svg");
    let pages = temporary.path().join("pages");
    let html = temporary.path().join("diagram.html");
    fs::write(
        &input,
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10"/></svg>"#,
    )
    .unwrap();

    let converted = Command::new(env!("CARGO_BIN_EXE_docsvg"))
        .arg(&input)
        .arg("--output")
        .arg(&pages)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    let stdout_report: serde_json::Value = serde_json::from_slice(&converted.stdout).unwrap();
    let disk_report: serde_json::Value =
        serde_json::from_slice(&fs::read(pages.join("conversion.json")).unwrap()).unwrap();
    assert_eq!(stdout_report, disk_report);
    assert_eq!(stdout_report["page_count"], 1);
    assert!(converted.stdout.ends_with(b"\n"));

    let reversed = Command::new(env!("CARGO_BIN_EXE_docsvg"))
        .arg("reverse")
        .arg(&input)
        .arg("--output")
        .arg(&html)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        reversed.status.success(),
        "{}",
        String::from_utf8_lossy(&reversed.stderr)
    );
    let reverse_report: serde_json::Value = serde_json::from_slice(&reversed.stdout).unwrap();
    assert_eq!(reverse_report["page_count"], 1);
    assert_eq!(reverse_report["output"], html.to_string_lossy().as_ref());
    assert_eq!(reverse_report["output_format"], "html");
    assert!(html.exists());
}
