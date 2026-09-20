//! Bounded RFC 7396 JSON Merge Patch previews.
//!
//! Merge Patch documents mimic a target JSON object: non-null members replace
//! or merge values and null members remove them. This adapter renders affected
//! paths, actions and value types only; it never applies a patch or displays
//! value payloads.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_MERGEPATCH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MERGEPATCH_DEPTH: usize = 100;
const MAX_MERGEPATCH_VALUES: usize = 300_000;
const MAX_MERGEPATCH_OPERATIONS: usize = 200_000;
const MAX_MERGEPATCH_STRING_BYTES: usize = 2 * 1024 * 1024;

struct MergePatchPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for MergePatchPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "jsonmergepatch".into();
        if page.title.is_empty() {
            page.title = "JSON Merge Patch".into();
        }
        page.description =
            "RFC 7396 JSON Merge Patch paths are rendered as inert metadata; target documents and values are not modified or displayed".into();
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
        options.max_input_bytes.min(MAX_MERGEPATCH_BYTES),
        "JSON Merge Patch input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("JSON Merge Patch must be UTF-8 JSON: {error}"))
    })?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "JSON Merge Patch".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = MergePatchPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct Summary {
    rows: Vec<Vec<String>>,
    set: usize,
    delete: usize,
    merge: usize,
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_MERGEPATCH_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON Merge Patch exceeds {MAX_MERGEPATCH_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid JSON Merge Patch: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let mut summary = Summary::default();
    collect_paths(&value, "/", 0, &mut summary)?;
    if summary.rows.is_empty() {
        summary.rows.push(vec![
            "/".into(),
            "(empty patch)".into(),
            "object".into(),
            "0".into(),
        ]);
    }
    let metadata = format!(
        "Paths: {}\nSet/replace: {}\nDelete: {}\nNested merge: {}",
        summary.rows.len(),
        summary.set,
        summary.delete,
        summary.merge
    );
    let warnings = vec![
        "JSON Merge Patch values are omitted; patches are never applied, target documents and JSON Pointers are not evaluated, and no external resource is opened".into(),
        "RFC 7396 null deletion and object merge semantics remain inert metadata; arrays and scalar roots are summarized as replacements".into(),
    ];
    Ok((
        TableData {
            headers: vec![
                "Path".into(),
                "Action".into(),
                "Value type".into(),
                "Depth".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn collect_paths(value: &Value, path: &str, depth: usize, summary: &mut Summary) -> Result<()> {
    if depth > MAX_MERGEPATCH_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "JSON Merge Patch nesting exceeds {MAX_MERGEPATCH_DEPTH} levels"
        )));
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if summary.rows.len() >= MAX_MERGEPATCH_OPERATIONS {
                    return Err(Error::LimitExceeded(format!(
                        "JSON Merge Patch paths exceed {MAX_MERGEPATCH_OPERATIONS}"
                    )));
                }
                let child_path = format!("{}{}", path.trim_end_matches('/'), pointer_segment(key));
                if child.is_null() {
                    summary.delete = summary.delete.saturating_add(1);
                    summary.rows.push(vec![
                        truncate(&child_path),
                        "delete".into(),
                        "null".into(),
                        (depth + 1).to_string(),
                    ]);
                } else if child.is_object() {
                    summary.merge = summary.merge.saturating_add(1);
                    summary.rows.push(vec![
                        truncate(&child_path),
                        "merge".into(),
                        "object".into(),
                        (depth + 1).to_string(),
                    ]);
                    collect_paths(child, &child_path, depth + 1, summary)?;
                } else {
                    summary.set = summary.set.saturating_add(1);
                    summary.rows.push(vec![
                        truncate(&child_path),
                        "set".into(),
                        json_type(child).into(),
                        (depth + 1).to_string(),
                    ]);
                }
            }
        }
        _ => {
            summary.set = summary.set.saturating_add(1);
            summary.rows.push(vec![
                truncate(path),
                "set".into(),
                json_type(value).into(),
                depth.to_string(),
            ]);
        }
    }
    Ok(())
}

fn pointer_segment(value: &str) -> String {
    format!("/{}", value.replace('~', "~0").replace('/', "~1"))
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_MERGEPATCH_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_MERGEPATCH_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_MERGEPATCH_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "JSON Merge Patch nesting exceeds {MAX_MERGEPATCH_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_MERGEPATCH_VALUES {
        return Err(Error::LimitExceeded(format!(
            "JSON Merge Patch contains more than {MAX_MERGEPATCH_VALUES} values"
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
        Value::String(value) if value.len() > MAX_MERGEPATCH_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "JSON Merge Patch string exceeds {MAX_MERGEPATCH_STRING_BYTES} bytes"
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
                if depth > MAX_MERGEPATCH_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "JSON Merge Patch nesting exceeds {MAX_MERGEPATCH_DEPTH} levels"
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
    fn summarizes_merge_patch_without_values() {
        let (table, metadata, warnings) = parse(
            r#"{"title":"Hello","author":{"familyName":null,"givenName":"John"},"tags":["example"],"token":"very-secret"}"#,
        )
        .unwrap();
        assert!(metadata.contains("Paths: 6"));
        assert!(table.rows.iter().any(|row| row[1] == "set"));
        assert!(table.rows.iter().any(|row| row[1] == "delete"));
        assert!(table.rows.iter().any(|row| row[1] == "merge"));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("never")));
    }
}
