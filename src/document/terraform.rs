//! Bounded Terraform JSON plan previews.
//!
//! `terraform show -json` plan output can include full configuration, state,
//! planned values and sensitive attributes. This adapter renders only resource
//! addresses, types, action sets and safe reasons; no values, providers,
//! expressions, state, plan/apply/refresh operation, or network access is used.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_TERRAFORM_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TERRAFORM_DEPTH: usize = 100;
const MAX_TERRAFORM_VALUES: usize = 300_000;
const MAX_TERRAFORM_RESOURCES: usize = 300_000;
const MAX_TERRAFORM_STRING_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"format_version\"")
        && (text.contains("\"resource_changes\"") || text.contains("\"planned_values\""))
}

struct TerraformPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for TerraformPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "terraform-plan".into();
        if page.title.is_empty() {
            page.title = "Terraform plan".into();
        }
        page.description =
            "Terraform JSON plan resource actions are rendered as inert metadata; values and provider operations are omitted".into();
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
        options.max_input_bytes.min(MAX_TERRAFORM_BYTES),
        "Terraform JSON plan input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Terraform plan must be UTF-8 JSON: {error}"))
    })?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Terraform plan".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = TerraformPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct ActionCounts {
    create: usize,
    update: usize,
    delete: usize,
    read: usize,
    noop: usize,
    other: usize,
}

impl ActionCounts {
    fn add(&mut self, actions: &[String]) {
        if actions.is_empty() {
            self.other = self.other.saturating_add(1);
            return;
        }
        for action in actions {
            match action.as_str() {
                "create" => self.create = self.create.saturating_add(1),
                "update" => self.update = self.update.saturating_add(1),
                "delete" => self.delete = self.delete.saturating_add(1),
                "read" => self.read = self.read.saturating_add(1),
                "no-op" => self.noop = self.noop.saturating_add(1),
                _ => self.other = self.other.saturating_add(1),
            }
        }
    }
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_TERRAFORM_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Terraform plan exceeds {MAX_TERRAFORM_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid Terraform plan JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("Terraform plan root must be an object".into()))?;
    let format_version = root
        .get("format_version")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput("Terraform plan requires format_version".into()))?;
    let major = format_version
        .split('.')
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    if major != 1 {
        return Err(Error::Unsupported(format!(
            "Terraform JSON format version {format_version} is unsupported; major version 1 is required"
        )));
    }
    let changes = root
        .get("resource_changes")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let drift = root
        .get("resource_drift")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut rows = Vec::new();
    let mut action_counts = ActionCounts::default();
    for change in changes {
        if rows.len() >= MAX_TERRAFORM_RESOURCES {
            return Err(Error::LimitExceeded(format!(
                "Terraform resource changes exceed {MAX_TERRAFORM_RESOURCES}"
            )));
        }
        let Some(object) = change.as_object() else {
            continue;
        };
        let address = object
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or("(unnamed resource)");
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| object.get("mode").and_then(Value::as_str))
            .unwrap_or("(unknown type)");
        let module = object
            .get("module_address")
            .and_then(Value::as_str)
            .unwrap_or("—");
        let change_object = object.get("change").and_then(Value::as_object);
        let actions = change_object
            .and_then(|value| value.get("actions"))
            .and_then(Value::as_array)
            .map(|actions| {
                actions
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        action_counts.add(&actions);
        let action_text = if actions.is_empty() {
            "unknown".to_owned()
        } else {
            actions.join("/")
        };
        let reason = object
            .get("action_reason")
            .and_then(Value::as_str)
            .map_or_else(|| "—".into(), truncate);
        rows.push(vec![
            truncate(address),
            truncate(kind),
            action_text,
            truncate(module),
            reason,
        ]);
    }
    if rows.is_empty() {
        rows.push(vec![
            "(no resource changes)".into(),
            "—".into(),
            "no-op".into(),
            "—".into(),
            "—".into(),
        ]);
    }
    let output_changes = root
        .get("output_changes")
        .and_then(Value::as_object)
        .map_or(0, |value| value.len());
    let variables = root
        .get("variables")
        .and_then(Value::as_object)
        .map_or(0, |value| value.len());
    let mut metadata = vec![
        format!("Format: {format_version}"),
        format!("Resource changes: {}", changes.len()),
        format!("Create: {}", action_counts.create),
        format!("Update: {}", action_counts.update),
        format!("Delete: {}", action_counts.delete),
        format!("Read: {}", action_counts.read),
        format!("Drift entries: {drift}"),
        format!("Output changes: {output_changes}"),
        format!("Variables: {variables} (values omitted)"),
    ];
    if let Some(terraform_version) = root.get("terraform_version").and_then(Value::as_str) {
        metadata.insert(0, format!("Terraform: {}", truncate(terraform_version)));
    }
    let warnings = vec![
        "Terraform before/after values, sensitive_values, state, variables, outputs, configuration, provider schemas, expressions and checks are omitted; plan/apply/refresh, provider, filesystem and network operations are never performed".into(),
        "Resource action sets and drift are summarized from Terraform's JSON representation without evaluating expressions or resolving providers".into(),
    ];
    Ok((
        TableData {
            headers: vec![
                "Address".into(),
                "Type".into(),
                "Actions".into(),
                "Module".into(),
                "Reason".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata.join("\n"),
        warnings,
    ))
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_TERRAFORM_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_TERRAFORM_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_TERRAFORM_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "Terraform plan JSON nesting exceeds {MAX_TERRAFORM_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_TERRAFORM_VALUES {
        return Err(Error::LimitExceeded(format!(
            "Terraform plan JSON contains more than {MAX_TERRAFORM_VALUES} values"
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
        Value::String(value) if value.len() > MAX_TERRAFORM_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "Terraform plan JSON string exceeds {MAX_TERRAFORM_STRING_BYTES} bytes"
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
                if depth > MAX_TERRAFORM_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "Terraform plan JSON nesting exceeds {MAX_TERRAFORM_DEPTH} levels"
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
    fn recognizes_terraform_plan_json() {
        assert!(looks_like_prefix(
            br#"{"format_version":"1.0","resource_changes":[]}"#
        ));
        assert!(!looks_like_prefix(
            b"{\"format_version\":\"2.0\",\"values\":{}}"
        ));
    }

    #[test]
    fn summarizes_actions_without_values() {
        let (table, metadata, warnings) = parse(
            r#"{"format_version":"1.0","terraform_version":"1.8.0","variables":{"token":{"value":"very-secret"}},"resource_changes":[{"address":"aws_instance.web","type":"aws_instance","module_address":"module.app","action_reason":"replace_because_tainted","change":{"actions":["delete","create"],"before":{"password":"very-secret"},"after":{"password":"very-secret"}}}]}"#,
        )
        .unwrap();
        assert!(metadata.contains("Delete: 1"));
        assert_eq!(table.rows[0][2], "delete/create");
        assert_eq!(table.rows[0][4], "replace_because_tainted");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("never")));
    }
}
