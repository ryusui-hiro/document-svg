//! Bounded JSON Schema draft previews.
//!
//! Schemas are validation descriptions, not programs. This adapter renders
//! schema paths, types, titles, required fields and common constraints as an
//! inert table. `$ref`, `$id`, `format`, patterns and examples remain text;
//! no reference, URI, regular expression, or validation engine is executed.

use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_SCHEMA_JSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SCHEMA_YAML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SCHEMA_DEPTH: usize = 100;
const MAX_SCHEMA_VALUES: usize = 300_000;
const MAX_SCHEMA_ROWS: usize = 200_000;
const MAX_SCHEMA_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCHEMA_TEXT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn looks_like_json_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    let has_schema = text.contains("\"$schema\"")
        && (text.contains("json-schema.org") || text.contains("draft-"));
    let has_schema_shape = text.contains("\"properties\"")
        || text.contains("\"$defs\"")
        || text.contains("\"definitions\"");
    has_schema || (text.contains("\"title\"") && has_schema_shape)
}

pub(crate) fn looks_like_yaml_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let schema = text.lines().any(|line| {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        indent == 0 && trimmed.starts_with("$schema:") && trimmed.contains("json-schema.org")
    });
    let shape = text.lines().any(|line| {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        indent == 0 && (trimmed.starts_with("properties:") || trimmed.starts_with("$defs:"))
    });
    schema
        || shape
            && text
                .lines()
                .any(|line| line.trim_start().starts_with("type:"))
}

struct SchemaPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SchemaPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "jsonschema".into();
        if page.title.is_empty() {
            page.title = "JSON Schema".into();
        }
        page.description =
            "JSON Schema structure is rendered inertly; references and validation semantics are not evaluated".into();
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
        options.max_input_bytes.min(MAX_SCHEMA_JSON_BYTES),
        "JSON Schema input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("JSON Schema input must be UTF-8: {error}"))
    })?;
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
            text: "JSON Schema".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = SchemaPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_json(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_SCHEMA_JSON_BYTES || text.len() > MAX_SCHEMA_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON Schema JSON exceeds {MAX_SCHEMA_JSON_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid JSON Schema JSON: {error}")))?;
    let mut count = 0usize;
    count_values(&value, 0, &mut count)?;
    summarize(&value)
}

fn summarize(value: &Value) -> Result<(TableData, String, Vec<String>)> {
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("JSON Schema root must be an object".into()))?;
    let schema_uri = root
        .get("$schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let id = root.get("$id").and_then(Value::as_str).unwrap_or_default();
    let title = root
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let description = root
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let schema_shape = root.contains_key("$schema")
        || root.contains_key("$defs")
        || root.contains_key("definitions")
        || root.contains_key("properties")
        || (root.contains_key("title") && root.contains_key("type"));
    if !schema_shape {
        return Err(Error::InvalidInput(
            "JSON document does not have a JSON Schema dialect or schema shape".into(),
        ));
    }
    let mut rows = Vec::new();
    walk_schema(value, "$", 0, &mut rows)?;
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "JSON Schema contains no renderable schema nodes".into(),
        ));
    }
    let mut warnings = vec![
        "JSON Schema `$ref`, `$id`, URI values, regular-expression patterns, examples, and format annotations are displayed inertly and are never resolved, fetched, executed, or evaluated".into(),
    ];
    if contains_key(value, "$ref") {
        warnings.push("JSON Schema reference(s) were retained as text and not resolved".into());
    }
    let mut metadata = Vec::new();
    if !schema_uri.is_empty() {
        metadata.push(format!("Dialect: {}", truncate(schema_uri)));
    }
    if !id.is_empty() {
        metadata.push(format!("ID: {}", truncate(id)));
    }
    if !title.is_empty() {
        metadata.push(format!("Title: {title}"));
    }
    if !description.is_empty() {
        metadata.push(format!("Description: {}", truncate(description)));
    }
    if metadata.is_empty() {
        metadata.push("Schema metadata: (none)".into());
    }
    Ok((table(rows), metadata.join("\n"), warnings))
}

fn walk_schema(value: &Value, path: &str, depth: usize, rows: &mut Vec<Vec<String>>) -> Result<()> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "JSON Schema nesting exceeds {MAX_SCHEMA_DEPTH} levels"
        )));
    }
    if rows.len() >= MAX_SCHEMA_ROWS {
        return Err(Error::LimitExceeded(format!(
            "JSON Schema preview exceeds {MAX_SCHEMA_ROWS} schema nodes"
        )));
    }
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let schema_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            if object.contains_key("properties") {
                Some("object".into())
            } else if object.contains_key("items") {
                Some("array".into())
            } else if object.contains_key("$ref") {
                Some("$ref".into())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "schema".into());
    let mut details = Vec::new();
    if let Some(title) = object.get("title").and_then(Value::as_str) {
        details.push(format!("title: {}", truncate(title)));
    }
    if let Some(description) = object.get("description").and_then(Value::as_str) {
        details.push(format!("description: {}", truncate(description)));
    }
    if let Some(format) = object.get("format").and_then(Value::as_str) {
        details.push(format!("format: {format}"));
    }
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        details.push(format!("$ref: {}", truncate(reference)));
    }
    let required = object
        .get("required")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let enum_count = object
        .get("enum")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if required > 0 {
        details.push(format!("required: {required}"));
    }
    if enum_count > 0 {
        details.push(format!("enum: {enum_count}"));
    }
    let constraints = [
        "const",
        "default",
        "minLength",
        "maxLength",
        "pattern",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minItems",
        "maxItems",
        "uniqueItems",
        "minProperties",
        "maxProperties",
        "additionalProperties",
    ]
    .iter()
    .filter_map(|key| {
        object
            .get(*key)
            .map(|value| format!("{key}={}", scalar_value(value)))
    })
    .collect::<Vec<_>>()
    .join(", ");
    rows.push(vec![
        truncate(path),
        schema_type,
        details.join("; "),
        truncate(&constraints),
    ]);

    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (name, child) in properties {
            walk_schema(child, &join_path(path, name), depth + 1, rows)?;
        }
    }
    if let Some(items) = object.get("items") {
        walk_schema(items, &format!("{path}[]"), depth + 1, rows)?;
    }
    for key in ["$defs", "definitions"] {
        if let Some(definitions) = object.get(key).and_then(Value::as_object) {
            for (name, child) in definitions {
                walk_schema(child, &format!("{path}#{key}/{name}"), depth + 1, rows)?;
            }
        }
    }
    for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
        if let Some(values) = object.get(key).and_then(Value::as_array) {
            for (index, child) in values.iter().enumerate() {
                walk_schema(child, &format!("{path}#{key}[{index}]"), depth + 1, rows)?;
            }
        }
    }
    Ok(())
}

fn parse_yaml(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_SCHEMA_YAML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON Schema YAML exceeds {MAX_SCHEMA_YAML_BYTES} bytes"
        )));
    }
    crate::document::yaml::parse_yaml_blocks(text)?;
    let mut schema_uri = String::new();
    let mut id = String::new();
    let mut title = String::new();
    let mut description = String::new();
    let mut root_type = String::new();
    let mut rows = Vec::new();
    let mut in_properties = false;
    let mut property_base = 0usize;
    let mut current_path = String::new();
    let mut current_type = String::new();
    let mut current_details = Vec::new();
    let mut current_constraints = Vec::new();
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
        let value = scalar_text(raw_value);
        if indent == 0 {
            flush_yaml_row(
                &mut rows,
                &mut current_path,
                &mut current_type,
                &mut current_details,
                &mut current_constraints,
            );
            if key == "properties" {
                in_properties = true;
                property_base = indent;
                continue;
            }
            match key {
                "$schema" => schema_uri = value,
                "$id" => id = value,
                "title" => title = value,
                "description" => description = value,
                "type" => root_type = value,
                _ => {}
            }
            in_properties = false;
            continue;
        }
        if key == "properties" {
            flush_yaml_row(
                &mut rows,
                &mut current_path,
                &mut current_type,
                &mut current_details,
                &mut current_constraints,
            );
            in_properties = true;
            property_base = indent;
            continue;
        }
        if in_properties && indent == property_base + 2 {
            flush_yaml_row(
                &mut rows,
                &mut current_path,
                &mut current_type,
                &mut current_details,
                &mut current_constraints,
            );
            current_path = format!("$.{}", key);
            current_type.clear();
            current_details.clear();
            current_constraints.clear();
        } else if in_properties && !current_path.is_empty() && indent > property_base + 2 {
            match key {
                "type" => current_type = value,
                "title" | "description" | "format" => {
                    current_details.push(format!("{key}: {}", truncate(&value)))
                }
                _ => current_constraints.push(format!("{key}={}", truncate(&value))),
            }
        } else if current_path.is_empty() && indent == 2 {
            match key {
                "type" => root_type = value,
                "title" => title = value,
                "description" => description = value,
                _ => {}
            }
        }
    }
    flush_yaml_row(
        &mut rows,
        &mut current_path,
        &mut current_type,
        &mut current_details,
        &mut current_constraints,
    );
    if rows.is_empty() {
        rows.push(vec![
            "$".into(),
            if root_type.is_empty() {
                "schema".into()
            } else {
                root_type
            },
            format!("title: {}", truncate(&title)),
            String::new(),
        ]);
    } else {
        rows.insert(
            0,
            vec![
                "$".into(),
                if root_type.is_empty() {
                    "schema".into()
                } else {
                    root_type
                },
                format!("title: {}", truncate(&title)),
                String::new(),
            ],
        );
    }
    let mut warnings = vec!["JSON Schema `$ref`, `$id`, URI values, regular-expression patterns, examples, and format annotations are displayed inertly and are never resolved, fetched, executed, or evaluated".into()];
    if text.contains("$ref:") {
        warnings.push("JSON Schema reference(s) were retained as text and not resolved".into());
    }
    let mut metadata = Vec::new();
    if !schema_uri.is_empty() {
        metadata.push(format!("Dialect: {}", truncate(&schema_uri)));
    }
    if !id.is_empty() {
        metadata.push(format!("ID: {}", truncate(&id)));
    }
    if !title.is_empty() {
        metadata.push(format!("Title: {title}"));
    }
    if !description.is_empty() {
        metadata.push(format!("Description: {}", truncate(&description)));
    }
    if metadata.is_empty() {
        metadata.push("Schema metadata: (none)".into());
    }
    Ok((table(rows), metadata.join("\n"), warnings))
}

fn flush_yaml_row(
    rows: &mut Vec<Vec<String>>,
    path: &mut String,
    schema_type: &mut String,
    details: &mut Vec<String>,
    constraints: &mut Vec<String>,
) {
    if path.is_empty() {
        return;
    }
    if rows.len() < MAX_SCHEMA_ROWS {
        rows.push(vec![
            truncate(path),
            if schema_type.is_empty() {
                "schema".into()
            } else {
                schema_type.clone()
            },
            details.join("; "),
            constraints.join(", "),
        ]);
    }
    path.clear();
    schema_type.clear();
    details.clear();
    constraints.clear();
}

fn table(rows: Vec<Vec<String>>) -> TableData {
    let rows = rows
        .into_iter()
        .filter_map(|row| {
            if row.len() < 4 {
                return None;
            }
            let mut detail = format!("type: {}", row[1]);
            if !row[2].is_empty() {
                detail.push_str("; ");
                detail.push_str(&row[2]);
            }
            if !row[3].is_empty() {
                detail.push_str("; ");
                detail.push_str(&row[3]);
            }
            Some(vec![row[0].clone(), truncate_cell(&detail)])
        })
        .collect();
    TableData {
        headers: vec!["Path".into(), "Details".into()],
        rows,
        alignments: vec![TableAlign::Left; 2],
        raw_source: String::new(),
    }
}

fn truncate_cell(value: &str) -> String {
    const MAX_CELL_CHARS: usize = 80;
    if value.chars().count() <= MAX_CELL_CHARS {
        return value.to_owned();
    }
    let end = value
        .char_indices()
        .nth(MAX_CELL_CHARS)
        .map_or(value.len(), |(index, _)| index);
    format!("{}…", &value[..end])
}

fn scalar_value(value: &Value) -> String {
    match value {
        Value::String(value) => truncate(value),
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => format!("[{} item(s)]", values.len()),
        Value::Object(object) => format!("{{{} key(s)}}", object.len()),
    }
}

fn scalar_text(value: &str) -> String {
    value
        .trim()
        .split_once(" #")
        .map_or(value.trim(), |(v, _)| v.trim())
        .trim_matches(['"', '\''])
        .to_owned()
}

fn join_path(parent: &str, key: &str) -> String {
    let simple = !key.is_empty()
        && key.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character == '$'
                || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
        });
    if simple {
        format!("{parent}.{key}")
    } else {
        format!(
            "{parent}[{}]",
            serde_json::to_string(key).unwrap_or_else(|_| "\"?\"".into())
        )
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_SCHEMA_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_SCHEMA_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn contains_key(value: &Value, target: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key(target) || map.values().any(|v| contains_key(v, target))
        }
        Value::Array(values) => values.iter().any(|v| contains_key(v, target)),
        _ => false,
    }
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "JSON Schema nesting exceeds {MAX_SCHEMA_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_SCHEMA_VALUES {
        return Err(Error::LimitExceeded(format!(
            "JSON Schema document contains more than {MAX_SCHEMA_VALUES} values"
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
        Value::String(text) if text.len() > MAX_SCHEMA_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "JSON Schema string exceeds {MAX_SCHEMA_STRING_BYTES} bytes"
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
                if depth > MAX_SCHEMA_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "JSON Schema nesting exceeds {MAX_SCHEMA_DEPTH} levels"
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
    fn previews_json_schema_properties_and_constraints() {
        let source = r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","$id":"https://example.invalid/catalog","title":"Catalog","type":"object","properties":{"name":{"type":"string","minLength":1},"count":{"type":"integer","minimum":0}},"required":["name"],"$defs":{"Tag":{"type":"string"}},"items":{"$ref":"#/ $defs/Tag"}}"##;
        let (table, metadata, warnings) = parse_json(source).unwrap();
        assert!(metadata.contains("Catalog"));
        assert!(table.rows.iter().any(|row| row[0].starts_with("$.name")));
        assert!(table.rows.iter().any(|row| row[1].contains("minLength")));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not resolved"))
        );
    }

    #[test]
    fn previews_yaml_schema_properties() {
        let source = "$schema: https://json-schema.org/draft/2020-12/schema\ntitle: Catalog YAML\ntype: object\nproperties:\n  name:\n    type: string\n    minLength: 1\n";
        let (table, metadata, _) = parse_yaml(source).unwrap();
        assert!(metadata.contains("Catalog YAML"));
        assert!(table.rows.iter().any(|row| row[0].starts_with("$.name")));
    }

    #[test]
    fn rejects_non_schema_json_without_schema_shape() {
        assert!(parse_json("{\"foo\":1}").is_err());
    }
}
