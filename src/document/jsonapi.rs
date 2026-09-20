//! Bounded JSON:API 1.1 resource-document previews.
//!
//! JSON:API documents contain primary data, optional included resources,
//! relationships, links and metadata. This adapter renders resource type/id and
//! structural counts only; attributes, link values and meta/error payloads are
//! omitted and no API endpoint is contacted.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_JSONAPI_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSONAPI_DEPTH: usize = 100;
const MAX_JSONAPI_VALUES: usize = 300_000;
const MAX_JSONAPI_RESOURCES: usize = 100_000;
const MAX_JSONAPI_ROWS: usize = 200_000;
const MAX_JSONAPI_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSONAPI_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) else {
        return text.contains("\"data\"")
            && (text.contains("\"relationships\"") || text.contains("\"included\""));
    };
    let Some(data) = object.get("data") else {
        return object
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty());
    };
    let valid_resource = |resource: &serde_json::Map<String, Value>| {
        resource.get("type").and_then(Value::as_str).is_some()
            && (resource.get("id").and_then(Value::as_str).is_some()
                || resource.get("lid").and_then(Value::as_str).is_some())
    };
    data.as_object().is_some_and(valid_resource)
        || data.as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_object().is_some_and(valid_resource))
        })
}

struct JsonApiPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for JsonApiPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "json-api".into();
        if page.title.is_empty() {
            page.title = "JSON:API 1.1".into();
        }
        page.description =
            "JSON:API resource identifiers and relationship structure are rendered inertly; attribute values, links and API operations are not displayed or resolved".into();
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
        options.max_input_bytes.min(MAX_JSONAPI_BYTES),
        "JSON:API input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("JSON:API must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "JSON:API 1.1".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = JsonApiPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct Summary {
    rows: Vec<Vec<String>>,
    primary: usize,
    included: usize,
    relationships: usize,
    attributes: usize,
    errors: usize,
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_JSONAPI_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON:API exceeds {MAX_JSONAPI_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid JSON:API: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("JSON:API root must be an object".into()))?;
    let primary_count = root.get("data").map_or(0, |data| match data {
        Value::Array(items) => items.len(),
        Value::Null => 0,
        _ => 1,
    });
    let included_count = root
        .get("included")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if primary_count.saturating_add(included_count) > MAX_JSONAPI_RESOURCES {
        return Err(Error::LimitExceeded(format!(
            "JSON:API resources exceed {MAX_JSONAPI_RESOURCES}"
        )));
    }
    if root.contains_key("data") && root.contains_key("errors") {
        return Err(Error::InvalidInput(
            "JSON:API data and errors members must not coexist".into(),
        ));
    }
    if root.contains_key("included") && !root.contains_key("data") {
        return Err(Error::InvalidInput(
            "JSON:API included requires a data member".into(),
        ));
    }
    for key in ["jsonapi", "meta", "links"] {
        if let Some(value) = root.get(key)
            && !value.is_object()
        {
            return Err(Error::InvalidInput(format!(
                "JSON:API top-level {key} must be an object"
            )));
        }
    }
    let mut summary = Summary::default();
    let mut warnings = Vec::new();
    if let Some(data) = root.get("data") {
        match data {
            Value::Null => {}
            Value::Object(_) => {
                summary.primary = 1;
                add_resource(data, "primary", &mut summary)?;
            }
            Value::Array(items) => {
                summary.primary = items.len();
                for item in items {
                    add_resource(item, "primary", &mut summary)?;
                }
            }
            _ => {
                return Err(Error::InvalidInput(
                    "JSON:API data must be an object, array or null".into(),
                ));
            }
        }
    }
    if let Some(included) = root.get("included") {
        let items = included
            .as_array()
            .ok_or_else(|| Error::InvalidInput("JSON:API included must be an array".into()))?;
        summary.included = items.len();
        for item in items {
            add_resource(item, "included", &mut summary)?;
        }
    }
    if let Some(errors) = root.get("errors") {
        let errors = errors
            .as_array()
            .ok_or_else(|| Error::InvalidInput("JSON:API errors must be an array".into()))?;
        summary.errors = errors.len();
        if summary.primary == 0 && summary.included == 0 && errors.is_empty() {
            return Err(Error::InvalidInput(
                "JSON:API errors must not be empty".into(),
            ));
        }
    }
    if summary.rows.is_empty() && summary.errors > 0 {
        summary.rows.push(vec![
            "errors".into(),
            "—".into(),
            "—".into(),
            "—".into(),
            format!("{} error objects", summary.errors),
        ]);
    }
    if summary.rows.is_empty() && root.contains_key("data") {
        summary.rows.push(vec![
            "primary".into(),
            "—".into(),
            "—".into(),
            "0".into(),
            "no primary resources".into(),
        ]);
    }
    if summary.rows.is_empty() {
        if root.get("meta").is_none() {
            return Err(Error::InvalidInput(
                "JSON:API requires data, errors, meta or an extension member".into(),
            ));
        }
        summary.rows.push(vec![
            "meta".into(),
            "—".into(),
            "—".into(),
            "—".into(),
            "metadata omitted".into(),
        ]);
    }
    let metadata = format!(
        "Primary resources: {}\nIncluded resources: {}\nRelationships: {}\nAttribute keys: {}\nErrors: {}\nTop-level links: {}",
        summary.primary,
        summary.included,
        summary.relationships,
        summary.attributes,
        summary.errors,
        usize::from(root.contains_key("links"))
    );
    warnings.push("JSON:API attributes, relationship linkage values, meta/error details, link URLs and JSON:API extensions are omitted; no endpoint, URL or API operation is accessed".into());
    warnings.push("JSON:API resource type/id pairs are displayed as inert identifiers; compound-document linkage is counted but not dereferenced or validated against a server".into());
    Ok((
        TableData {
            headers: vec![
                "Kind".into(),
                "Type / ID".into(),
                "Status".into(),
                "Attrs".into(),
                "Relationships".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn add_resource(value: &Value, kind: &str, summary: &mut Summary) -> Result<()> {
    let object = value.as_object().ok_or_else(|| {
        Error::InvalidInput(format!("JSON:API {kind} resource must be an object"))
    })?;
    let resource_type = required_string(object, "type", kind)?;
    let identifier = object
        .get("id")
        .or_else(|| object.get("lid"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::InvalidInput(format!(
                "JSON:API {kind} resource requires string id or lid"
            ))
        })?;
    if identifier.is_empty() {
        return Err(Error::InvalidInput(format!(
            "JSON:API {kind} resource id/lid must not be empty"
        )));
    }
    let attributes = object.get("attributes").map_or(Ok(0), |value| {
        value.as_object().map(|map| map.len()).ok_or_else(|| {
            Error::InvalidInput(format!("JSON:API {kind} attributes must be an object"))
        })
    })?;
    let relationships = object.get("relationships").map_or(Ok(0), |value| {
        value.as_object().map(|map| map.len()).ok_or_else(|| {
            Error::InvalidInput(format!("JSON:API {kind} relationships must be an object"))
        })
    })?;
    summary.attributes = summary.attributes.saturating_add(attributes);
    summary.relationships = summary.relationships.saturating_add(relationships);
    let status = object
        .get("meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("status"))
        .and_then(Value::as_str)
        .map_or_else(|| "—".into(), truncate);
    push_row(
        summary,
        vec![
            kind.into(),
            truncate(&format!("{resource_type}/{identifier}")),
            status,
            attributes.to_string(),
            relationships.to_string(),
        ],
    )
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    kind: &str,
) -> Result<&'a str> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput(format!("JSON:API {kind} requires string {key}")))?;
    if value.is_empty() {
        return Err(Error::InvalidInput(format!(
            "JSON:API {kind} {key} must not be empty"
        )));
    }
    Ok(value)
}

fn push_row(summary: &mut Summary, row: Vec<String>) -> Result<()> {
    if summary.rows.len() >= MAX_JSONAPI_ROWS {
        return Err(Error::LimitExceeded(format!(
            "JSON:API rows exceed {MAX_JSONAPI_ROWS}"
        )));
    }
    summary.rows.push(row);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_JSONAPI_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_JSONAPI_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_JSONAPI_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "JSON:API nesting exceeds {MAX_JSONAPI_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_JSONAPI_VALUES {
        return Err(Error::LimitExceeded(format!(
            "JSON:API contains more than {MAX_JSONAPI_VALUES} values"
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
        Value::String(value) if value.len() > MAX_JSONAPI_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "JSON:API string exceeds {MAX_JSONAPI_STRING_BYTES} bytes"
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
                if depth > MAX_JSONAPI_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "JSON:API nesting exceeds {MAX_JSONAPI_DEPTH} levels"
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
    fn recognizes_json_api_resource_document() {
        assert!(looks_like_prefix(
            br#"{"data":{"type":"articles","id":"1","relationships":{}}}"#
        ));
        assert!(!looks_like_prefix(br#"{"data":{"title":"plain JSON"}}"#));
    }

    #[test]
    fn summarizes_compound_document_without_attribute_values() {
        let (table, metadata, warnings) = parse(
            r#"{"jsonapi":{"version":"1.1"},"data":{"type":"articles","id":"1","attributes":{"title":"secret title"},"relationships":{"author":{"data":{"type":"people","id":"9"}}}},"included":[{"type":"people","id":"9","attributes":{"name":"private"}}],"links":{"self":"https://private.invalid/articles/1?token=secret"}}"#,
        ).unwrap();
        assert_eq!(table.rows.len(), 2);
        assert!(metadata.contains("Primary resources: 1"));
        assert!(metadata.contains("Included resources: 1"));
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
                .any(|warning| warning.contains("attributes"))
        );
    }

    #[test]
    fn accepts_errors_only_and_empty_primary_data_documents() {
        let (errors_table, _, _) =
            parse(r#"{"errors":[{"status":"404","title":"secret error"}]}"#).unwrap();
        assert_eq!(errors_table.rows[0][0], "errors");
        let (empty_table, _, _) = parse(r#"{"data":[]}"#).unwrap();
        assert_eq!(empty_table.rows[0][0], "primary");
    }
}
