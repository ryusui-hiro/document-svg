//! Bounded Apache Avro JSON schema/protocol previews.
//!
//! Avro schemas describe records, enums, arrays, maps, unions and fixed values;
//! protocols additionally describe typed messages. This adapter renders names,
//! field paths and type shapes as inert metadata. Defaults, documentation,
//! aliases, logical values, imports and protocol execution are never evaluated.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_AVRO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_AVRO_DEPTH: usize = 100;
const MAX_AVRO_VALUES: usize = 300_000;
const MAX_AVRO_ROWS: usize = 200_000;
const MAX_AVRO_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_AVRO_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return looks_like_value(&value);
    }
    text.contains("\"protocol\"") || (text.contains("\"fields\"") && text.contains("\"type\""))
}

fn looks_like_value(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            (object.get("type").and_then(Value::as_str) == Some("record")
                && object.get("fields").and_then(Value::as_array).is_some())
                || (object.get("protocol").and_then(Value::as_str).is_some()
                    && object.get("messages").and_then(Value::as_object).is_some())
                || object
                    .get("types")
                    .and_then(Value::as_array)
                    .is_some_and(|types| types.iter().any(looks_like_value))
        }
        Value::Array(values) => values.iter().any(looks_like_value),
        _ => false,
    }
}

struct AvroPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for AvroPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "avro".into();
        if page.title.is_empty() {
            page.title = "Apache Avro schema".into();
        }
        page.description =
            "Avro schema and protocol metadata is rendered inertly; defaults, documentation, imports and message execution are not evaluated".into();
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
        options.max_input_bytes.min(MAX_AVRO_BYTES),
        "Avro JSON schema input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("Avro JSON must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Apache Avro schema".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = AvroPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_AVRO_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Avro JSON exceeds {MAX_AVRO_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid Avro JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut named = 0usize;
    let mut fields = 0usize;
    let mut messages = 0usize;
    let metadata = match &value {
        Value::Object(object) if object.get("protocol").is_some() => {
            let protocol = required_string(object, "protocol")?;
            let namespace = object
                .get("namespace")
                .and_then(Value::as_str)
                .unwrap_or("—");
            let types = object
                .get("types")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if let Some(types) = object.get("types") {
                let types = types.as_array().ok_or_else(|| {
                    Error::InvalidInput("Avro protocol types must be an array".into())
                })?;
                for schema in types {
                    walk_schema(schema, "/types", 0, &mut rows, &mut named, &mut fields)?;
                }
            }
            if let Some(message_map) = object.get("messages") {
                let message_map = message_map.as_object().ok_or_else(|| {
                    Error::InvalidInput("Avro protocol messages must be an object".into())
                })?;
                for (name, message) in message_map {
                    messages = messages.saturating_add(1);
                    let message = message.as_object().ok_or_else(|| {
                        Error::InvalidInput(format!("Avro message {name} must be an object"))
                    })?;
                    let request = message
                        .get("request")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    let response = type_label(message.get("response"));
                    let errors = message
                        .get("errors")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    push_row(
                        &mut rows,
                        vec![
                            format!("/messages/{name}"),
                            "message".into(),
                            format!("request {request}; response {response}; errors {errors}"),
                        ],
                    )?;
                }
            }
            let metadata = format!(
                "Protocol: {}\nNamespace: {}\nNamed schemas: {}\nFields: {}\nMessages: {}\nDeclared types: {}",
                truncate(protocol),
                truncate(namespace),
                named,
                fields,
                messages,
                types
            );
            metadata
        }
        Value::Object(object) => {
            walk_schema(&value, "/", 0, &mut rows, &mut named, &mut fields)?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("(anonymous)");
            format!(
                "Root schema: {}\nNamed schemas: {}\nFields: {}",
                truncate(name),
                named,
                fields
            )
        }
        Value::Array(values) => {
            for (index, schema) in values.iter().enumerate() {
                walk_schema(
                    schema,
                    &format!("/{index}"),
                    0,
                    &mut rows,
                    &mut named,
                    &mut fields,
                )?;
            }
            format!(
                "Root union members: {}\nNamed schemas: {}\nFields: {}",
                values.len(),
                named,
                fields
            )
        }
        _ => {
            return Err(Error::InvalidInput(
                "Avro JSON root must be a schema object, protocol or union array".into(),
            ));
        }
    };
    if rows.is_empty() {
        rows.push(vec!["/".into(), "schema".into(), "(no fields)".into()]);
    }
    warnings.push("Avro defaults, docs, aliases, logical-type semantics, field values, imports and protocol messages remain inert; no code generation, deserialization, RPC or external resource access occurs".into());
    warnings.push("Avro JSON is bounded by input size, nesting depth, value count, schema-row count and string length; named references are shown as type text and are not resolved".into());
    Ok((
        TableData {
            headers: vec!["Path".into(), "Kind".into(), "Type / detail".into()],
            rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn walk_schema(
    value: &Value,
    path: &str,
    depth: usize,
    rows: &mut Vec<Vec<String>>,
    named: &mut usize,
    fields: &mut usize,
) -> Result<()> {
    if depth > MAX_AVRO_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "Avro schema nesting exceeds {MAX_AVRO_DEPTH} levels"
        )));
    }
    match value {
        Value::String(type_name) => {
            push_row(
                rows,
                vec![truncate(path), "reference".into(), truncate(type_name)],
            )?;
        }
        Value::Array(members) => {
            push_row(
                rows,
                vec![
                    truncate(path),
                    "union".into(),
                    format!("{} members", members.len()),
                ],
            )?;
            for (index, member) in members.iter().enumerate() {
                walk_schema(
                    member,
                    &format!("{path}/{index}"),
                    depth + 1,
                    rows,
                    named,
                    fields,
                )?;
            }
        }
        Value::Object(object) => {
            let type_value = object
                .get("type")
                .ok_or_else(|| Error::InvalidInput(format!("Avro schema {path} requires type")))?;
            let kind = type_label(Some(type_value));
            match type_value {
                Value::Object(_) | Value::Array(_) => {
                    walk_schema(
                        type_value,
                        &format!("{path}/type"),
                        depth + 1,
                        rows,
                        named,
                        fields,
                    )?;
                }
                _ => {}
            }
            match kind.as_str() {
                "record" | "error" | "request" => {
                    let name = required_string(object, "name")?;
                    *named = named.saturating_add(1);
                    push_row(
                        rows,
                        vec![truncate(path), kind.clone(), format!("name {name}")],
                    )?;
                    let field_values =
                        object
                            .get("fields")
                            .and_then(Value::as_array)
                            .ok_or_else(|| {
                                Error::InvalidInput(format!(
                                    "Avro {kind} {name} requires fields array"
                                ))
                            })?;
                    for field in field_values {
                        let field_object = field.as_object().ok_or_else(|| {
                            Error::InvalidInput(format!("Avro field in {name} must be an object"))
                        })?;
                        let field_name = required_string(field_object, "name")?;
                        let field_type = field_object.get("type").ok_or_else(|| {
                            Error::InvalidInput(format!("Avro field {field_name} requires type"))
                        })?;
                        *fields = fields.saturating_add(1);
                        let field_path =
                            format!("{}/fields/{field_name}", path.trim_end_matches('/'));
                        push_row(
                            rows,
                            vec![
                                truncate(&field_path),
                                "field".into(),
                                type_label(Some(field_type)),
                            ],
                        )?;
                        walk_schema(
                            field_type,
                            &format!("{field_path}/type"),
                            depth + 1,
                            rows,
                            named,
                            fields,
                        )?;
                    }
                }
                "enum" => {
                    let name = required_string(object, "name")?;
                    let symbols =
                        object
                            .get("symbols")
                            .and_then(Value::as_array)
                            .ok_or_else(|| {
                                Error::InvalidInput(format!(
                                    "Avro enum {name} requires symbols array"
                                ))
                            })?;
                    *named = named.saturating_add(1);
                    push_row(
                        rows,
                        vec![
                            truncate(path),
                            "enum".into(),
                            format!("name {name}; {} symbols", symbols.len()),
                        ],
                    )?;
                }
                "fixed" => {
                    let name = required_string(object, "name")?;
                    let size = object.get("size").and_then(Value::as_u64).ok_or_else(|| {
                        Error::InvalidInput(format!("Avro fixed {name} requires unsigned size"))
                    })?;
                    *named = named.saturating_add(1);
                    push_row(
                        rows,
                        vec![
                            truncate(path),
                            "fixed".into(),
                            format!("name {name}; {size} bytes"),
                        ],
                    )?;
                }
                "array" => {
                    let items = object.get("items").ok_or_else(|| {
                        Error::InvalidInput(format!("Avro array {path} requires items"))
                    })?;
                    push_row(
                        rows,
                        vec![truncate(path), "array".into(), type_label(Some(items))],
                    )?;
                    walk_schema(
                        items,
                        &format!("{path}/items"),
                        depth + 1,
                        rows,
                        named,
                        fields,
                    )?;
                }
                "map" => {
                    let values = object.get("values").ok_or_else(|| {
                        Error::InvalidInput(format!("Avro map {path} requires values"))
                    })?;
                    push_row(
                        rows,
                        vec![truncate(path), "map".into(), type_label(Some(values))],
                    )?;
                    walk_schema(
                        values,
                        &format!("{path}/values"),
                        depth + 1,
                        rows,
                        named,
                        fields,
                    )?;
                }
                _ => {
                    push_row(rows, vec![truncate(path), "schema".into(), kind])?;
                }
            }
        }
        _ => {
            return Err(Error::InvalidInput(format!(
                "Avro schema {path} must be a string, object or union array"
            )));
        }
    }
    Ok(())
}

fn push_row(rows: &mut Vec<Vec<String>>, row: Vec<String>) -> Result<()> {
    if rows.len() >= MAX_AVRO_ROWS {
        return Err(Error::LimitExceeded(format!(
            "Avro schema rows exceed {MAX_AVRO_ROWS}"
        )));
    }
    rows.push(row);
    Ok(())
}

fn required_string<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Result<&'a str> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput(format!("Avro schema requires string {key}")))?;
    if value.is_empty() {
        return Err(Error::InvalidInput(format!(
            "Avro schema {key} must not be empty"
        )));
    }
    Ok(value)
}

fn type_label(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => truncate(value),
        Some(Value::Array(values)) => format!("union({})", values.len()),
        Some(Value::Object(object)) => object
            .get("type")
            .and_then(Value::as_str)
            .map_or_else(|| "object".into(), truncate),
        Some(_) => "invalid".into(),
        None => "—".into(),
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_AVRO_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_AVRO_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_AVRO_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "Avro JSON nesting exceeds {MAX_AVRO_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_AVRO_VALUES {
        return Err(Error::LimitExceeded(format!(
            "Avro JSON contains more than {MAX_AVRO_VALUES} values"
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
        Value::String(value) if value.len() > MAX_AVRO_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "Avro JSON string exceeds {MAX_AVRO_STRING_BYTES} bytes"
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
                if depth > MAX_AVRO_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "Avro JSON nesting exceeds {MAX_AVRO_DEPTH} levels"
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
    fn recognizes_avro_schema_and_protocol() {
        assert!(looks_like_prefix(
            br#"{"type":"record","name":"User","fields":[{"name":"id","type":"long"}]}"#
        ));
        assert!(looks_like_prefix(br#"{"protocol":"Ping","messages":{}}"#));
        assert!(!looks_like_prefix(br#"{"type":"object","properties":{}}"#));
    }

    #[test]
    fn summarizes_nested_schema_without_defaults_or_docs() {
        let (table, metadata, warnings) = parse(
            r#"{"type":"record","name":"User","namespace":"example","doc":"secret docs","fields":[{"name":"id","type":"long","default":7},{"name":"profile","type":{"type":"record","name":"Profile","fields":[{"name":"email","type":"string"}]}},{"name":"tags","type":{"type":"array","items":"string"}}]}"#,
        )
        .unwrap();
        assert!(metadata.contains("Root schema: User"));
        assert!(table.rows.iter().any(|row| row[0].contains("profile")));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("7"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("defaults")));
    }
}
