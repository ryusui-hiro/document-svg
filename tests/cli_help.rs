use std::process::Command;

#[test]
fn help_is_concise_and_points_to_the_complete_format_reference() {
    for arguments in [&["--help"][..], &["convert", "--help"][..]] {
        let result = Command::new(env!("CARGO_BIN_EXE_docsvg"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(result.status.success());
        let help = String::from_utf8(result.stdout).unwrap();
        assert!(help.contains("docsvg report.pdf --output preview/report"));
        assert!(help.contains("https://ryusui-hiro.github.io/document-svg/formats.html"));
        assert!(help.contains("--max-pages"));
        assert!(help.len() < 2_500, "help is {} bytes", help.len());
    }
}
