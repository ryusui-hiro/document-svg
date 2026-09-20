//! Bounded OASIS STIX 2.1 JSON threat-intelligence previews.
//!
//! STIX bundles contain domain objects, cyber-observable objects and relationship
//! objects. This adapter renders object type/id and safe structural metadata;
//! descriptions, indicator patterns, hashes, URLs, labels and reference payloads
//! remain inert and no TAXII/API/network operation is performed.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_STIX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_STIX_DEPTH: usize = 100;
const MAX_STIX_VALUES: usize = 300_000;
const MAX_STIX_OBJECTS: usize = 100_000;
const MAX_STIX_ROWS: usize = 200_000;
const MAX_STIX_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_STIX_DISPLAY_BYTES: usize = 256;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) {
        let Some(kind) = object.get("type").and_then(Value::as_str) else {
            return false;
        };
        if kind == "bundle" {
            return object.get("objects").and_then(Value::as_array).is_some();
        }
        let canonical_id = object
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.contains("--"));
        return canonical_id
            && (object.get("spec_version").is_some()
                || object.get("created").is_some()
                || object.get("modified").is_some());
    }
    text.contains("\"type\"")
        && text.contains("\"id\"")
        && text.contains("--")
        && (text.contains("\"spec_version\"") || text.contains("\"created\""))
}

struct StixPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for StixPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "stix-json".into();
        if page.title.is_empty() {
            page.title = "STIX 2.1 JSON".into();
        }
        page.description =
            "STIX threat-intelligence object metadata is rendered inertly; patterns, payloads, references and network operations are not displayed or resolved".into();
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
        options.max_input_bytes.min(MAX_STIX_BYTES),
        "STIX JSON input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("STIX JSON must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "STIX 2.1 JSON".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = StixPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct Summary {
    rows: Vec<Vec<String>>,
    objects: usize,
    relationships: usize,
    labels: usize,
    refs: usize,
    indicators: usize,
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_STIX_BYTES {
        return Err(Error::LimitExceeded(format!(
            "STIX JSON exceeds {MAX_STIX_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid STIX JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("STIX JSON root must be an object".into()))?;
    let mut summary = Summary::default();
    let mut warnings = Vec::new();
    let root_type = root
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput("STIX object requires a string type".into()))?;
    if root_type == "bundle" {
        let objects = root
            .get("objects")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::InvalidInput("STIX bundle requires an objects array".into()))?;
        if objects.len() > MAX_STIX_OBJECTS {
            return Err(Error::LimitExceeded(format!(
                "STIX objects exceed {MAX_STIX_OBJECTS}"
            )));
        }
        for object in objects {
            add_object(object, &mut summary)?;
        }
    } else {
        add_object(&value, &mut summary)?;
    }
    if summary.rows.is_empty() {
        return Err(Error::InvalidInput(
            "STIX JSON contains no object rows".into(),
        ));
    }
    if summary.indicators > 0 {
        warnings.push(
            "STIX indicator pattern text is omitted and never evaluated as a detection query"
                .into(),
        );
    }
    warnings.push("STIX descriptions, labels, hashes, URLs, pattern payloads, reference values, marking content and custom properties are omitted; no TAXII/API/network operation runs".into());
    warnings.push("STIX object IDs are shown as inert identifiers; relationship and reference counts are not dereferenced or semantically validated".into());
    let metadata = format!(
        "Objects: {}\nRelationships: {}\nLabels: {}\nReference arrays: {}\nIndicators: {}\nRoot: {}",
        summary.objects,
        summary.relationships,
        summary.labels,
        summary.refs,
        summary.indicators,
        root_type
    );
    Ok((
        TableData {
            headers: vec![
                "Type".into(),
                "ID".into(),
                "Created / modified".into(),
                "Labels".into(),
                "Structure".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn add_object(value: &Value, summary: &mut Summary) -> Result<()> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("STIX object entry must be an object".into()))?;
    let kind = required_string(object, "type")?;
    let id = required_string(object, "id")?;
    let created = object.get("created").and_then(Value::as_str).unwrap_or("—");
    let modified = object
        .get("modified")
        .and_then(Value::as_str)
        .unwrap_or("—");
    let labels = object
        .get("labels")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let relationships = usize::from(kind == "relationship")
        + object.get("objects").map_or(0, count_reference_array)
        + object.get("object_refs").map_or(0, count_reference_array)
        + object.get("created_by_ref").map_or(0, |_| 1);
    let refs = object
        .iter()
        .filter(|(key, value)| {
            key.ends_with("_ref")
                || key.ends_with("_refs")
                || value.is_array() && key.contains("refs")
        })
        .map(|(_, value)| {
            if value.is_array() {
                value.as_array().map_or(0, Vec::len)
            } else {
                1
            }
        })
        .sum::<usize>();
    let structure = format!("refs {refs} · properties omitted");
    summary.objects = summary.objects.saturating_add(1);
    summary.relationships = summary.relationships.saturating_add(relationships);
    summary.labels = summary.labels.saturating_add(labels);
    summary.refs = summary.refs.saturating_add(refs);
    if kind == "indicator" {
        summary.indicators = summary.indicators.saturating_add(1);
    }
    if summary.rows.len() >= MAX_STIX_ROWS {
        return Err(Error::LimitExceeded(format!(
            "STIX rows exceed {MAX_STIX_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(kind),
        truncate(id),
        format!("{} / {}", truncate(created), truncate(modified)),
        labels.to_string(),
        structure,
    ]);
    Ok(())
}

fn count_reference_array(value: &Value) -> usize {
    value.as_array().map_or(0, Vec::len)
}

fn required_string<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput(format!("STIX object requires string {key}")))?;
    if value.is_empty() {
        return Err(Error::InvalidInput(format!("STIX {key} must not be empty")));
    }
    Ok(value)
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_STIX_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_STIX_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_STIX_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "STIX JSON nesting exceeds {MAX_STIX_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_STIX_VALUES {
        return Err(Error::LimitExceeded(format!(
            "STIX JSON contains more than {MAX_STIX_VALUES} values"
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
        Value::String(value) if value.len() > MAX_STIX_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "STIX JSON string exceeds {MAX_STIX_STRING_BYTES} bytes"
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
                if depth > MAX_STIX_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "STIX JSON nesting exceeds {MAX_STIX_DEPTH} levels"
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
    fn recognizes_stix_bundle_and_object() {
        assert!(looks_like_prefix(
            br#"{"type":"bundle","id":"bundle--1","objects":[]}"#
        ));
        assert!(looks_like_prefix(
            br#"{"type":"indicator","id":"indicator--1","spec_version":"2.1"}"#
        ));
        assert!(!looks_like_prefix(br#"{"type":"thing"}"#));
    }

    #[test]
    fn summarizes_bundle_without_indicator_pattern_or_description() {
        let (table, metadata, warnings) = parse(
            r#"{"type":"bundle","id":"bundle--1","objects":[{"type":"indicator","id":"indicator--1","created":"2024-01-01T00:00:00Z","modified":"2024-01-02T00:00:00Z","labels":["malicious"],"pattern":"[file:hashes.MD5 = 'secret']","description":"private description"},{"type":"relationship","id":"relationship--1","created":"2024-01-01T00:00:00Z","modified":"2024-01-01T00:00:00Z","source_ref":"indicator--1","target_ref":"file--1"}]}"#,
        ).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert!(metadata.contains("Indicators: 1"));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("malicious"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("pattern")));
    }
}
