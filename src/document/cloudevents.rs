//! Bounded CloudEvents JSON 1.0 previews.
//!
//! CloudEvents JSON envelopes carry event metadata and an optional data payload.
//! This adapter renders only inert metadata (with URI query values masked): data,
//! extension values and external schemas are summarized without being displayed,
//! decoded, fetched, or executed.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_CLOUDEVENTS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CLOUDEVENTS_DEPTH: usize = 100;
const MAX_CLOUDEVENTS_VALUES: usize = 300_000;
const MAX_CLOUDEVENTS_EVENTS: usize = 100_000;
const MAX_CLOUDEVENTS_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_CLOUDEVENTS_DISPLAY_BYTES: usize = 512;
const MAX_CLOUDEVENTS_URI_BYTES: usize = 512;

/// Detect a CloudEvents JSON object or batch from a bounded prefix.
pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return match value {
            Value::Object(object) => looks_like_event_object(&object),
            Value::Array(items) => items
                .first()
                .and_then(Value::as_object)
                .is_some_and(looks_like_event_object),
            _ => false,
        };
    }
    text.contains("\"specversion\"")
        && text.contains("\"type\"")
        && text.contains("\"source\"")
        && text.contains("\"id\"")
}

fn looks_like_event_object(object: &serde_json::Map<String, Value>) -> bool {
    ["specversion", "type", "source", "id"]
        .iter()
        .all(|key| object.get(*key).and_then(Value::as_str).is_some())
}

struct CloudEventsPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CloudEventsPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "cloudevents".into();
        if page.title.is_empty() {
            page.title = "CloudEvents JSON".into();
        }
        page.description =
            "CloudEvents envelope metadata is rendered inertly; data, extension values, and external schemas are not displayed or resolved".into();
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
        options.max_input_bytes.min(MAX_CLOUDEVENTS_BYTES),
        "CloudEvents JSON input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("CloudEvents JSON must be UTF-8 JSON: {error}"))
    })?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "CloudEvents JSON".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = CloudEventsPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_CLOUDEVENTS_BYTES {
        return Err(Error::LimitExceeded(format!(
            "CloudEvents JSON exceeds {MAX_CLOUDEVENTS_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid CloudEvents JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let events: Vec<&Value> = match &value {
        Value::Object(_) => vec![&value],
        Value::Array(items) => {
            if items.len() > MAX_CLOUDEVENTS_EVENTS {
                return Err(Error::LimitExceeded(format!(
                    "CloudEvents batch exceeds {MAX_CLOUDEVENTS_EVENTS} events"
                )));
            }
            items.iter().collect()
        }
        _ => {
            return Err(Error::InvalidInput(
                "CloudEvents JSON root must be an event object or batch array".into(),
            ));
        }
    };
    let mut rows = Vec::with_capacity(events.len().min(MAX_CLOUDEVENTS_EVENTS));
    let mut warnings = Vec::new();
    let mut data_count = 0usize;
    let mut base64_count = 0usize;
    let mut schema_count = 0usize;
    let mut extension_count = 0usize;
    let mut content_types = std::collections::BTreeSet::new();
    let mut specversions = std::collections::BTreeSet::new();
    for (index, event) in events.iter().enumerate() {
        let object = event.as_object().ok_or_else(|| {
            Error::InvalidInput(format!(
                "CloudEvents batch item {} must be an object",
                index + 1
            ))
        })?;
        let specversion = required_string(object, "specversion", index)?;
        if specversion != "1.0" {
            return Err(Error::Unsupported(format!(
                "CloudEvents specversion '{specversion}' is unsupported (expected 1.0)"
            )));
        }
        specversions.insert(specversion.to_owned());
        let event_type = required_string(object, "type", index)?;
        let source = required_string(object, "source", index)?;
        let id = required_string(object, "id", index)?;
        let time = optional_string(object, "time", index)?.map_or_else(|| "—".into(), truncate);
        let subject =
            optional_string(object, "subject", index)?.map_or_else(|| "—".into(), truncate);
        let content_type =
            optional_string(object, "datacontenttype", index)?.map_or_else(|| "—".into(), truncate);
        if content_type != "—" && content_type != "(invalid)" {
            content_types.insert(content_type.clone());
        }
        if object.contains_key("data") && object.contains_key("data_base64") {
            return Err(Error::InvalidInput(format!(
                "CloudEvents batch item {} cannot contain both data and data_base64",
                index + 1
            )));
        }
        let data = if let Some(data) = object.get("data") {
            data_count = data_count.saturating_add(1);
            format!(
                "{}/{}B",
                json_type(data),
                serde_json::to_string(data).map_or(0, |s| s.len())
            )
        } else if let Some(encoded) = object.get("data_base64") {
            let encoded = encoded.as_str().ok_or_else(|| {
                Error::InvalidInput(format!(
                    "CloudEvents batch item {} data_base64 must be a string",
                    index + 1
                ))
            })?;
            base64_count = base64_count.saturating_add(1);
            format!("base64 ({} chars)", encoded.len())
        } else {
            "—".into()
        };
        if object.contains_key("dataschema") {
            optional_string(object, "dataschema", index)?;
            schema_count = schema_count.saturating_add(1);
        }
        let extension = object
            .keys()
            .filter(|key| !STANDARD_ATTRIBUTES.contains(&key.as_str()))
            .count();
        extension_count = extension_count.saturating_add(extension);
        rows.push(vec![
            truncate(event_type),
            truncate(id),
            format!("{} · {} · {} · {data}", source_label(source), time, subject),
        ]);
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "CloudEvents JSON batch must contain at least one event".into(),
        ));
    }
    let metadata = format!(
        "Events: {}\nSpecversion: {}\nWith data: {}\nWith data_base64: {}\nWith dataschema: {}\nExtension attributes: {}",
        rows.len(),
        specversions.into_iter().collect::<Vec<_>>().join(", "),
        data_count,
        base64_count,
        schema_count,
        extension_count
    );
    let metadata = format!(
        "{metadata}\nContent types: {}",
        if content_types.is_empty() {
            "—".into()
        } else {
            content_types.into_iter().collect::<Vec<_>>().join(", ")
        }
    );
    warnings.push("CloudEvents data/data_base64 payloads are summarized by type and size only; payloads are never decoded, rendered, executed or sent to a network endpoint".into());
    warnings.push("CloudEvents source and dataschema URI values are displayed inertly with query values masked; no URI is fetched or resolved".into());
    if extension_count > 0 {
        warnings.push(format!(
            "{extension_count} CloudEvents extension attribute(s) were counted but their values were omitted"
        ));
    }
    Ok((
        TableData {
            headers: vec![
                "Type".into(),
                "ID".into(),
                "Context (source · time · subject · data)".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

const STANDARD_ATTRIBUTES: &[&str] = &[
    "specversion",
    "type",
    "source",
    "subject",
    "id",
    "time",
    "datacontenttype",
    "dataschema",
    "data",
    "data_base64",
];

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    index: usize,
) -> Result<&'a str> {
    let value = object.get(key).and_then(Value::as_str).ok_or_else(|| {
        Error::InvalidInput(format!(
            "CloudEvents batch item {} requires string {key}",
            index + 1
        ))
    })?;
    if value.is_empty() {
        return Err(Error::InvalidInput(format!(
            "CloudEvents batch item {} {key} must not be empty",
            index + 1
        )));
    }
    Ok(value)
}

fn optional_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    index: usize,
) -> Result<Option<&'a str>> {
    let Some(value) = object.get(key) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(|| {
        Error::InvalidInput(format!(
            "CloudEvents batch item {} {key} must be a string",
            index + 1
        ))
    })?;
    if value.is_empty() {
        return Err(Error::InvalidInput(format!(
            "CloudEvents batch item {} {key} must not be empty",
            index + 1
        )));
    }
    Ok(Some(value))
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
    if value.len() <= MAX_CLOUDEVENTS_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_CLOUDEVENTS_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn mask_uri(value: &str) -> String {
    let mut masked = value.to_owned();
    if let Some(query_start) = masked.find('?') {
        let fragment_start = masked[query_start..]
            .find('#')
            .map_or(masked.len(), |offset| query_start + offset);
        let query = &masked[query_start + 1..fragment_start];
        let pairs = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let key = pair.split('=').next().unwrap_or("");
                if key.is_empty() {
                    "[redacted]".to_owned()
                } else {
                    format!("{key}=[redacted]")
                }
            })
            .collect::<Vec<_>>();
        masked.replace_range(query_start + 1..fragment_start, &pairs.join("&"));
    }
    truncate(
        &masked
            .chars()
            .take(MAX_CLOUDEVENTS_URI_BYTES)
            .collect::<String>(),
    )
}

fn source_label(value: &str) -> String {
    let masked = mask_uri(value);
    let Some(scheme_end) = masked.find("://") else {
        return truncate(&masked);
    };
    let authority_start = scheme_end + 3;
    let authority_end = masked[authority_start..]
        .find(['/', '?', '#'])
        .map_or(masked.len(), |offset| authority_start + offset);
    let authority = &masked[authority_start..authority_end];
    if authority.is_empty() {
        return truncate(&masked);
    }
    let mut label = authority.to_owned();
    if masked[authority_end..].contains('?') {
        label.push_str("?…");
    }
    truncate(&label)
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_CLOUDEVENTS_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "CloudEvents JSON nesting exceeds {MAX_CLOUDEVENTS_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_CLOUDEVENTS_VALUES {
        return Err(Error::LimitExceeded(format!(
            "CloudEvents JSON contains more than {MAX_CLOUDEVENTS_VALUES} values"
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
        Value::String(value) if value.len() > MAX_CLOUDEVENTS_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "CloudEvents JSON string exceeds {MAX_CLOUDEVENTS_STRING_BYTES} bytes"
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
                if depth > MAX_CLOUDEVENTS_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "CloudEvents JSON nesting exceeds {MAX_CLOUDEVENTS_DEPTH} levels"
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
    fn recognizes_cloud_event_object_and_batch() {
        assert!(looks_like_prefix(
            br#"{"specversion":"1.0","type":"com.example.created","source":"/demo","id":"1"}"#
        ));
        assert!(looks_like_prefix(
            br#"[{"specversion":"1.0","type":"com.example.created","source":"/demo","id":"1"}]"#
        ));
        assert!(!looks_like_prefix(
            br#"{"type":"thing","source":"/demo","id":"1"}"#
        ));
        assert!(!looks_like_prefix(
            br#"{"version":"https://jsonfeed.org/version/1.1","title":"Feed","items":[{"specversion":"1.0","type":"thing","source":"/demo","id":"1"}]}"#
        ));
    }

    #[test]
    fn summarizes_payload_and_masks_uri_values() {
        let (table, metadata, warnings) = parse(
            r#"{"specversion":"1.0","type":"com.example.created","source":"https://example.invalid/events?token=secret&kind=test","id":"evt-1","time":"2024-05-12T00:00:00Z","subject":"item-1","datacontenttype":"application/json","dataschema":"https://schema.invalid/event.json","traceparent":"secret-extension","data":{"message":"very-secret"}}"#,
        )
        .unwrap();
        assert!(metadata.contains("Events: 1"));
        assert_eq!(table.rows[0][0], "com.example.created");
        assert!(table.rows[0][2].contains("example.invalid"));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret"))
        );
        assert!(table.rows[0][2].contains("object/"));
        assert!(metadata.contains("Extension attributes: 1"));
        assert!(warnings.iter().any(|warning| warning.contains("payloads")));
    }

    #[test]
    fn accepts_json_batch_and_base64_summary() {
        let (table, metadata, _) = parse(
            r#"[{"specversion":"1.0","type":"a","source":"/a","id":"1"},{"specversion":"1.0","type":"b","source":"/b","id":"2","data_base64":"c2VjcmV0"}]"#,
        )
        .unwrap();
        assert_eq!(table.rows.len(), 2);
        assert!(table.rows[1][2].contains("base64 (8 chars)"));
        assert!(metadata.contains("With data_base64: 1"));
    }

    #[test]
    fn rejects_conflicting_payload_encodings() {
        let result = parse(
            r#"{"specversion":"1.0","type":"a","source":"/a","id":"1","data":null,"data_base64":"AA=="}"#,
        );
        assert!(result.is_err());
    }
}
