//! Bounded OASIS TAXII 2.1 JSON resource previews.
//!
//! TAXII resources include collection metadata, object envelopes, manifests,
//! discovery responses, statuses and errors. This adapter renders resource
//! structure and counts only; URLs, descriptions, STIX payloads and TAXII
//! endpoints are not displayed, dereferenced or contacted.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_TAXII_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TAXII_DEPTH: usize = 100;
const MAX_TAXII_VALUES: usize = 300_000;
const MAX_TAXII_ROWS: usize = 200_000;
const MAX_TAXII_ENTRIES: usize = 100_000;
const MAX_TAXII_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_TAXII_DISPLAY_BYTES: usize = 256;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) {
        let object_envelope =
            object
                .get("objects")
                .and_then(Value::as_array)
                .is_some_and(|objects| {
                    object.get("more").is_some()
                        || object.get("next").is_some()
                        || objects.iter().any(|entry| {
                            entry.as_object().is_some_and(|item| {
                                item.contains_key("date_added") || item.contains_key("media_type")
                            })
                        })
                });
        return object_envelope
            || (object.get("title").is_some() && object.get("can_read").is_some())
            || object.get("api_roots").and_then(Value::as_array).is_some()
            || object.get("status").is_some() && object.get("request_timestamp").is_some();
    }
    (text.contains("\"objects\"")
        && (text.contains("\"date_added\"") || text.contains("\"media_type\"")))
        || text.contains("\"api_roots\"")
}

struct TaxiiPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for TaxiiPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "taxii-json".into();
        if page.title.is_empty() {
            page.title = "TAXII 2.1 JSON".into();
        }
        page.description =
            "TAXII resource metadata is rendered inertly; STIX payloads, URLs and TAXII endpoints are not displayed or contacted".into();
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
        options.max_input_bytes.min(MAX_TAXII_BYTES),
        "TAXII JSON input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("TAXII JSON must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "TAXII 2.1 JSON".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = TaxiiPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_TAXII_BYTES {
        return Err(Error::LimitExceeded(format!(
            "TAXII JSON exceeds {MAX_TAXII_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid TAXII JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("TAXII JSON root must be an object".into()))?;
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let resource_kind;
    let mut resource_id = root.get("id").and_then(Value::as_str).unwrap_or("—");
    if let Some(objects) = root.get("objects") {
        let objects = objects
            .as_array()
            .ok_or_else(|| Error::InvalidInput("TAXII objects must be an array".into()))?;
        if objects.len() > MAX_TAXII_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "TAXII objects exceed {MAX_TAXII_ENTRIES}"
            )));
        }
        let manifest_like = objects.iter().all(|object| {
            object.as_object().is_some_and(|object| {
                object.contains_key("date_added") || object.contains_key("media_type")
            })
        });
        resource_kind = if manifest_like {
            "manifest"
        } else {
            "envelope"
        };
        for object in objects {
            let object = object.as_object().ok_or_else(|| {
                Error::InvalidInput("TAXII object entry must be an object".into())
            })?;
            let id = object.get("id").and_then(Value::as_str).unwrap_or("—");
            let media = object
                .get("media_type")
                .and_then(Value::as_str)
                .map_or_else(|| "—".into(), truncate);
            let version = object
                .get("version")
                .or_else(|| object.get("date_added"))
                .and_then(Value::as_str)
                .map_or_else(|| "—".into(), truncate);
            rows.push(vec!["object".into(), truncate(id), version, media]);
        }
        if rows.is_empty() {
            rows.push(vec![
                "envelope".into(),
                "—".into(),
                "0 objects".into(),
                "—".into(),
            ]);
        }
    } else if let Some(api_roots) = root.get("api_roots") {
        let api_roots = api_roots
            .as_array()
            .ok_or_else(|| Error::InvalidInput("TAXII api_roots must be an array".into()))?;
        resource_kind = "discovery";
        for api_root in api_roots {
            rows.push(vec![
                "api-root".into(),
                "—".into(),
                "—".into(),
                "URL omitted".into(),
            ]);
            if rows.len() > MAX_TAXII_ROWS {
                return Err(Error::LimitExceeded(format!(
                    "TAXII rows exceed {MAX_TAXII_ROWS}"
                )));
            }
            let _ = api_root;
        }
    } else if root.get("title").is_some() && root.get("can_read").is_some() {
        resource_kind = "collection";
        rows.push(vec![
            "collection".into(),
            truncate(root.get("title").and_then(Value::as_str).unwrap_or("—")),
            "read/write flags omitted".into(),
            "media types omitted".into(),
        ]);
    } else if root.get("status").is_some() && root.get("request_timestamp").is_some() {
        resource_kind = "status";
        rows.push(vec![
            "status".into(),
            truncate(root.get("status").and_then(Value::as_str).unwrap_or("—")),
            format!("total {}", number_text(root.get("total_count"))),
            format!(
                "success {} · failure {}",
                number_text(root.get("success_count")),
                number_text(root.get("failure_count"))
            ),
        ]);
    } else if root.get("title").is_some() || root.get("description").is_some() {
        resource_kind = "error";
        rows.push(vec![
            "error".into(),
            truncate(root.get("title").and_then(Value::as_str).unwrap_or("—")),
            format!("code {}", number_text(root.get("code"))),
            "description omitted".into(),
        ]);
    } else {
        return Err(Error::InvalidInput(
            "TAXII JSON envelope, manifest, collection, discovery, status or error resource was not recognized".into(),
        ));
    }
    if rows.len() > MAX_TAXII_ROWS {
        return Err(Error::LimitExceeded(format!(
            "TAXII rows exceed {MAX_TAXII_ROWS}"
        )));
    }
    if root.get("id").is_none() {
        resource_id = "—";
    }
    let metadata = format!(
        "Resource: {resource_kind}\nID: {}\nRows: {}\nMore: {}\nNext page: {}",
        truncate(resource_id),
        rows.len(),
        root.get("more")
            .and_then(Value::as_bool)
            .map_or("—", |value| if value { "true" } else { "false" }),
        if root.get("next").is_some() {
            "present"
        } else {
            "—"
        }
    );
    warnings.push("TAXII URLs, descriptions, STIX object payloads, authorization metadata and server endpoints are omitted; no TAXII client, HTTP request or resource fetch runs".into());
    warnings.push("TAXII 2.1 envelope/manifest counts are bounded; object IDs and timestamps are displayed as inert metadata without STIX dereferencing".into());
    Ok((
        TableData {
            headers: vec![
                "Kind".into(),
                "ID / title".into(),
                "Version / count".into(),
                "Detail".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn number_text(value: Option<&Value>) -> String {
    value.map_or_else(|| "—".into(), |value| value.to_string())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_TAXII_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_TAXII_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_TAXII_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "TAXII JSON nesting exceeds {MAX_TAXII_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_TAXII_VALUES {
        return Err(Error::LimitExceeded(format!(
            "TAXII JSON contains more than {MAX_TAXII_VALUES} values"
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
        Value::String(value) if value.len() > MAX_TAXII_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "TAXII JSON string exceeds {MAX_TAXII_STRING_BYTES} bytes"
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
                if depth > MAX_TAXII_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "TAXII JSON nesting exceeds {MAX_TAXII_DEPTH} levels"
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
    fn recognizes_taxii_resources() {
        assert!(looks_like_prefix(
            br#"{"objects":[{"id":"indicator--1","media_type":"application/stix+json"}]}"#
        ));
        assert!(looks_like_prefix(
            br#"{"title":"Collection","can_read":true}"#
        ));
        assert!(!looks_like_prefix(
            br#"{"title":"plain","description":"x"}"#
        ));
    }

    #[test]
    fn summarizes_manifest_without_urls_or_payloads() {
        let (table, metadata, warnings) = parse(
            r#"{"objects":[{"id":"indicator--1","date_added":"2024-01-01T00:00:00Z","version":"2024-01-02T00:00:00Z","media_type":"application/stix+json;version=2.1","url":"https://private.invalid"}],"more":false,"next":"https://private.invalid/next"}"#,
        ).unwrap();
        assert_eq!(table.rows.len(), 1);
        assert!(metadata.contains("Resource: manifest"));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("private"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("TAXII")));
    }
}
