//! Bounded RFC 6902 JSON Patch previews.
//!
//! A JSON Patch document is an ordered array of operation objects. This adapter
//! validates the operation shape and renders op/path/from plus value presence
//! and type; it never applies the patch, evaluates JSON Pointers, or displays
//! value payloads.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_JSONPATCH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSONPATCH_DEPTH: usize = 100;
const MAX_JSONPATCH_VALUES: usize = 300_000;
const MAX_JSONPATCH_OPERATIONS: usize = 200_000;
const MAX_JSONPATCH_STRING_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('[')
        && text.contains("\"op\"")
        && text.contains("\"path\"")
        && [
            "\"add\"",
            "\"remove\"",
            "\"replace\"",
            "\"move\"",
            "\"copy\"",
            "\"test\"",
        ]
        .iter()
        .any(|op| text.contains(op))
}

struct JsonPatchPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for JsonPatchPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "jsonpatch".into();
        if page.title.is_empty() {
            page.title = "JSON Patch".into();
        }
        page.description =
            "RFC 6902 JSON Patch operations are rendered as inert metadata; target documents and value payloads are not modified or displayed".into();
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
        options.max_input_bytes.min(MAX_JSONPATCH_BYTES),
        "JSON Patch input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("JSON Patch must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "JSON Patch".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = JsonPatchPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_JSONPATCH_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON Patch exceeds {MAX_JSONPATCH_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid JSON Patch: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let operations = value
        .as_array()
        .ok_or_else(|| Error::InvalidInput("JSON Patch root must be an array".into()))?;
    if operations.len() > MAX_JSONPATCH_OPERATIONS {
        return Err(Error::LimitExceeded(format!(
            "JSON Patch operations exceed {MAX_JSONPATCH_OPERATIONS}"
        )));
    }
    let mut rows = Vec::new();
    let mut counts = [0usize; 6];
    for (index, operation) in operations.iter().enumerate() {
        let object = operation.as_object().ok_or_else(|| {
            Error::InvalidInput(format!(
                "JSON Patch operation {} must be an object",
                index + 1
            ))
        })?;
        let op = object.get("op").and_then(Value::as_str).ok_or_else(|| {
            Error::InvalidInput(format!("JSON Patch operation {} requires op", index + 1))
        })?;
        let op_index = match op {
            "add" => 0,
            "remove" => 1,
            "replace" => 2,
            "move" => 3,
            "copy" => 4,
            "test" => 5,
            _ => {
                return Err(Error::InvalidInput(format!(
                    "JSON Patch operation {} has unsupported op {op}",
                    index + 1
                )));
            }
        };
        counts[op_index] = counts[op_index].saturating_add(1);
        let path = object.get("path").and_then(Value::as_str).ok_or_else(|| {
            Error::InvalidInput(format!("JSON Patch operation {} requires path", index + 1))
        })?;
        let from = object.get("from").and_then(Value::as_str);
        if matches!(op, "move" | "copy") && from.is_none() {
            return Err(Error::InvalidInput(format!(
                "JSON Patch {op} operation {} requires from",
                index + 1
            )));
        }
        if matches!(op, "add" | "replace" | "test") && !object.contains_key("value") {
            return Err(Error::InvalidInput(format!(
                "JSON Patch {op} operation {} requires value",
                index + 1
            )));
        }
        let value_type = object.get("value").map(json_type).unwrap_or("—").to_owned();
        rows.push(vec![
            (index + 1).to_string(),
            truncate(op),
            truncate(path),
            from.map_or_else(|| "—".into(), truncate),
            value_type,
        ]);
    }
    if rows.is_empty() {
        rows.push(vec![
            "—".into(),
            "(no operations)".into(),
            "—".into(),
            "—".into(),
            "—".into(),
        ]);
    }
    let metadata = format!(
        "Operations: {}\nAdd: {}\nRemove: {}\nReplace: {}\nMove: {}\nCopy: {}\nTest: {}",
        operations.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4],
        counts[5]
    );
    let warnings = vec![
        "JSON Patch value payloads are omitted; operations are never applied, JSON Pointers are not evaluated, and no target document or external resource is opened".into(),
        "RFC 6902 operation order and target-document semantics remain inert metadata".into(),
    ];
    Ok((
        TableData {
            headers: vec![
                "#".into(),
                "Op".into(),
                "Path".into(),
                "From".into(),
                "Value type".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
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
    if value.len() <= MAX_JSONPATCH_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_JSONPATCH_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_JSONPATCH_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "JSON Patch nesting exceeds {MAX_JSONPATCH_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_JSONPATCH_VALUES {
        return Err(Error::LimitExceeded(format!(
            "JSON Patch contains more than {MAX_JSONPATCH_VALUES} values"
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
        Value::String(value) if value.len() > MAX_JSONPATCH_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "JSON Patch string exceeds {MAX_JSONPATCH_STRING_BYTES} bytes"
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
                if depth > MAX_JSONPATCH_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "JSON Patch nesting exceeds {MAX_JSONPATCH_DEPTH} levels"
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
    fn recognizes_json_patch() {
        assert!(looks_like_prefix(
            br#"[{"op":"replace","path":"/a","value":1}]"#
        ));
        assert!(!looks_like_prefix(br#"[{"path":"/a","value":1}]"#));
    }

    #[test]
    fn validates_operations_without_displaying_values() {
        let (table, metadata, warnings) = parse(
            r#"[{"op":"add","path":"/token","value":"very-secret"},{"op":"move","from":"/a","path":"/b"},{"op":"remove","path":"/old"}]"#,
        )
        .unwrap();
        assert!(metadata.contains("Operations: 3"));
        assert_eq!(table.rows[0][4], "string");
        assert_eq!(table.rows[1][3], "/a");
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
