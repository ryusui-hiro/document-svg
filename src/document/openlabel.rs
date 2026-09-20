//! Bounded ASAM OpenLABEL 1.0 JSON annotation previews.
//!
//! OpenLABEL describes multi-sensor annotations for objects, frames, actions,
//! events and relations. This adapter renders collection counts and structural
//! metadata only; sensor payloads, attribute values, coordinates, image/point
//! cloud resources and external ontologies are never displayed or fetched.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_OPENLABEL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OPENLABEL_DEPTH: usize = 100;
const MAX_OPENLABEL_VALUES: usize = 300_000;
const MAX_OPENLABEL_ROWS: usize = 200_000;
const MAX_OPENLABEL_ENTRIES: usize = 100_000;
const MAX_OPENLABEL_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_OPENLABEL_DISPLAY_BYTES: usize = 256;

const COLLECTIONS: &[(&str, &str)] = &[
    ("objects", "object labels"),
    ("actions", "action labels"),
    ("events", "event labels"),
    ("contexts", "context labels"),
    ("relations", "relations"),
    ("frames", "annotated frames"),
    ("frame_intervals", "frame intervals"),
    ("tags", "scenario tags"),
    ("ontologies", "ontology references"),
    ("resources", "sensor resources"),
    ("coordinate_systems", "coordinate systems"),
    ("streams", "sensor streams"),
];

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) {
        return object.get("openlabel").and_then(Value::as_object).is_some();
    }
    text.contains("\"openlabel\"")
}

struct OpenLabelPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OpenLabelPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "openlabel".into();
        if page.title.is_empty() {
            page.title = "ASAM OpenLABEL JSON".into();
        }
        page.description =
            "OpenLABEL annotation structure is rendered inertly; sensor payloads, coordinates, attribute values and external resources are not displayed or resolved".into();
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
        options.max_input_bytes.min(MAX_OPENLABEL_BYTES),
        "OpenLABEL JSON input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("OpenLABEL JSON must be UTF-8 JSON: {error}"))
    })?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "ASAM OpenLABEL JSON".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = OpenLabelPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_OPENLABEL_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OpenLABEL JSON exceeds {MAX_OPENLABEL_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid OpenLABEL JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("OpenLABEL JSON root must be an object".into()))?;
    let openlabel = root
        .get("openlabel")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::InvalidInput("OpenLABEL JSON requires an openlabel object".into()))?;
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut collection_counts = Vec::new();
    let mut total_entries = 0usize;
    let mut object_data = 0usize;
    let mut frame_object_data = 0usize;
    for (key, description) in COLLECTIONS {
        let count = match openlabel.get(*key) {
            None => 0,
            Some(value) => {
                let map = value.as_object().ok_or_else(|| {
                    Error::InvalidInput(format!("OpenLABEL {key} must be an object map"))
                })?;
                if map.len() > MAX_OPENLABEL_ENTRIES {
                    return Err(Error::LimitExceeded(format!(
                        "OpenLABEL {key} entries exceed {MAX_OPENLABEL_ENTRIES}"
                    )));
                }
                if *key == "objects" {
                    object_data = map.values().map(count_object_data).sum();
                }
                if *key == "frames" {
                    frame_object_data = map.values().map(count_frame_object_data).sum();
                }
                map.len()
            }
        };
        total_entries = total_entries.saturating_add(count);
        collection_counts.push((*key, count));
        if rows.len() >= MAX_OPENLABEL_ROWS {
            return Err(Error::LimitExceeded(format!(
                "OpenLABEL rows exceed {MAX_OPENLABEL_ROWS}"
            )));
        }
        rows.push(vec![
            (*key).into(),
            count.to_string(),
            (*description).into(),
        ]);
    }
    if total_entries == 0 && openlabel.get("metadata").is_none() {
        return Err(Error::InvalidInput(
            "OpenLABEL openlabel object contains no recognized collections or metadata".into(),
        ));
    }
    let metadata = openlabel.get("metadata").and_then(Value::as_object);
    if openlabel.contains_key("metadata") && metadata.is_none() {
        return Err(Error::InvalidInput(
            "OpenLABEL metadata must be an object".into(),
        ));
    }
    let version = metadata
        .and_then(|map| map.get("schema_version").or_else(|| map.get("version")))
        .and_then(Value::as_str)
        .map_or_else(|| "—".into(), truncate);
    let metadata_fields = metadata.map_or(0, serde_json::Map::len);
    let metadata = format!(
        "Schema version: {version}\nMetadata fields: {metadata_fields}\nCollections: {}\nObject data entries: {object_data}\nFrame object-data entries: {frame_object_data}",
        collection_counts
            .iter()
            .filter(|(_, count)| *count > 0)
            .count()
    );
    warnings.push("OpenLABEL object/attribute values, bounding-box coordinates, sensor frames, image/point-cloud resources, ontology URLs and external references are omitted; no sensor data or network resource is opened".into());
    warnings.push("OpenLABEL collection maps and nested annotation counts are bounded; labels are summarized without scenario-tag evaluation or tracking execution".into());
    Ok((
        TableData {
            headers: vec!["Collection".into(), "N".into(), "Content".into()],
            rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn count_object_data(value: &Value) -> usize {
    value
        .as_object()
        .and_then(|object| object.get("object_data"))
        .map_or(0, collection_len)
}

fn count_frame_object_data(value: &Value) -> usize {
    value
        .as_object()
        .and_then(|frame| frame.get("objects"))
        .map_or(0, collection_len)
}

fn collection_len(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.len(),
        Value::Object(map) => map.len(),
        _ => 0,
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OPENLABEL_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_OPENLABEL_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_OPENLABEL_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "OpenLABEL JSON nesting exceeds {MAX_OPENLABEL_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_OPENLABEL_VALUES {
        return Err(Error::LimitExceeded(format!(
            "OpenLABEL JSON contains more than {MAX_OPENLABEL_VALUES} values"
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
        Value::String(value) if value.len() > MAX_OPENLABEL_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "OpenLABEL JSON string exceeds {MAX_OPENLABEL_STRING_BYTES} bytes"
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
                if depth > MAX_OPENLABEL_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "OpenLABEL JSON nesting exceeds {MAX_OPENLABEL_DEPTH} levels"
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
    fn recognizes_openlabel_root() {
        assert!(looks_like_prefix(br#"{"openlabel":{"objects":{}}}"#));
        assert!(!looks_like_prefix(br#"{"objects":{}}"#));
    }

    #[test]
    fn summarizes_collections_without_annotation_values() {
        let (table, metadata, warnings) = parse(
            r#"{"openlabel":{"metadata":{"schema_version":"1.0.0","comment":"private"},"objects":{"car":{"object_data":[{"type":"bbox","val":"secret"}]}},"frames":{"1":{"objects":{"car":{"object_data":[{"type":"point","val":"private"}]}}}},"relations":{"r1":{"rdf_subject":"car"}},"streams":{"camera":{"uri":"https://private.invalid/cam"}}}}"#,
        ).unwrap();
        assert_eq!(table.rows.len(), 12);
        assert!(metadata.contains("Schema version: 1.0.0"));
        assert!(metadata.contains("Object data entries: 1"));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("private"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("attribute values"))
        );
    }
}
