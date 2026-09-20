//! Bounded, inert previews for TOML configuration documents.

use std::path::Path;

use toml::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_TOML_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TOML_LINES: usize = 50_000;
const MAX_TOML_LINE_BYTES: usize = 64 * 1024;
const MAX_TOML_DEPTH: usize = 80;
const MAX_TOML_VALUES: usize = 200_000;
const MAX_TOML_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOML_PATH_BYTES: usize = 4 * 1024;
const MAX_TOML_RENDERED_TEXT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
struct PreviewState {
    blocks: Vec<HtmlBlock>,
    values: usize,
    text_bytes: usize,
    truncated_paths: usize,
    normalized_floats: usize,
}

struct TomlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for TomlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "toml".into();
        if page.title.is_empty() {
            page.title = "TOML configuration".into();
        }
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
        options.max_input_bytes.min(MAX_TOML_BYTES),
        "TOML input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("TOML input must be UTF-8: {error}")))?;
    let (blocks, warnings) = parse_toml_blocks(&text)?;
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "TOML conversion requires at least one page; max_pages is zero".into(),
        ));
    }
    let mut page_sink = TomlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

pub(crate) fn parse_toml_blocks(text: &str) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if text.len() as u64 > MAX_TOML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "TOML input exceeds {MAX_TOML_BYTES} bytes"
        )));
    }
    for (line_number, line) in text.lines().enumerate() {
        if line_number >= MAX_TOML_LINES {
            return Err(Error::LimitExceeded(format!(
                "TOML input exceeds {MAX_TOML_LINES} lines"
            )));
        }
        if line.len() > MAX_TOML_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "TOML line exceeds {MAX_TOML_LINE_BYTES} bytes"
            )));
        }
    }

    let value = toml::from_str::<Value>(text)
        .map_err(|error| Error::InvalidInput(format!("invalid TOML input: {error}")))?;
    let mut state = PreviewState::default();
    walk_value(&value, "$", 0, &mut state)?;

    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: "TOML configuration".into(),
    }];
    blocks.append(&mut state.blocks);
    let mut warnings = Vec::new();
    if state.truncated_paths > 0 {
        warnings.push(format!(
            "{} TOML field path(s) were truncated to {} bytes for display",
            state.truncated_paths, MAX_TOML_PATH_BYTES
        ));
    }
    if state.normalized_floats > 0 {
        warnings.push(format!(
            "{} TOML float value(s) were normalized by numeric parsing; original spelling is not retained",
            state.normalized_floats
        ));
    }
    Ok((blocks, warnings))
}

fn walk_value(value: &Value, path: &str, depth: usize, state: &mut PreviewState) -> Result<()> {
    if depth > MAX_TOML_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "TOML nesting exceeds {MAX_TOML_DEPTH} levels"
        )));
    }
    state.values = state.values.saturating_add(1);
    if state.values > MAX_TOML_VALUES {
        return Err(Error::LimitExceeded(format!(
            "TOML document contains more than {MAX_TOML_VALUES} values"
        )));
    }

    match value {
        Value::String(value) => {
            if value.len() > MAX_TOML_STRING_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "TOML string exceeds {MAX_TOML_STRING_BYTES} bytes"
                )));
            }
            let encoded = serde_json::to_string(value)?;
            add_value_line(state, format!("{path} (string): {encoded}"))
        }
        Value::Integer(value) => add_value_line(state, format!("{path} (integer): {value}")),
        Value::Float(value) => {
            state.normalized_floats = state.normalized_floats.saturating_add(1);
            add_value_line(state, format!("{path} (float): {value}"))
        }
        Value::Boolean(value) => add_value_line(state, format!("{path} (boolean): {value}")),
        Value::Datetime(value) => add_value_line(state, format!("{path} (datetime): {value}")),
        Value::Array(values) => {
            if values.is_empty() {
                return add_value_line(state, format!("{path} (array): []"));
            }
            for (index, value) in values.iter().enumerate() {
                walk_value(value, &format!("{path}[{index}]"), depth + 1, state)?;
            }
            Ok(())
        }
        Value::Table(table) => {
            if table.is_empty() {
                return add_value_line(state, format!("{path} (table): {{}}"));
            }
            for (key, value) in table {
                let child_path = join_path(path, key, state)?;
                walk_value(value, &child_path, depth + 1, state)?;
            }
            Ok(())
        }
    }
}

fn join_path(parent: &str, key: &str, state: &mut PreviewState) -> Result<String> {
    if key.len() > MAX_TOML_STRING_BYTES {
        return Err(Error::LimitExceeded(format!(
            "TOML key exceeds {MAX_TOML_STRING_BYTES} bytes"
        )));
    }
    let simple_key = !key.is_empty()
        && key.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character == '$'
                || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
        });
    let child = if simple_key {
        format!("{parent}.{key}")
    } else {
        let encoded = serde_json::to_string(key)?;
        format!("{parent}[{encoded}]")
    };
    if child.len() <= MAX_TOML_PATH_BYTES {
        return Ok(child);
    }
    state.truncated_paths = state.truncated_paths.saturating_add(1);
    let mut end = MAX_TOML_PATH_BYTES;
    while !child.is_char_boundary(end) {
        end -= 1;
    }
    Ok(child[..end].to_owned())
}

fn add_value_line(state: &mut PreviewState, line: String) -> Result<()> {
    if state.blocks.len() >= MAX_TOML_VALUES {
        return Err(Error::LimitExceeded(format!(
            "TOML preview exceeds {MAX_TOML_VALUES} rendered values"
        )));
    }
    let text_bytes = state.text_bytes.saturating_add(line.len());
    if text_bytes > MAX_TOML_RENDERED_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "TOML preview text exceeds {MAX_TOML_RENDERED_TEXT_BYTES} bytes"
        )));
    }
    state.text_bytes = text_bytes;
    state.blocks.push(HtmlBlock::Paragraph { text: line });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_nested_tables_arrays_datetimes_and_inert_strings() {
        let source = r#"
title = "Build configuration"
unsafe_example = "<script>this is text</script>"
created = 2026-08-03T17:20:00Z
ports = [8080, 8081]
ratio = 1.5000

[service]
name = "api"
enabled = true

[[targets]]
name = "debug"

[[targets]]
name = "release"
"#;
        let (blocks, warnings) = parse_toml_blocks(source).unwrap();
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Heading { text, .. } | HtmlBlock::Paragraph { text } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(text.contains(&"$.service.name (string): \"api\""));
        assert!(text.contains(&"$.ports[1] (integer): 8081"));
        assert!(text.contains(&"$.ratio (float): 1.5"));
        assert!(text.contains(&"$.targets[1].name (string): \"release\""));
        assert!(text.contains(&"$.created (datetime): 2026-08-03T17:20:00Z"));
        assert!(
            text.iter()
                .any(|value| value.contains("<script>this is text</script>"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("float value(s) were normalized"))
        );
    }

    #[test]
    fn rejects_invalid_toml_and_enforces_line_and_depth_bounds() {
        assert!(matches!(
            parse_toml_blocks("value = ["),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            parse_toml_blocks("key = 1\nkey = 2\n"),
            Err(Error::InvalidInput(_))
        ));
        let nested = format!(
            "value = {}0{}",
            "[".repeat(MAX_TOML_DEPTH + 1),
            "]".repeat(MAX_TOML_DEPTH + 1)
        );
        assert!(parse_toml_blocks(&nested).is_err());
        let long_line = format!("key = \"{}\"", "x".repeat(MAX_TOML_LINE_BYTES));
        assert!(matches!(
            parse_toml_blocks(&long_line),
            Err(Error::LimitExceeded(_))
        ));
    }
}
