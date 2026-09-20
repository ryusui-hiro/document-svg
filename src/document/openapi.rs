//! Bounded OpenAPI/Swagger description previews.
//!
//! OpenAPI documents are interface descriptions, not executable API clients.
//! This adapter renders the document metadata and operation inventory as an
//! inert table. Servers, external documentation, examples, and `$ref` values
//! are displayed as text; no URL, reference, callback, or security scheme is
//! fetched or executed.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_OPENAPI_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OPENAPI_YAML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_OPENAPI_DEPTH: usize = 100;
const MAX_OPENAPI_VALUES: usize = 300_000;
const MAX_OPENAPI_ROWS: usize = 200_000;
const MAX_OPENAPI_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_OPENAPI_STRING_BYTES: usize = 2 * 1024 * 1024;
const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace", "connect",
];

pub(crate) fn looks_like_json_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    has_version_key(trimmed)
}

pub(crate) fn looks_like_yaml_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" {
            return false;
        }
        let indent = line.len() - line.trim_start().len();
        indent == 0 && (trimmed.starts_with("openapi:") || trimmed.starts_with("swagger:"))
    })
}

fn has_version_key(text: &str) -> bool {
    text.lines().take(80).any(|line| {
        let line = line.trim_start();
        line.starts_with("\"openapi\"") || line.starts_with("\"swagger\"")
    })
}

struct OpenApiPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OpenApiPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "openapi".into();
        if page.title.is_empty() {
            page.title = "OpenAPI description".into();
        }
        page.description =
            "OpenAPI metadata and operations are rendered inertly; references and servers are not resolved".into();
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
        options.max_input_bytes.min(MAX_OPENAPI_BYTES),
        "OpenAPI input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("OpenAPI input must be UTF-8: {error}")))?;
    let (table, metadata, warnings) = if text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('{')
    {
        parse_json(&text)?
    } else {
        parse_yaml(&text)?
    };
    let mut blocks = Vec::new();
    blocks.push(HtmlBlock::Heading {
        level: 1,
        text: "OpenAPI description".into(),
    });
    if !metadata.is_empty() {
        blocks.push(HtmlBlock::Paragraph { text: metadata });
    }
    blocks.push(HtmlBlock::Table(table));
    let mut page_sink = OpenApiPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_json(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() > MAX_OPENAPI_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OpenAPI input exceeds {MAX_OPENAPI_TEXT_BYTES} rendered text bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid OpenAPI JSON: {error}")))?;
    let mut count = 0usize;
    count_values(&value, 0, &mut count)?;
    summarize_value(&value)
}

fn summarize_value(value: &Value) -> Result<(TableData, String, Vec<String>)> {
    let object = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("OpenAPI root must be a mapping/object".into()))?;
    let version = string_field(object, "openapi").or_else(|| string_field(object, "swagger"));
    let Some(version) = version else {
        return Err(Error::InvalidInput(
            "OpenAPI document requires an openapi or swagger version field".into(),
        ));
    };
    let info = object.get("info").and_then(Value::as_object);
    let title = info
        .and_then(|v| string_field(v, "title"))
        .unwrap_or_default();
    let api_version = info
        .and_then(|v| string_field(v, "version"))
        .unwrap_or_default();
    let description = info
        .and_then(|v| string_field(v, "description"))
        .unwrap_or_default();
    let servers = object
        .get("servers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_object().and_then(|v| string_field(v, "url")))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let security_count = object
        .get("components")
        .and_then(Value::as_object)
        .and_then(|v| v.get("securitySchemes"))
        .and_then(Value::as_object)
        .map_or(0, |v| v.len());
    let schema_count = object
        .get("components")
        .and_then(Value::as_object)
        .and_then(|v| v.get("schemas"))
        .and_then(Value::as_object)
        .map_or(0, |v| v.len());

    let mut warnings = vec![
        "OpenAPI `$ref`, externalDocs, server URLs, callbacks, links, examples, and security schemes are displayed inertly and are never fetched or executed".into(),
    ];
    if object.contains_key("$ref") || text_contains_ref(value) {
        warnings.push("OpenAPI reference(s) were retained as text and not resolved".into());
    }
    let paths = object.get("paths").and_then(Value::as_object);
    let mut rows = Vec::new();
    if let Some(paths) = paths {
        for (path, item) in paths {
            if rows.len() >= MAX_OPENAPI_ROWS {
                return Err(Error::LimitExceeded(format!(
                    "OpenAPI preview exceeds {MAX_OPENAPI_ROWS} operations"
                )));
            }
            let Some(item) = item.as_object() else {
                continue;
            };
            for method in HTTP_METHODS {
                let Some(operation) = item.get(*method).and_then(Value::as_object) else {
                    continue;
                };
                let operation_id = string_field(operation, "operationId").unwrap_or_default();
                let summary = string_field(operation, "summary")
                    .or_else(|| string_field(operation, "description"))
                    .unwrap_or_default();
                let responses = operation
                    .get("responses")
                    .and_then(Value::as_object)
                    .map(|v| v.keys().cloned().collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                let tags = operation
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|v| {
                        v.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                let parameters = operation
                    .get("parameters")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
                    .to_string();
                let mut operation_label = operation_id;
                if !tags.is_empty() {
                    operation_label.push_str(&format!(" [tags: {tags}]"));
                }
                if parameters != "0" {
                    operation_label.push_str(&format!(" [params: {parameters}]"));
                }
                rows.push(vec![
                    format!("{} {path}", method.to_ascii_uppercase()),
                    operation_label,
                    truncate_display(&summary),
                    responses,
                ]);
            }
        }
    }
    if rows.is_empty() {
        warnings.push("OpenAPI document contains no HTTP path operations".into());
    }
    let mut metadata_parts = vec![format!("Specification: {version}")];
    if !title.is_empty() {
        metadata_parts.push(format!("Title: {title}"));
    }
    if !api_version.is_empty() {
        metadata_parts.push(format!("API version: {api_version}"));
    }
    if !description.is_empty() {
        metadata_parts.push(format!("Description: {}", truncate_display(&description)));
    }
    if !servers.is_empty() {
        metadata_parts.push(format!(
            "Servers: {}",
            servers
                .into_iter()
                .map(|s| truncate_display(&s))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    metadata_parts.push(format!(
        "Components: {schema_count} schema(s), {security_count} security scheme(s)"
    ));
    let table = TableData {
        headers: vec![
            "Endpoint".into(),
            "Operation".into(),
            "Summary".into(),
            "Responses".into(),
        ],
        rows,
        alignments: vec![TableAlign::Left; 4],
        raw_source: String::new(),
    };
    Ok((table, metadata_parts.join("\n"), warnings))
}

fn parse_yaml(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_OPENAPI_YAML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OpenAPI YAML exceeds {MAX_OPENAPI_YAML_BYTES} bytes"
        )));
    }
    // Parse with the bounded YAML event preview first. This validates aliases,
    // tags, depth, line length, and document limits without expanding aliases.
    crate::document::yaml::parse_yaml_blocks(text)?;
    let mut version = String::new();
    let mut title = String::new();
    let mut api_version = String::new();
    let mut description = String::new();
    let mut servers = Vec::new();
    let mut rows = Vec::new();
    let mut section = String::new();
    let mut current_path = String::new();
    let mut current_method = String::new();
    let mut operation_id = String::new();
    let mut summary = String::new();
    let mut tags = String::new();
    let mut responses = BTreeSet::new();
    let mut parameter_count = 0usize;
    let mut in_responses = false;
    let mut pending_server = false;
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
            flush_yaml_operation(
                &mut rows,
                &mut current_path,
                &mut current_method,
                &mut operation_id,
                &mut summary,
                &mut responses,
                &mut tags,
                &mut parameter_count,
            );
            if key == "openapi" || key == "swagger" {
                version = value;
            }
            section = key.to_owned();
            current_path.clear();
            current_method.clear();
            in_responses = false;
            continue;
        }
        if section == "info" && indent >= 2 {
            match key {
                "title" => title = value,
                "version" => api_version = value,
                "description" => description = value,
                _ => {}
            }
        } else if section == "servers" {
            if trimmed.starts_with("- url:") {
                servers.push(scalar(trimmed.trim_start_matches("- url:")));
                pending_server = false;
            } else if key == "url" {
                servers.push(value);
                pending_server = false;
            } else if trimmed.starts_with("-") && trimmed.contains("url:") {
                pending_server = true;
            } else if pending_server && !value.is_empty() {
                servers.push(value);
                pending_server = false;
            }
        } else if section == "paths" {
            if indent <= 2 && key.starts_with('/') {
                flush_yaml_operation(
                    &mut rows,
                    &mut current_path,
                    &mut current_method,
                    &mut operation_id,
                    &mut summary,
                    &mut responses,
                    &mut tags,
                    &mut parameter_count,
                );
                current_path = key.to_owned();
                in_responses = false;
            } else if indent <= 4 && HTTP_METHODS.contains(&key.to_ascii_lowercase().as_str()) {
                flush_yaml_operation(
                    &mut rows,
                    &mut current_path,
                    &mut current_method,
                    &mut operation_id,
                    &mut summary,
                    &mut responses,
                    &mut tags,
                    &mut parameter_count,
                );
                current_method = key.to_ascii_uppercase();
                in_responses = false;
            } else if current_method.is_empty() {
                continue;
            } else if key == "responses" {
                in_responses = true;
            } else if in_responses && is_response_key(key) {
                responses.insert(key.to_owned());
            } else if key == "operationId" {
                operation_id = value;
            } else if key == "summary" || key == "description" {
                if summary.is_empty() {
                    summary = value;
                }
            } else if key == "tags" {
                tags = value;
            } else if key == "parameters" && trimmed.starts_with("parameters:") {
                parameter_count = parameter_count.saturating_add(1);
            }
        }
    }
    flush_yaml_operation(
        &mut rows,
        &mut current_path,
        &mut current_method,
        &mut operation_id,
        &mut summary,
        &mut responses,
        &mut tags,
        &mut parameter_count,
    );
    if version.is_empty() {
        return Err(Error::InvalidInput(
            "OpenAPI YAML requires an openapi or swagger version field".into(),
        ));
    }
    let mut warnings = vec!["OpenAPI `$ref`, externalDocs, server URLs, callbacks, links, examples, and security schemes are displayed inertly and are never fetched or executed".into()];
    if text.contains("$ref:") {
        warnings.push("OpenAPI reference(s) were retained as text and not resolved".into());
    }
    if rows.is_empty() {
        warnings.push("OpenAPI document contains no HTTP path operations".into());
    }
    let metadata = format!(
        "Specification: {version}{}{}{}{}",
        if title.is_empty() {
            String::new()
        } else {
            format!("\nTitle: {title}")
        },
        if api_version.is_empty() {
            String::new()
        } else {
            format!("\nAPI version: {api_version}")
        },
        if description.is_empty() {
            String::new()
        } else {
            format!("\nDescription: {}", truncate_display(&description))
        },
        if servers.is_empty() {
            String::new()
        } else {
            format!(
                "\nServers: {}",
                servers
                    .into_iter()
                    .map(|s| truncate_display(&s))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    );
    Ok((
        TableData {
            headers: vec![
                "Endpoint".into(),
                "Operation".into(),
                "Summary".into(),
                "Responses".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

#[allow(clippy::ptr_arg, clippy::too_many_arguments)]
fn flush_yaml_operation(
    rows: &mut Vec<Vec<String>>,
    path: &mut String,
    method: &mut String,
    operation_id: &mut String,
    summary: &mut String,
    responses: &mut BTreeSet<String>,
    tags: &mut String,
    parameter_count: &mut usize,
) {
    if path.is_empty() || method.is_empty() {
        return;
    }
    if rows.len() < MAX_OPENAPI_ROWS {
        rows.push(vec![
            format!("{method} {path}"),
            {
                let mut label = operation_id.clone();
                if !tags.is_empty() {
                    label.push_str(&format!(" [tags: {tags}]"));
                }
                if *parameter_count > 0 {
                    label.push_str(&format!(" [params: {}]", *parameter_count));
                }
                label
            },
            truncate_display(summary),
            responses.iter().cloned().collect::<Vec<_>>().join(", "),
        ]);
    }
    method.clear();
    operation_id.clear();
    summary.clear();
    responses.clear();
    tags.clear();
    *parameter_count = 0;
}

fn scalar(value: &str) -> String {
    let value = value.trim();
    let value = value.split_once(" #").map_or(value, |(v, _)| v).trim();
    value.trim_matches(['"', '\'']).to_owned()
}

fn is_response_key(key: &str) -> bool {
    key == "default"
        || (key.len() == 3 && key.bytes().all(|byte| byte.is_ascii_digit()))
        || (key.len() == 3 && key.ends_with("XX") && key.as_bytes()[0].is_ascii_digit())
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn truncate_display(value: &str) -> String {
    if value.len() <= MAX_OPENAPI_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_OPENAPI_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_OPENAPI_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "OpenAPI nesting exceeds {MAX_OPENAPI_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_OPENAPI_VALUES {
        return Err(Error::LimitExceeded(format!(
            "OpenAPI document contains more than {MAX_OPENAPI_VALUES} values"
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
        Value::String(text) if text.len() > MAX_OPENAPI_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "OpenAPI string exceeds {MAX_OPENAPI_STRING_BYTES} bytes"
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
                if depth > MAX_OPENAPI_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "OpenAPI nesting exceeds {MAX_OPENAPI_DEPTH} levels"
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
    fn previews_json_operations_and_keeps_refs_inert() {
        let source = r#"{"openapi":"3.0.3","info":{"title":"Catalog","version":"1"},"servers":[{"url":"https://api.example.test"}],"paths":{"/pets":{"get":{"operationId":"listPets","summary":"List pets","responses":{"200":{"description":"ok"}},"parameters":[{"name":"limit"}]}}},"components":{"schemas":{"Pet":{}},"securitySchemes":{"bearer":{}}}}"#;
        let (table, metadata, warnings) = parse_json(source).unwrap();
        assert!(metadata.contains("Catalog"));
        assert_eq!(table.rows[0][0], "GET /pets");
        assert!(table.rows[0][1].contains("listPets"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("never fetched"))
        );
    }

    #[test]
    fn previews_yaml_operations() {
        let source = "openapi: 3.1.0\ninfo:\n  title: Catalog\n  version: '2'\npaths:\n  /pets:\n    get:\n      operationId: listPets\n      summary: List pets\n      responses:\n        '200':\n          description: ok\n";
        let (table, metadata, _) = parse_yaml(source).unwrap();
        assert!(metadata.contains("Catalog"));
        assert_eq!(table.rows[0][0], "GET /pets");
        assert!(table.rows[0][1].contains("listPets"));
        assert!(table.rows[0][3].contains("200"));
    }

    #[test]
    fn rejects_non_openapi_json() {
        assert!(parse_json("{\"title\":\"ordinary\"}").is_err());
    }
}
