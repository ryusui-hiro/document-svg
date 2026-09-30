use std::process::Command;

#[test]
fn formats_search_uses_the_published_catalog() {
    let binary = env!("CARGO_BIN_EXE_docsvg");

    let categories = Command::new(binary).arg("formats").output().unwrap();
    assert!(categories.status.success());
    let categories = String::from_utf8(categories.stdout).unwrap();
    assert!(categories.contains("Office & documents"));
    assert!(categories.contains("docsvg formats QUERY"));

    let pdf = Command::new(binary)
        .args(["formats", "pdf", "--lang", "ja", "--details"])
        .output()
        .unwrap();
    assert!(pdf.status.success());
    let pdf = String::from_utf8(pdf.stdout).unwrap();
    assert!(pdf.contains(".pdf  |  to SVG: A  |  from SVG: A"));
    assert!(pdf.contains("オフィス文書"));
    assert!(pdf.contains("PDFのベクターコンテンツ"));
    assert!(pdf.contains("1 format(s) matched"));

    let powerpoint = Command::new(binary)
        .args(["formats", "PowerPoint"])
        .output()
        .unwrap();
    assert!(powerpoint.status.success());
    let powerpoint = String::from_utf8(powerpoint.stdout).unwrap();
    assert!(powerpoint.contains(".pptx / .docx / .xlsx"));
    assert!(powerpoint.contains(".ppt (CFB PowerPoint binary)"));

    let japanese = Command::new(binary)
        .args(["formats", "パワーポイント", "--lang", "ja"])
        .output()
        .unwrap();
    assert!(japanese.status.success());
    let japanese = String::from_utf8(japanese.stdout).unwrap();
    assert!(japanese.contains(".pptx / .docx / .xlsx"));
    assert!(japanese.contains(".ppt (CFB PowerPoint binary)"));

    let visio = Command::new(binary)
        .args(["formats", "Visio"])
        .output()
        .unwrap();
    assert!(visio.status.success());
    let visio = String::from_utf8(visio.stdout).unwrap();
    assert!(visio.contains(".vsdx / .vsdm"));
    assert!(visio.contains(".vsd / .vss"));

    let missing = Command::new(binary)
        .args(["formats", "not-a-real-format-12345"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no supported formats match"));
}
