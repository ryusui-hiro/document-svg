//! Safe resolution of relative resources associated with a local input file.

use std::fs;
use std::path::{Component, Path, PathBuf};

pub(crate) fn resolve_relative_file(base_dir: &Path, reference: &str) -> Option<PathBuf> {
    if reference.trim().is_empty()
        || reference.starts_with("//")
        || reference.starts_with('\\')
        || reference.starts_with('/')
        || reference.contains(':')
    {
        return None;
    }
    let uri_path = reference.split(['?', '#']).next()?.replace('\\', "/");
    let decoded = percent_decode_path(&uri_path)?;
    let relative = Path::new(&decoded);
    if decoded.contains(':')
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return None;
    }
    let canonical_base = fs::canonicalize(base_dir).ok()?;
    let candidate = fs::canonicalize(canonical_base.join(relative)).ok()?;
    candidate.starts_with(&canonical_base).then_some(candidate)
}

fn percent_decode_path(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push((hex_nibble(high)? << 4) | hex_nibble(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.chars().any(char::is_control)).then_some(decoded)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_relative_file;

    #[test]
    fn confines_decoded_relative_paths_to_the_input_tree() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("site");
        std::fs::create_dir_all(base.join("assets")).unwrap();
        std::fs::write(base.join("assets/image red.png"), b"local image").unwrap();
        let base = std::fs::canonicalize(base).unwrap();

        assert_eq!(
            resolve_relative_file(&base, "assets/image%20red.png"),
            Some(base.join("assets/image red.png"))
        );
        assert!(resolve_relative_file(&base, "../outside.png").is_none());
        assert!(resolve_relative_file(&base, "%2e%2e/outside.png").is_none());
        assert!(resolve_relative_file(&base, "https://example.invalid/file.png").is_none());
        assert!(resolve_relative_file(&base, "//example.invalid/file.png").is_none());
    }
}
