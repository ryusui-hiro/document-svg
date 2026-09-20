//! Bounded Postman Collection v2.x previews.
//!
//! Collection files describe API requests and optional scripts. This adapter
//! renders an inert request inventory: URLs are masked for secret-like query
//! values, auth credentials and variable values are omitted, and pre-request/
//! test scripts plus request/response bodies are never executed or displayed.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_POSTMAN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_POSTMAN_DEPTH: usize = 100;
const MAX_POSTMAN_VALUES: usize = 300_000;
const MAX_POSTMAN_ITEMS: usize = 200_000;
const MAX_POSTMAN_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_POSTMAN_TEXT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"info\"")
        && text.contains("\"item\"")
        && (text.contains("postman") || text.contains("schema.getpostman.com"))
}

struct PostmanPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for PostmanPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "postman".into();
        if page.title.is_empty() {
            page.title = "Postman collection".into();
        }
        page.description =
            "Postman request metadata is rendered safely; requests and scripts are never executed"
                .into();
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
        options.max_input_bytes.min(MAX_POSTMAN_BYTES),
        "Postman collection input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Postman collection must be UTF-8 JSON: {error}"))
    })?;
    let (table, metadata, warnings) = parse_collection(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Postman collection".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = PostmanPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_collection(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_POSTMAN_BYTES || text.len() > MAX_POSTMAN_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Postman collection exceeds {MAX_POSTMAN_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text).map_err(|error| {
        Error::InvalidInput(format!("invalid Postman collection JSON: {error}"))
    })?;
    let mut count = 0usize;
    count_values(&value, 0, &mut count)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("Postman collection root must be an object".into()))?;
    let info = root
        .get("info")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::InvalidInput("Postman collection requires an info object".into()))?;
    let name = info.get("name").and_then(Value::as_str).unwrap_or_default();
    let schema = info
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if schema.is_empty() && !text.to_ascii_lowercase().contains("postman") {
        return Err(Error::InvalidInput(
            "JSON document does not have a Postman collection signature".into(),
        ));
    }
    let items = root
        .get("item")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidInput("Postman collection requires an item array".into()))?;
    let mut rows = Vec::new();
    let mut masked = 0usize;
    let mut scripts = 0usize;
    let mut bodies = 0usize;
    walk_items(items, "", &mut rows, &mut masked, &mut scripts, &mut bodies)?;
    let mut warnings = vec![
        "Postman URLs and request metadata are displayed inertly; requests, auth credentials, variables, scripts, headers, cookies and bodies are never executed, fetched or displayed".into(),
    ];
    if masked > 0 {
        warnings.push(format!("{masked} Postman URL query value(s) were masked"));
    }
    if scripts > 0 {
        warnings.push(format!(
            "{scripts} Postman pre-request/test script event(s) were omitted"
        ));
    }
    if bodies > 0 {
        warnings.push(format!(
            "{bodies} Postman request/response body section(s) were omitted"
        ));
    }
    let variables = root
        .get("variable")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let events = root
        .get("event")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let auth = root
        .get("auth")
        .and_then(Value::as_object)
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("none");
    let mut metadata = vec![
        format!("Requests: {}", rows.len()),
        format!("Variables: {variables}"),
        format!("Collection events: {events}"),
        format!("Collection auth: {auth}"),
    ];
    if !name.is_empty() {
        metadata.insert(0, format!("Name: {name}"));
    }
    if !schema.is_empty() {
        metadata.push(format!("Schema: {}", truncate(schema)));
    }
    Ok((
        TableData {
            headers: vec![
                "Folder / request".into(),
                "Method".into(),
                "URL".into(),
                "Status / responses".into(),
                "Auth / scripts".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata.join("\n"),
        warnings,
    ))
}

fn walk_items(
    items: &[Value],
    folder: &str,
    rows: &mut Vec<Vec<String>>,
    masked: &mut usize,
    scripts: &mut usize,
    bodies: &mut usize,
) -> Result<()> {
    for item in items {
        if rows.len() >= MAX_POSTMAN_ITEMS {
            return Err(Error::LimitExceeded(format!(
                "Postman requests exceed {MAX_POSTMAN_ITEMS}"
            )));
        }
        let Some(object) = item.as_object() else {
            continue;
        };
        let item_name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)");
        if let Some(children) = object.get("item").and_then(Value::as_array) {
            let next_folder = if folder.is_empty() {
                item_name.to_owned()
            } else {
                format!("{folder} / {item_name}")
            };
            walk_items(children, &next_folder, rows, masked, scripts, bodies)?;
            continue;
        }
        let Some(request) = object.get("request") else {
            continue;
        };
        let request_object = request.as_object();
        let method = request_object
            .and_then(|v| v.get("method"))
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let raw_url = request_object
            .and_then(|v| v.get("url"))
            .and_then(|v| {
                v.as_str().map(ToOwned::to_owned).or_else(|| {
                    v.as_object()
                        .and_then(|u| u.get("raw"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
            })
            .unwrap_or_default();
        let (url, masked_here) = mask_url(&raw_url);
        *masked += masked_here;
        let auth_type = request_object
            .and_then(|v| v.get("auth"))
            .and_then(Value::as_object)
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("inherit");
        let response_count = object
            .get("response")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let response_statuses = object
            .get("response")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| {
                        v.get("code")
                            .and_then(Value::as_i64)
                            .map(|code| code.to_string())
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        if object.get("event").is_some() {
            *scripts += object
                .get("event")
                .and_then(Value::as_array)
                .map_or(1, Vec::len);
        }
        if request_object.is_some_and(|v| v.contains_key("body")) || response_count > 0 {
            *bodies += 1;
        }
        let status = if response_statuses.is_empty() {
            format!("{response_count} response(s)")
        } else {
            response_statuses
        };
        let auth_scripts = format!(
            "auth: {auth_type}; scripts: {}",
            object
                .get("event")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        );
        let label = if folder.is_empty() {
            item_name.to_owned()
        } else {
            format!("{folder} / {item_name}")
        };
        rows.push(vec![
            truncate(&label),
            method,
            truncate(&url),
            truncate(&status),
            truncate(&auth_scripts),
        ]);
    }
    Ok(())
}

fn mask_url(url: &str) -> (String, usize) {
    let Some((prefix, query)) = url.split_once('?') else {
        return (truncate(url), 0);
    };
    let mut count = 0usize;
    let query = query
        .split('&')
        .map(|part| {
            let Some((key, value)) = part.split_once('=') else {
                return part.to_owned();
            };
            let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
            if [
                "token",
                "secret",
                "password",
                "apikey",
                "authorization",
                "cookie",
                "session",
                "credential",
            ]
            .iter()
            .any(|needle| normalized.contains(needle))
            {
                count += 1;
                format!("{key}=***")
            } else {
                format!("{key}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    (truncate(&format!("{prefix}?{query}")), count)
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_POSTMAN_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_POSTMAN_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_POSTMAN_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "Postman nesting exceeds {MAX_POSTMAN_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_POSTMAN_VALUES {
        return Err(Error::LimitExceeded(format!(
            "Postman collection contains more than {MAX_POSTMAN_VALUES} values"
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
        Value::String(text) if text.len() > MAX_POSTMAN_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "Postman string exceeds {MAX_POSTMAN_STRING_BYTES} bytes"
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
                if depth > MAX_POSTMAN_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "Postman nesting exceeds {MAX_POSTMAN_DEPTH} levels"
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
    fn previews_collection_requests_and_masks_auth_query() {
        let source = r#"{"info":{"name":"Demo","schema":"https://schema.postman.com/json/collection/v2.1.0/collection.json"},"item":[{"name":"Pets","item":[{"name":"List","request":{"method":"GET","url":"https://api.example.invalid/pets?api_key=secret&limit=2","auth":{"type":"bearer"},"body":{"raw":"secret"}},"response":[{"code":200}]}],"event":[{"listen":"test","script":{"exec":["secret"]}}]}],"variable":[{"key":"token","value":"secret"}]}"#;
        let (table, metadata, warnings) = parse_collection(source).unwrap();
        assert!(metadata.contains("Demo"));
        assert_eq!(table.rows[0][1], "GET");
        assert!(table.rows[0][2].contains("api_key=***"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("never executed"))
        );
    }

    #[test]
    fn rejects_generic_json() {
        assert!(parse_collection("{\"info\":{},\"item\":[]}").is_err());
    }
}
