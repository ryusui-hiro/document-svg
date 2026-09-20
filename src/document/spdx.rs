//! Bounded SPDX 2.x JSON bill-of-material previews.
//!
//! SPDX documents can carry checksums, license expressions, suppliers, PURLs,
//! annotations and external references. This adapter renders package/file IDs,
//! names and versions only, plus relationship counts; it never resolves or
//! publishes any external resource.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_SPDX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SPDX_DEPTH: usize = 100;
const MAX_SPDX_VALUES: usize = 300_000;
const MAX_SPDX_ELEMENTS: usize = 200_000;
const MAX_SPDX_STRING_BYTES: usize = 512 * 1024;
const MAX_SPDX_LINES: usize = 500_000;
const MAX_SPDX_LINE_BYTES: usize = 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"spdxVersion\"")
        && text.contains("\"SPDXID\"")
        && (text.contains("\"packages\"")
            || text.contains("\"files\"")
            || text.contains("\"relationships\""))
}

pub(crate) fn looks_like_tag_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    (trimmed.starts_with("SPDXVersion:") || trimmed.contains("\nSPDXVersion:"))
        && text.contains("SPDX-2.")
        && (text.contains("PackageName:") || text.contains("FileName:"))
}

struct SpdxPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SpdxPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "spdx".into();
        if page.title.is_empty() {
            page.title = "SPDX document".into();
        }
        page.description =
            "SPDX package and file identities are rendered as inert metadata; checksums, licenses and external references are omitted".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SPDX_BYTES),
        "SPDX input",
    )?;
    let is_json = bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| byte == b'{');
    let (table, metadata, warnings) = if is_json {
        let text = String::from_utf8(bytes)
            .map_err(|error| Error::InvalidInput(format!("SPDX JSON must be UTF-8: {error}")))?;
        parse(&text)?
    } else {
        parse_tag_value(&bytes)?
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "SPDX document".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = SpdxPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct TagElement {
    kind: &'static str,
    id: String,
    name: String,
    version: String,
}

fn parse_tag_value(bytes: &[u8]) -> Result<(TableData, String, Vec<String>)> {
    if bytes.len() as u64 > MAX_SPDX_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SPDX tag:value input exceeds {MAX_SPDX_BYTES} bytes"
        )));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("SPDX tag:value must be UTF-8: {error}")))?;
    let mut version = None;
    let mut document_name = String::new();
    let mut pending_id = String::new();
    let mut current: Option<TagElement> = None;
    let mut rows = Vec::new();
    let mut relationships = 0usize;
    let mut annotations = 0usize;
    let mut external_refs = 0usize;
    let mut snippets = 0usize;
    for (line_number, raw) in text.lines().enumerate() {
        if line_number >= MAX_SPDX_LINES {
            return Err(Error::LimitExceeded(format!(
                "SPDX tag:value exceeds {MAX_SPDX_LINES} lines"
            )));
        }
        if raw.len() > MAX_SPDX_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "SPDX tag:value line {} exceeds {MAX_SPDX_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let raw = raw.trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        let Some((field, value)) = raw.split_once(':') else {
            continue;
        };
        let field = field.trim();
        let value = value.trim();
        match field {
            "SPDXVersion" => version = Some(value.to_owned()),
            "DocumentName" => document_name = truncate(value),
            "SPDXID" => {
                if let Some(element) = current.as_mut() {
                    element.id = truncate(value);
                } else {
                    pending_id = truncate(value);
                }
            }
            "PackageName" => {
                flush_tag_element(&mut current, &mut rows)?;
                current = Some(TagElement {
                    kind: "package",
                    id: std::mem::take(&mut pending_id),
                    name: truncate(value),
                    ..TagElement::default()
                });
            }
            "PackageVersion" => {
                if let Some(element) = current.as_mut().filter(|element| element.kind == "package")
                {
                    element.version = truncate(value);
                }
            }
            "FileName" => {
                flush_tag_element(&mut current, &mut rows)?;
                current = Some(TagElement {
                    kind: "file",
                    id: std::mem::take(&mut pending_id),
                    name: truncate(value),
                    ..TagElement::default()
                });
            }
            "Relationship" => relationships = relationships.saturating_add(1),
            "Annotation" | "Annotator" => annotations = annotations.saturating_add(1),
            "ExternalDocumentRef" | "ExternalRef" => {
                external_refs = external_refs.saturating_add(1)
            }
            "SnippetSPDXID" => snippets = snippets.saturating_add(1),
            _ => {}
        }
    }
    flush_tag_element(&mut current, &mut rows)?;
    let version =
        version.ok_or_else(|| Error::InvalidInput("SPDX tag:value requires SPDXVersion".into()))?;
    if !version.starts_with("SPDX-2.") {
        return Err(Error::Unsupported(format!(
            "SPDX version {version} is unsupported; SPDX-2.x tag:value is required"
        )));
    }
    if rows.is_empty() {
        rows.push(vec![
            "(no package/file elements)".into(),
            "—".into(),
            "—".into(),
            "—".into(),
        ]);
    }
    let metadata = format!(
        "Name: {}\nSPDX version: {version}\nPackages/files: {}\nRelationships: {relationships}\nAnnotations: {annotations}\nSnippets: {snippets}\nExternal documents: {external_refs}",
        if document_name.is_empty() {
            "—"
        } else {
            &document_name
        },
        rows.len().saturating_sub(usize::from(
            rows.len() == 1 && rows[0][0] == "(no package/file elements)"
        ))
    );
    let warnings = vec![
        "SPDX checksums, license expressions/text, suppliers, PURLs, download locations, annotations, external document references, comments and package/file properties are omitted; no registry, URL or code operation is performed".into(),
        "SPDX tag:value package/file identities are displayed without resolving relationship targets or evaluating license and compliance semantics".into(),
    ];
    Ok((table(rows), metadata, warnings))
}

fn flush_tag_element(current: &mut Option<TagElement>, rows: &mut Vec<Vec<String>>) -> Result<()> {
    let Some(element) = current.take() else {
        return Ok(());
    };
    if rows.len() >= MAX_SPDX_ELEMENTS {
        return Err(Error::LimitExceeded(format!(
            "SPDX package/file elements exceed {MAX_SPDX_ELEMENTS}"
        )));
    }
    rows.push(vec![
        element.kind.into(),
        if element.id.is_empty() {
            "(unassigned)".into()
        } else {
            element.id
        },
        if element.name.is_empty() {
            "(unnamed)".into()
        } else {
            element.name
        },
        if element.version.is_empty() {
            "—".into()
        } else {
            element.version
        },
    ]);
    Ok(())
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_SPDX_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SPDX JSON exceeds {MAX_SPDX_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid SPDX JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("SPDX root must be an object".into()))?;
    let version = root
        .get("spdxVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput("SPDX requires spdxVersion".into()))?;
    if !version.starts_with("SPDX-2.") {
        return Err(Error::Unsupported(format!(
            "SPDX version {version} is unsupported; SPDX-2.x JSON is required"
        )));
    }
    let mut rows = Vec::new();
    let packages = root
        .get("packages")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for package in packages {
        push_element_row(package, "package", &mut rows)?;
    }
    let files = root
        .get("files")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for file in files {
        push_element_row(file, "file", &mut rows)?;
    }
    if rows.is_empty() {
        rows.push(vec![
            "(no package/file elements)".into(),
            "—".into(),
            "—".into(),
            "—".into(),
        ]);
    }
    let relationships = root
        .get("relationships")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let annotations = root
        .get("annotations")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let snippets = root
        .get("snippets")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let external_refs = root
        .get("externalDocumentRefs")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let document_name = root.get("name").and_then(Value::as_str).unwrap_or("—");
    let metadata = format!(
        "Name: {}\nSPDX version: {version}\nPackages: {}\nFiles: {}\nRelationships: {relationships}\nAnnotations: {annotations}\nSnippets: {snippets}\nExternal documents: {external_refs}",
        truncate(document_name),
        packages.len(),
        files.len()
    );
    let warnings = vec![
        "SPDX checksums, license expressions/text, suppliers, PURLs, download locations, annotations, external document references, comments and package/file properties are omitted; no registry, URL or code operation is performed".into(),
        "SPDX package/file identities are displayed without resolving relationship targets or evaluating license and compliance semantics".into(),
    ];
    Ok((
        TableData {
            headers: vec![
                "Kind".into(),
                "SPDX ID".into(),
                "Name".into(),
                "Version".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn push_element_row(element: &Value, kind: &str, rows: &mut Vec<Vec<String>>) -> Result<()> {
    if rows.len() >= MAX_SPDX_ELEMENTS {
        return Err(Error::LimitExceeded(format!(
            "SPDX package/file elements exceed {MAX_SPDX_ELEMENTS}"
        )));
    }
    let Some(object) = element.as_object() else {
        return Ok(());
    };
    let id = object
        .get("SPDXID")
        .and_then(Value::as_str)
        .unwrap_or("(unassigned)");
    let name = if kind == "file" {
        object
            .get("fileName")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed file)")
    } else {
        object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed package)")
    };
    let version = object
        .get("versionInfo")
        .and_then(Value::as_str)
        .unwrap_or("—");
    rows.push(vec![
        kind.to_owned(),
        truncate(id),
        truncate(name),
        truncate(version),
    ]);
    Ok(())
}

fn table(rows: Vec<Vec<String>>) -> TableData {
    TableData {
        headers: vec![
            "Kind".into(),
            "SPDX ID".into(),
            "Name".into(),
            "Version".into(),
        ],
        rows,
        alignments: vec![TableAlign::Left; 4],
        raw_source: String::new(),
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_SPDX_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_SPDX_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_SPDX_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "SPDX JSON nesting exceeds {MAX_SPDX_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_SPDX_VALUES {
        return Err(Error::LimitExceeded(format!(
            "SPDX JSON contains more than {MAX_SPDX_VALUES} values"
        )));
    }
    match value {
        Value::Array(values) => {
            for item in values {
                count_values(item, depth + 1, count)?;
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                count_values(item, depth + 1, count)?;
            }
        }
        Value::String(value) if value.len() > MAX_SPDX_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "SPDX JSON string exceeds {MAX_SPDX_STRING_BYTES} bytes"
            )));
        }
        _ => {}
    }
    Ok(())
}

fn preflight_depth(text: &str) -> Result<()> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_SPDX_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "SPDX JSON nesting exceeds {MAX_SPDX_DEPTH} levels"
                    )));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_spdx_json() {
        assert!(looks_like_prefix(
            br#"{"spdxVersion":"SPDX-2.3","SPDXID":"SPDXRef-DOCUMENT","packages":[]}"#
        ));
        assert!(!looks_like_prefix(
            b"{\"spdxVersion\":\"SPDX-3.0\",\"packages\":[]}"
        ));
        assert!(looks_like_tag_prefix(
            b"SPDXVersion: SPDX-2.3\nPackageName: serde\n"
        ));
    }

    #[test]
    fn summarizes_packages_without_sensitive_payloads() {
        let (table, metadata, warnings) = parse(
            r#"{"spdxVersion":"SPDX-2.3","SPDXID":"SPDXRef-DOCUMENT","name":"private-sbom","documentNamespace":"https://private.example/sbom","packages":[{"SPDXID":"SPDXRef-Package","name":"serde","versionInfo":"1.0","downloadLocation":"https://private.example/serde","licenseConcluded":"MIT","externalRefs":[{"referenceLocator":"pkg:cargo/serde@1.0"}],"checksums":[{"checksumValue":"very-secret"}]}],"files":[{"SPDXID":"SPDXRef-File","fileName":"src/lib.rs","licenseConcluded":"NOASSERTION"}],"relationships":[{"spdxElementId":"SPDXRef-Package","relationshipType":"DEPENDS_ON","relatedSpdxElement":"SPDXRef-Other"}]}"#,
        )
        .unwrap();
        assert!(metadata.contains("Packages: 1"));
        assert_eq!(table.rows[0][2], "serde");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("private"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("omitted")));
    }

    #[test]
    fn summarizes_tag_value_without_sensitive_payloads() {
        let (table, metadata, _) = parse_tag_value(
            b"SPDXVersion: SPDX-2.3\nSPDXID: SPDXRef-DOCUMENT\nDocumentName: Preview\n\nPackageName: serde\nSPDXID: SPDXRef-Package\nPackageVersion: 1.0\nPackageChecksum: SHA256: very-secret\n\nFileName: src/lib.rs\nSPDXID: SPDXRef-File\n",
        )
        .unwrap();
        assert!(metadata.contains("Packages/files: 2"));
        assert_eq!(table.rows[0][2], "serde");
        assert_eq!(table.rows[1][2], "src/lib.rs");
    }
}
