//! Bounded AsyncAPI message-driven API previews.
//!
//! AsyncAPI documents describe channels and operations; they are never API
//! clients. This adapter renders the protocol metadata and channel inventory
//! as inert table rows. Server URLs, `$ref` values, examples, bindings and
//! security information remain text and are not fetched or executed.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_ASYNCAPI_JSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ASYNCAPI_YAML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ASYNCAPI_DEPTH: usize = 100;
const MAX_ASYNCAPI_VALUES: usize = 300_000;
const MAX_ASYNCAPI_ROWS: usize = 200_000;
const MAX_ASYNCAPI_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_ASYNCAPI_TEXT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn looks_like_json_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.lines().take(80).any(|line| {
            let line = line.trim_start();
            line.starts_with("\"asyncapi\"")
        })
}

pub(crate) fn looks_like_yaml_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines().any(|line| {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        indent == 0 && trimmed.starts_with("asyncapi:")
    })
}

struct AsyncApiPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for AsyncApiPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "asyncapi".into();
        if page.title.is_empty() {
            page.title = "AsyncAPI description".into();
        }
        page.description =
            "AsyncAPI channels and operations are rendered inertly; servers and references are not resolved".into();
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
        options.max_input_bytes.min(MAX_ASYNCAPI_JSON_BYTES),
        "AsyncAPI input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("AsyncAPI input must be UTF-8: {error}")))?;
    let (table, metadata, warnings) = if text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('{')
    {
        parse_json(&text)?
    } else {
        parse_yaml(&text)?
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "AsyncAPI description".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = AsyncApiPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_json(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_ASYNCAPI_JSON_BYTES || text.len() > MAX_ASYNCAPI_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "AsyncAPI JSON exceeds {MAX_ASYNCAPI_JSON_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid AsyncAPI JSON: {error}")))?;
    let mut count = 0usize;
    count_values(&value, 0, &mut count)?;
    summarize_value(&value)
}

fn summarize_value(value: &Value) -> Result<(TableData, String, Vec<String>)> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("AsyncAPI root must be a mapping/object".into()))?;
    let version = object
        .get("asyncapi")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::InvalidInput("AsyncAPI document requires an asyncapi version field".into())
        })?;
    let info = object.get("info").and_then(Value::as_object);
    let title = info.and_then(|v| field(v, "title")).unwrap_or_default();
    let api_version = info.and_then(|v| field(v, "version")).unwrap_or_default();
    let description = info
        .and_then(|v| field(v, "description"))
        .unwrap_or_default();
    let servers = object
        .get("servers")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .values()
                .filter_map(|item| item.as_object().and_then(|v| field(v, "url")))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let components = object.get("components").and_then(Value::as_object);
    let message_count = components
        .and_then(|v| v.get("messages"))
        .and_then(Value::as_object)
        .map_or(0, |v| v.len());
    let schema_count = components
        .and_then(|v| v.get("schemas"))
        .and_then(Value::as_object)
        .map_or(0, |v| v.len());
    let mut warnings = vec![
        "AsyncAPI server URLs, `$ref`, externalDocs, examples, protocol bindings, and security schemes are displayed inertly and are never fetched or executed".into(),
    ];
    if text_contains_ref(value) {
        warnings.push("AsyncAPI reference(s) were retained as text and not resolved".into());
    }
    let mut rows = Vec::new();
    if let Some(channels) = object.get("channels").and_then(Value::as_object) {
        for (channel, item) in channels {
            let Some(item) = item.as_object() else {
                continue;
            };
            for action in ["publish", "subscribe"] {
                let Some(operation) = item.get(action).and_then(Value::as_object) else {
                    continue;
                };
                rows.push(operation_row(
                    channel,
                    action,
                    operation,
                    item.get("messages"),
                ));
                if rows.len() >= MAX_ASYNCAPI_ROWS {
                    return Err(Error::LimitExceeded(format!(
                        "AsyncAPI preview exceeds {MAX_ASYNCAPI_ROWS} operations"
                    )));
                }
            }
        }
    }
    // AsyncAPI 3.x promotes operations to a top-level map with send/receive
    // actions and a channel reference/object.
    if let Some(operations) = object.get("operations").and_then(Value::as_object) {
        for (name, operation) in operations {
            let Some(operation) = operation.as_object() else {
                continue;
            };
            let action = field(operation, "action").unwrap_or_else(|| "operation".into());
            let channel = operation
                .get("channel")
                .and_then(|v| v.as_object())
                .and_then(|v| field(v, "$ref").or_else(|| field(v, "address")))
                .or_else(|| {
                    operation
                        .get("channel")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| "(channel reference)".into());
            rows.push(operation_row(&channel, &action, operation, None));
            if rows.len() >= MAX_ASYNCAPI_ROWS {
                return Err(Error::LimitExceeded(format!(
                    "AsyncAPI preview exceeds {MAX_ASYNCAPI_ROWS} operations"
                )));
            }
            if name.is_empty() { /* operation name is retained in the label below */ }
            if let Some(last) = rows.last_mut() {
                if last[1].is_empty() {
                    last[1] = name.clone();
                } else {
                    last[1] = format!("{} ({name})", last[1]);
                }
            }
        }
    }
    if rows.is_empty() {
        warnings.push(
            "AsyncAPI document contains no publish/subscribe or send/receive operations".into(),
        );
    }
    let mut metadata = vec![format!("Specification: {version}")];
    if !title.is_empty() {
        metadata.push(format!("Title: {title}"));
    }
    if !api_version.is_empty() {
        metadata.push(format!("API version: {api_version}"));
    }
    if !description.is_empty() {
        metadata.push(format!("Description: {}", truncate(&description)));
    }
    if !servers.is_empty() {
        metadata.push(format!(
            "Servers: {}",
            servers
                .into_iter()
                .map(|s| truncate(&s))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    metadata.push(format!(
        "Components: {message_count} message(s), {schema_count} schema(s)"
    ));
    Ok((table(rows), metadata.join("\n"), warnings))
}

fn operation_row(
    channel: &str,
    action: &str,
    operation: &serde_json::Map<String, Value>,
    channel_messages: Option<&Value>,
) -> Vec<String> {
    let name = field(operation, "operationId").unwrap_or_default();
    let summary = field(operation, "summary")
        .or_else(|| field(operation, "description"))
        .unwrap_or_default();
    let message_count = operation.get("message").map_or(0, |_| 1)
        + operation
            .get("messages")
            .and_then(Value::as_object)
            .map_or(0, |v| v.len())
        + channel_messages
            .and_then(Value::as_object)
            .map_or(0, |v| v.len());
    let message = if message_count == 0 {
        String::new()
    } else {
        message_count.to_string()
    };
    vec![
        truncate(channel),
        format!(
            "{}{}",
            action.to_ascii_uppercase(),
            if name.is_empty() {
                String::new()
            } else {
                format!(" / {name}")
            }
        ),
        truncate(&summary),
        message,
    ]
}

fn table(rows: Vec<Vec<String>>) -> TableData {
    TableData {
        headers: vec![
            "Channel".into(),
            "Action / Operation".into(),
            "Summary".into(),
            "Msgs".into(),
        ],
        rows,
        alignments: vec![TableAlign::Left; 4],
        raw_source: String::new(),
    }
}

fn parse_yaml(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_ASYNCAPI_YAML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "AsyncAPI YAML exceeds {MAX_ASYNCAPI_YAML_BYTES} bytes"
        )));
    }
    crate::document::yaml::parse_yaml_blocks(text)?;
    let mut version = String::new();
    let mut title = String::new();
    let mut api_version = String::new();
    let mut description = String::new();
    let mut servers = Vec::new();
    let mut rows = Vec::new();
    let mut section = String::new();
    let mut channel = String::new();
    let mut action = String::new();
    let mut operation = String::new();
    let mut summary = String::new();
    let mut messages = BTreeSet::new();
    let mut in_messages = false;
    let mut in_channel = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "..." {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = raw_key.trim().trim_matches(['"', '\'']);
        let value = scalar(raw_value);
        if indent == 0 {
            flush_yaml(
                &mut rows,
                &mut channel,
                &mut action,
                &mut operation,
                &mut summary,
                &mut messages,
            );
            if key == "asyncapi" {
                version = value;
            }
            section = key.to_owned();
            in_messages = false;
            in_channel = false;
            continue;
        }
        if section == "info" {
            match key {
                "title" => title = value,
                "version" => api_version = value,
                "description" => description = value,
                _ => {}
            }
        } else if section == "servers" {
            if key == "url" {
                servers.push(value);
            }
        } else if section == "channels" {
            if indent <= 2 {
                flush_yaml(
                    &mut rows,
                    &mut channel,
                    &mut action,
                    &mut operation,
                    &mut summary,
                    &mut messages,
                );
                channel = key.to_owned();
                in_channel = true;
                in_messages = false;
            } else if in_channel && indent <= 4 && (key == "publish" || key == "subscribe") {
                flush_yaml(
                    &mut rows,
                    &mut channel,
                    &mut action,
                    &mut operation,
                    &mut summary,
                    &mut messages,
                );
                action = key.to_ascii_uppercase();
                in_messages = false;
            } else if !action.is_empty() {
                if key == "operationId" {
                    operation = value;
                } else if key == "summary" || key == "description" {
                    if summary.is_empty() {
                        summary = value;
                    }
                } else if key == "messages" || key == "message" {
                    in_messages = true;
                    if key == "message" {
                        messages.insert(value);
                    }
                } else if in_messages && indent >= 6 {
                    messages.insert(key.to_owned());
                }
            }
        } else if section == "operations" {
            if indent <= 2 {
                flush_yaml(
                    &mut rows,
                    &mut channel,
                    &mut action,
                    &mut operation,
                    &mut summary,
                    &mut messages,
                );
                operation = key.to_owned();
            } else if key == "action" {
                action = value.to_ascii_uppercase();
            } else if key == "summary" || key == "description" {
                summary = value;
            } else if key == "$ref" || key == "address" {
                channel = value;
            }
        }
    }
    flush_yaml(
        &mut rows,
        &mut channel,
        &mut action,
        &mut operation,
        &mut summary,
        &mut messages,
    );
    if version.is_empty() {
        return Err(Error::InvalidInput(
            "AsyncAPI YAML requires an asyncapi version field".into(),
        ));
    }
    let mut warnings = vec!["AsyncAPI server URLs, `$ref`, externalDocs, examples, protocol bindings, and security schemes are displayed inertly and are never fetched or executed".into()];
    if text.contains("$ref:") {
        warnings.push("AsyncAPI reference(s) were retained as text and not resolved".into());
    }
    if rows.is_empty() {
        warnings.push(
            "AsyncAPI document contains no publish/subscribe or send/receive operations".into(),
        );
    }
    let mut metadata = vec![format!("Specification: {version}")];
    if !title.is_empty() {
        metadata.push(format!("Title: {title}"));
    }
    if !api_version.is_empty() {
        metadata.push(format!("API version: {api_version}"));
    }
    if !description.is_empty() {
        metadata.push(format!("Description: {}", truncate(&description)));
    }
    if !servers.is_empty() {
        metadata.push(format!(
            "Servers: {}",
            servers
                .into_iter()
                .map(|s| truncate(&s))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok((table(rows), metadata.join("\n"), warnings))
}

#[allow(clippy::ptr_arg, clippy::too_many_arguments)]
fn flush_yaml(
    rows: &mut Vec<Vec<String>>,
    channel: &mut String,
    action: &mut String,
    operation: &mut String,
    summary: &mut String,
    messages: &mut BTreeSet<String>,
) {
    if channel.is_empty() || action.is_empty() {
        return;
    }
    if rows.len() < MAX_ASYNCAPI_ROWS {
        rows.push(vec![
            truncate(channel),
            format!(
                "{}{}",
                action,
                if operation.is_empty() {
                    String::new()
                } else {
                    format!(" / {operation}")
                }
            ),
            truncate(summary),
            if messages.is_empty() {
                String::new()
            } else {
                messages.len().to_string()
            },
        ]);
    }
    action.clear();
    operation.clear();
    summary.clear();
    messages.clear();
}

fn scalar(value: &str) -> String {
    value
        .trim()
        .split_once(" #")
        .map_or(value.trim(), |(v, _)| v.trim())
        .trim_matches(['"', '\''])
        .to_owned()
}

fn field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_ASYNCAPI_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_ASYNCAPI_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_ASYNCAPI_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "AsyncAPI nesting exceeds {MAX_ASYNCAPI_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_ASYNCAPI_VALUES {
        return Err(Error::LimitExceeded(format!(
            "AsyncAPI document contains more than {MAX_ASYNCAPI_VALUES} values"
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
        Value::String(text) if text.len() > MAX_ASYNCAPI_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "AsyncAPI string exceeds {MAX_ASYNCAPI_STRING_BYTES} bytes"
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
                if depth > MAX_ASYNCAPI_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "AsyncAPI nesting exceeds {MAX_ASYNCAPI_DEPTH} levels"
                    )));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

fn text_contains_ref(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.keys().any(|key| key == "$ref") || map.values().any(text_contains_ref)
        }
        Value::Array(values) => values.iter().any(text_contains_ref),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_asyncapi_json_channels_and_operations() {
        let source = r##"{"asyncapi":"2.6.0","info":{"title":"Events","version":"1"},"servers":{"kafka":{"url":"kafka://broker.example.invalid"}},"channels":{"user/signedup":{"publish":{"operationId":"onUser","summary":"User signed up","message":{"$ref":"#/components/messages/User"}}}} ,"components":{"messages":{"User":{}},"schemas":{"User":{}}}}"##;
        let (table, metadata, warnings) = parse_json(source).unwrap();
        assert!(metadata.contains("Events"));
        assert_eq!(table.rows[0][0], "user/signedup");
        assert!(table.rows[0][1].contains("onUser"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not resolved"))
        );
    }

    #[test]
    fn previews_asyncapi_yaml_channels() {
        let source = "asyncapi: 3.0.0\ninfo:\n  title: Events YAML\n  version: '1'\nchannels:\n  user/signedup:\n    address: user.signedup\n    messages:\n      User: {}\n    publish:\n      operationId: onUser\n      summary: User signed up\n";
        let (table, metadata, _) = parse_yaml(source).unwrap();
        assert!(metadata.contains("Events YAML"));
        assert_eq!(table.rows[0][0], "user/signedup");
        assert!(table.rows[0][1].contains("onUser"));
    }

    #[test]
    fn rejects_non_asyncapi_json() {
        assert!(parse_json("{\"openapi\":\"3.0.0\"}").is_err());
    }
}
