//! Bounded Citation Style Language (CSL-JSON) bibliography previews.
//!
//! CSL-JSON stores bibliographic metadata for citation processors. This adapter
//! renders safe citation fields and author/date summaries without applying a
//! CSL style, executing markup, resolving DOI/URL links, or fetching resources.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_CSL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CSL_DEPTH: usize = 100;
const MAX_CSL_VALUES: usize = 300_000;
const MAX_CSL_ITEMS: usize = 100_000;
const MAX_CSL_AUTHORS: usize = 512;
const MAX_CSL_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_CSL_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    (trimmed.starts_with('[') || trimmed.starts_with('{'))
        && text.contains("\"type\"")
        && (text.contains("\"title\"") || text.contains("\"author\""))
        && (text.contains("\"issued\"")
            || text.contains("\"container-title\"")
            || text.contains("\"DOI\""))
}

struct CslPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CslPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "csl-json".into();
        if page.title.is_empty() {
            page.title = "CSL-JSON bibliography".into();
        }
        page.description =
            "CSL-JSON citation metadata is rendered inertly; styles, links and external resources are never resolved".into();
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
        options.max_input_bytes.min(MAX_CSL_BYTES),
        "CSL-JSON input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("CSL-JSON must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "CSL-JSON bibliography".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = CslPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_CSL_BYTES {
        return Err(Error::LimitExceeded(format!(
            "CSL-JSON exceeds {MAX_CSL_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid CSL-JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let items = if let Some(items) = value.as_array() {
        items.as_slice()
    } else if let Some(items) = value.get("items").and_then(Value::as_array) {
        items.as_slice()
    } else {
        return Err(Error::InvalidInput(
            "CSL-JSON root must be an item array or an object containing items".into(),
        ));
    };
    if items.len() > MAX_CSL_ITEMS {
        return Err(Error::LimitExceeded(format!(
            "CSL-JSON items exceed {MAX_CSL_ITEMS}"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut with_links = 0usize;
    for (index, item) in items.iter().enumerate() {
        let Some(object) = item.as_object() else {
            warnings.push(format!(
                "CSL-JSON item {} was not an object and was omitted",
                index + 1
            ));
            continue;
        };
        let item_type = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let title = object
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)");
        let id = object
            .get("id")
            .or_else(|| object.get("citation-key"))
            .and_then(Value::as_str)
            .unwrap_or("—");
        let authors = format_names(object.get("author"), object.get("editor"), &mut warnings)?;
        let issued = date_text(object.get("issued"));
        if object.get("DOI").and_then(Value::as_str).is_some()
            || object.get("URL").and_then(Value::as_str).is_some()
        {
            with_links = with_links.saturating_add(1);
        }
        rows.push(vec![
            truncate(id),
            truncate(item_type),
            truncate(title),
            truncate(&authors),
            truncate(&issued),
        ]);
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "CSL-JSON contains no usable citation items".into(),
        ));
    }
    let metadata = format!(
        "Items: {}\nWith authors/editors: {}\nWith DOI/URL: {}",
        rows.len(),
        rows.iter().filter(|row| row[3] != "—").count(),
        with_links
    );
    warnings.push("CSL styles/locales, citation formatting, DOI/URL links, abstract markup and external resources remain inert; no style processor or network request runs".into());
    Ok((
        TableData {
            headers: vec![
                "ID".into(),
                "Type".into(),
                "Title".into(),
                "Author".into(),
                "Yr".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn format_names(
    author: Option<&Value>,
    editor: Option<&Value>,
    warnings: &mut Vec<String>,
) -> Result<String> {
    let names = author.or(editor).and_then(Value::as_array);
    let Some(names) = names else {
        return Ok("—".into());
    };
    if names.len() > MAX_CSL_AUTHORS {
        return Err(Error::LimitExceeded(format!(
            "CSL-JSON authors/editors exceed {MAX_CSL_AUTHORS}"
        )));
    }
    let mut rendered = Vec::new();
    for name in names {
        let Some(object) = name.as_object() else {
            warnings.push("CSL-JSON non-object name entry was omitted".into());
            continue;
        };
        let value = object
            .get("literal")
            .or_else(|| object.get("family"))
            .or_else(|| object.get("given"))
            .and_then(Value::as_str)
            .unwrap_or("(unnamed)");
        rendered.push(truncate(value));
    }
    if rendered.is_empty() {
        Ok("—".into())
    } else {
        Ok(rendered.join(", "))
    }
}

fn date_text(value: Option<&Value>) -> String {
    let parts = value
        .and_then(Value::as_object)
        .and_then(|object| object.get("date-parts"))
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(Value::as_array);
    let Some(parts) = parts else {
        return "—".into();
    };
    let values = parts
        .iter()
        .filter_map(Value::as_i64)
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    values.first().cloned().unwrap_or_else(|| "—".into())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_CSL_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_CSL_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_CSL_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "CSL-JSON nesting exceeds {MAX_CSL_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_CSL_VALUES {
        return Err(Error::LimitExceeded(format!(
            "CSL-JSON contains more than {MAX_CSL_VALUES} values"
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
        Value::String(value) if value.len() > MAX_CSL_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "CSL-JSON string exceeds {MAX_CSL_STRING_BYTES} bytes"
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
                if depth > MAX_CSL_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "CSL-JSON nesting exceeds {MAX_CSL_DEPTH} levels"
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
    fn recognizes_csl_json() {
        assert!(looks_like_prefix(br#"[{"id":"x","type":"article-journal","title":"T","author":[],"issued":{"date-parts":[[2024]]}}]"#));
        assert!(!looks_like_prefix(b"[{\"type\":\"article\",\"value\":1}]"));
    }

    #[test]
    fn summarizes_citations_without_fetching() {
        let (table, metadata, warnings) = parse(
            r#"[{"id":"paper-1","type":"article-journal","title":"A title","author":[{"family":"Doe","given":"Jane"}],"issued":{"date-parts":[[2024,5]]},"DOI":"10.1234/example","URL":"https://private.example/paper","abstract":"very-secret"}]"#,
        )
        .unwrap();
        assert!(metadata.contains("Items: 1"));
        assert_eq!(table.rows[0][3], "Doe");
        assert_eq!(table.rows[0][4], "2024");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("private"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("inert")));
    }
}
