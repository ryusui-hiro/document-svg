use std::fs;
use std::process::Command;

#[test]
fn transform_replaces_output_only_after_success() {
    let temporary = tempfile::tempdir().unwrap();
    let input = temporary.path().join("input.svg");
    let output = temporary.path().join("output.svg");
    fs::write(&input, br#"<svg viewBox="0 0 10.123 10"/>"#).unwrap();
    fs::write(&output, "previous output").unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_docsvg"))
        .arg("transform")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg("--precision")
        .arg(usize::MAX.to_string())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("precision must be between 0 and 12"));
    assert_eq!(fs::read_to_string(&output).unwrap(), "previous output");

    fs::write(&input, br#"<svg width="100%" height="100%"/>"#).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_docsvg"))
        .arg("transform")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg("--responsive")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("requires a viewBox"));
    assert_eq!(fs::read_to_string(&output).unwrap(), "previous output");

    fs::write(&input, br#"<html/>"#).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_docsvg"))
        .arg("transform")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("not an SVG element"));
    assert_eq!(fs::read_to_string(&output).unwrap(), "previous output");

    fs::write(&input, br#"<svg viewBox="0 0 10.123 10"/>"#).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_docsvg"))
        .arg("transform")
        .arg(&input)
        .arg("--output")
        .arg(&input)
        .arg("--precision")
        .arg("2")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(
        fs::read_to_string(&input)
            .unwrap()
            .contains("viewBox=\"0 0 10.12 10\"")
    );
    assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn transform_keeps_output_symlink_and_target_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temporary = tempfile::tempdir().unwrap();
    let input = temporary.path().join("input.svg");
    let target = temporary.path().join("target.svg");
    let output = temporary.path().join("alias.svg");
    fs::write(&input, br#"<svg viewBox="0 0 10.123 10"/>"#).unwrap();
    fs::write(&target, "old").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&target, &output).unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_docsvg"))
        .arg("transform")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg("--precision")
        .arg("2")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(output.is_symlink());
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("viewBox=\"0 0 10.12 10\"")
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644
    );
}
