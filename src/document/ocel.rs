//! Bounded OCEL 2.0 JSON object-centric event-log previews.
//!
//! OCEL 2.0 stores event types, events, object types and objects together with
//! event-to-object and object-to-object relationships. This adapter renders
//! identifiers, types, timestamps and counts only; attribute values, qualifiers
//! and external references remain inert and are never evaluated or fetched.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_OCEL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OCEL_DEPTH: usize = 100;
const MAX_OCEL_VALUES: usize = 300_000;
const MAX_OCEL_ROWS: usize = 200_000;
const MAX_OCEL_NODES: usize = 100_000;
const MAX_OCEL_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_OCEL_DISPLAY_BYTES: usize = 512;
const ATTRIBUTE_TYPES: &[&str] = &["string", "time", "integer", "float", "boolean"];

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) {
        return ["eventTypes", "events", "objectTypes", "objects"]
            .iter()
            .all(|key| object.get(*key).and_then(Value::as_array).is_some());
    }
    ["eventTypes", "events", "objectTypes", "objects"]
        .iter()
        .all(|key| text.contains(&format!("\"{key}\"")))
}

struct OcelPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OcelPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "ocel-json".into();
        if page.title.is_empty() {
            page.title = "OCEL 2.0 JSON".into();
        }
        page.description =
            "OCEL 2.0 event and object metadata is rendered inertly; attribute values, qualifiers and external references are not displayed or resolved".into();
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
        options.max_input_bytes.min(MAX_OCEL_BYTES),
        "OCEL JSON input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("OCEL JSON must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OCEL 2.0 JSON".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = OcelPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct Summary {
    rows: Vec<Vec<String>>,
    event_types: usize,
    object_types: usize,
    events: usize,
    objects: usize,
    event_attributes: usize,
    object_attributes: usize,
    event_relationships: usize,
    object_relationships: usize,
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_OCEL_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OCEL JSON exceeds {MAX_OCEL_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid OCEL JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("OCEL JSON root must be an object".into()))?;
    let event_types = required_array(root, "eventTypes")?;
    let events = required_array(root, "events")?;
    let object_types = required_array(root, "objectTypes")?;
    let objects = required_array(root, "objects")?;
    if event_types
        .len()
        .saturating_add(events.len())
        .saturating_add(object_types.len())
        .saturating_add(objects.len())
        > MAX_OCEL_NODES
    {
        return Err(Error::LimitExceeded(format!(
            "OCEL nodes exceed {MAX_OCEL_NODES}"
        )));
    }
    let mut summary = Summary::default();
    let mut warnings = Vec::new();
    for (index, event_type) in event_types.iter().enumerate() {
        let object = event_type.as_object().ok_or_else(|| {
            Error::InvalidInput(format!(
                "OCEL eventTypes item {} must be an object",
                index + 1
            ))
        })?;
        let name = required_string(object, "name", "event type", index)?;
        let attrs = definition_attributes(object, "event type", index)?;
        summary.event_types = summary.event_types.saturating_add(1);
        push_row(
            &mut summary,
            vec![
                "event-type".into(),
                truncate(name),
                "—".into(),
                format!("{attrs} attrs"),
                "—".into(),
            ],
        )?;
    }
    for (index, object_type) in object_types.iter().enumerate() {
        let object = object_type.as_object().ok_or_else(|| {
            Error::InvalidInput(format!(
                "OCEL objectTypes item {} must be an object",
                index + 1
            ))
        })?;
        let name = required_string(object, "name", "object type", index)?;
        let attrs = definition_attributes(object, "object type", index)?;
        summary.object_types = summary.object_types.saturating_add(1);
        push_row(
            &mut summary,
            vec![
                "object-type".into(),
                truncate(name),
                "—".into(),
                format!("{attrs} attrs"),
                "—".into(),
            ],
        )?;
    }
    for (index, event) in events.iter().enumerate() {
        let object = event.as_object().ok_or_else(|| {
            Error::InvalidInput(format!("OCEL events item {} must be an object", index + 1))
        })?;
        let id = required_string(object, "id", "event", index)?;
        let event_type = required_string(object, "type", "event", index)?;
        let time = required_string(object, "time", "event", index)?;
        let attrs = attribute_array(object, "attributes", "event", index)?;
        let relationships = relationship_array(object, "relationships", "event", index)?;
        for relationship in relationships {
            validate_relationship(relationship, "event relationship", index)?;
        }
        summary.events = summary.events.saturating_add(1);
        summary.event_attributes = summary.event_attributes.saturating_add(attrs.len());
        summary.event_relationships = summary
            .event_relationships
            .saturating_add(relationships.len());
        push_row(
            &mut summary,
            vec![
                "event".into(),
                truncate(id),
                truncate(event_type),
                truncate(time),
                format!("attrs {} · objects {}", attrs.len(), relationships.len()),
            ],
        )?;
    }
    for (index, item) in objects.iter().enumerate() {
        let object = item.as_object().ok_or_else(|| {
            Error::InvalidInput(format!("OCEL objects item {} must be an object", index + 1))
        })?;
        let id = required_string(object, "id", "object", index)?;
        let object_type = required_string(object, "type", "object", index)?;
        let attrs = attribute_array(object, "attributes", "object", index)?;
        let relationships = relationship_array(object, "relationships", "object", index)?;
        for relationship in relationships {
            validate_relationship(relationship, "object relationship", index)?;
        }
        summary.objects = summary.objects.saturating_add(1);
        summary.object_attributes = summary.object_attributes.saturating_add(attrs.len());
        summary.object_relationships = summary
            .object_relationships
            .saturating_add(relationships.len());
        push_row(
            &mut summary,
            vec![
                "object".into(),
                truncate(id),
                truncate(object_type),
                format!("{} attrs", attrs.len()),
                format!("objects {}", relationships.len()),
            ],
        )?;
    }
    if summary.rows.is_empty() {
        return Err(Error::InvalidInput(
            "OCEL JSON contains no event, object or type rows".into(),
        ));
    }
    let metadata = format!(
        "Event types: {}\nObject types: {}\nEvents: {}\nObjects: {}\nEvent attributes: {}\nObject attributes: {}\nEvent→object relationships: {}\nObject→object relationships: {}",
        summary.event_types,
        summary.object_types,
        summary.events,
        summary.objects,
        summary.event_attributes,
        summary.object_attributes,
        summary.event_relationships,
        summary.object_relationships
    );
    warnings.push("OCEL attribute values, qualifiers, object IDs in relationship payloads, timestamps beyond their inert text, URLs and external resources are omitted; no process-mining discovery, filtering or execution runs".into());
    warnings.push("OCEL 2.0 JSON arrays and relationship counts are bounded; event/object type names are shown as labels without dereferencing or semantic evaluation".into());
    Ok((
        TableData {
            headers: vec![
                "Kind".into(),
                "ID / name".into(),
                "Type".into(),
                "Time / attrs".into(),
                "Relations".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn required_array<'a>(root: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a [Value]> {
    root.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| Error::InvalidInput(format!("OCEL JSON requires {key} array")))
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    kind: &str,
    index: usize,
) -> Result<&'a str> {
    let value = object.get(key).and_then(Value::as_str).ok_or_else(|| {
        Error::InvalidInput(format!("OCEL {kind} {} requires string {key}", index + 1))
    })?;
    if value.is_empty() {
        return Err(Error::InvalidInput(format!(
            "OCEL {kind} {} {key} must not be empty",
            index + 1
        )));
    }
    Ok(value)
}

fn definition_attributes(
    object: &serde_json::Map<String, Value>,
    kind: &str,
    index: usize,
) -> Result<usize> {
    let Some(attributes) = object.get("attributes") else {
        return Ok(0);
    };
    let attributes = attributes.as_array().ok_or_else(|| {
        Error::InvalidInput(format!(
            "OCEL {kind} {} attributes must be an array",
            index + 1
        ))
    })?;
    for attribute in attributes {
        let attribute = attribute.as_object().ok_or_else(|| {
            Error::InvalidInput(format!(
                "OCEL {kind} {} attribute must be an object",
                index + 1
            ))
        })?;
        required_string(attribute, "name", "attribute", index)?;
        let value_type = required_string(attribute, "type", "attribute", index)?;
        if !ATTRIBUTE_TYPES.contains(&value_type) {
            return Err(Error::Unsupported(format!(
                "OCEL attribute type '{value_type}' is unsupported"
            )));
        }
    }
    Ok(attributes.len())
}

fn attribute_array<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    kind: &str,
    index: usize,
) -> Result<&'a [Value]> {
    object.get(key).map_or(Ok(&[]), |value| {
        value.as_array().map(Vec::as_slice).ok_or_else(|| {
            Error::InvalidInput(format!("OCEL {kind} {} {key} must be an array", index + 1))
        })
    })
}

fn relationship_array<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    kind: &str,
    index: usize,
) -> Result<&'a [Value]> {
    object.get(key).map_or(Ok(&[]), |value| {
        value.as_array().map(Vec::as_slice).ok_or_else(|| {
            Error::InvalidInput(format!("OCEL {kind} {} {key} must be an array", index + 1))
        })
    })
}

fn validate_relationship(value: &Value, kind: &str, index: usize) -> Result<()> {
    let object = value.as_object().ok_or_else(|| {
        Error::InvalidInput(format!("OCEL {kind} {} must be an object", index + 1))
    })?;
    required_string(object, "objectId", kind, index)?;
    Ok(())
}

fn push_row(summary: &mut Summary, row: Vec<String>) -> Result<()> {
    if summary.rows.len() >= MAX_OCEL_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OCEL rows exceed {MAX_OCEL_ROWS}"
        )));
    }
    summary.rows.push(row);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OCEL_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_OCEL_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_OCEL_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "OCEL JSON nesting exceeds {MAX_OCEL_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_OCEL_VALUES {
        return Err(Error::LimitExceeded(format!(
            "OCEL JSON contains more than {MAX_OCEL_VALUES} values"
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
        Value::String(value) if value.len() > MAX_OCEL_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "OCEL JSON string exceeds {MAX_OCEL_STRING_BYTES} bytes"
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
                if depth > MAX_OCEL_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "OCEL JSON nesting exceeds {MAX_OCEL_DEPTH} levels"
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
    fn recognizes_ocel_root() {
        assert!(looks_like_prefix(
            br#"{"eventTypes":[],"events":[],"objectTypes":[],"objects":[]}"#
        ));
        assert!(!looks_like_prefix(br#"{"events":[]}"#));
    }

    #[test]
    fn summarizes_event_object_relationships_without_values() {
        let (table, metadata, warnings) = parse(
            r#"{"eventTypes":[{"name":"create-order","attributes":[{"name":"total","type":"integer"}]}],"objectTypes":[{"name":"order","attributes":[{"name":"secret","type":"string"}]}],"events":[{"id":"e1","type":"create-order","time":"2024-01-01T00:00:00Z","attributes":[{"name":"total","value":7}],"relationships":[{"objectId":"o1","qualifier":"item"}]}],"objects":[{"id":"o1","type":"order","attributes":[{"name":"secret","time":"2024-01-01T00:00:00Z","value":"very-secret"}]}]}"#,
        ).unwrap();
        assert_eq!(table.rows.len(), 4);
        assert!(metadata.contains("Event→object relationships: 1"));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("very-secret") || value.contains("7"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("attribute values"))
        );
    }
}
