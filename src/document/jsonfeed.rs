//! Bounded JSON Feed 1.0/1.1 previews.
//!
//! JSON Feed items may contain HTML, Markdown, links, images, authors and
//! attachments. This adapter renders item metadata and attachment counts only;
//! content and URLs are never fetched, rendered as active markup, or followed.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_JSONFEED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSONFEED_DEPTH: usize = 100;
const MAX_JSONFEED_VALUES: usize = 300_000;
const MAX_JSONFEED_ITEMS: usize = 100_000;
const MAX_JSONFEED_ATTACHMENTS: usize = 100;
const MAX_JSONFEED_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSONFEED_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"version\"")
        && text.contains("jsonfeed.org/version/")
        && text.contains("\"items\"")
        && text.contains("\"title\"")
}

struct JsonFeedPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for JsonFeedPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "jsonfeed".into();
        if page.title.is_empty() {
            page.title = "JSON Feed".into();
        }
        page.description =
            "JSON Feed item metadata is rendered inertly; content, URLs and attachments are not fetched or executed".into();
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
        options.max_input_bytes.min(MAX_JSONFEED_BYTES),
        "JSON Feed input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("JSON Feed must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "JSON Feed".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = JsonFeedPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_JSONFEED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON Feed exceeds {MAX_JSONFEED_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid JSON Feed: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("JSON Feed root must be an object".into()))?;
    let version = root
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput("JSON Feed requires a version URL".into()))?;
    if !version.starts_with("https://jsonfeed.org/version/")
        && !version.starts_with("http://jsonfeed.org/version/")
    {
        return Err(Error::Unsupported(format!(
            "JSON Feed version URL '{version}' is unsupported"
        )));
    }
    let title = root.get("title").and_then(Value::as_str).unwrap_or("—");
    let items = root
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidInput("JSON Feed requires an items array".into()))?;
    if items.len() > MAX_JSONFEED_ITEMS {
        return Err(Error::LimitExceeded(format!(
            "JSON Feed items exceed {MAX_JSONFEED_ITEMS}"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut content_count = 0usize;
    let mut link_count = 0usize;
    for (index, item) in items.iter().enumerate() {
        let object = item.as_object().ok_or_else(|| {
            Error::InvalidInput(format!("JSON Feed item {} must be an object", index + 1))
        })?;
        let id = object.get("id").and_then(Value::as_str).ok_or_else(|| {
            Error::InvalidInput(format!("JSON Feed item {} requires id", index + 1))
        })?;
        let item_title = object
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("(untitled)");
        let date = object
            .get("date_published")
            .or_else(|| object.get("date_modified"))
            .and_then(Value::as_str)
            .map_or_else(
                || "—".into(),
                |value| truncate(&value.chars().take(10).collect::<String>()),
            );
        let item_type = if object.get("content_html").is_some() {
            content_count = content_count.saturating_add(1);
            "html"
        } else if object.get("content_text").is_some() {
            content_count = content_count.saturating_add(1);
            "text"
        } else {
            "—"
        };
        if object.get("url").is_some() || object.get("external_url").is_some() {
            link_count = link_count.saturating_add(1);
        }
        let attachments = object
            .get("attachments")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if attachments > MAX_JSONFEED_ATTACHMENTS {
            return Err(Error::LimitExceeded(format!(
                "JSON Feed item {} attachments exceed {MAX_JSONFEED_ATTACHMENTS}",
                index + 1
            )));
        }
        let author = object
            .get("author")
            .and_then(Value::as_object)
            .and_then(|author| author.get("name"))
            .and_then(Value::as_str)
            .map_or_else(|| "—".into(), truncate);
        rows.push(vec![
            truncate(id),
            truncate(item_title),
            item_type.into(),
            date,
            author,
            attachments.to_string(),
        ]);
    }
    if rows.is_empty() {
        rows.push(vec![
            "—".into(),
            "(no items)".into(),
            "—".into(),
            "—".into(),
            "—".into(),
            "0".into(),
        ]);
    }
    let metadata = format!(
        "Title: {}\nVersion: {}\nItems: {}\nWith content: {content_count}\nWith links: {link_count}",
        truncate(title),
        truncate(version),
        items.len()
    );
    warnings.push("JSON Feed content_text/content_html, summaries, tags, URLs, images and attachment URLs are omitted; no markup, link, feed or network operation is executed".into());
    warnings.push("JSON Feed item identifiers and dates are shown as metadata without fetching or resolving linked content".into());
    Ok((
        TableData {
            headers: vec![
                "ID".into(),
                "Title".into(),
                "Content".into(),
                "Date".into(),
                "Author".into(),
                "Att".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 6],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_JSONFEED_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_JSONFEED_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_JSONFEED_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "JSON Feed nesting exceeds {MAX_JSONFEED_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_JSONFEED_VALUES {
        return Err(Error::LimitExceeded(format!(
            "JSON Feed contains more than {MAX_JSONFEED_VALUES} values"
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
        Value::String(value) if value.len() > MAX_JSONFEED_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "JSON Feed string exceeds {MAX_JSONFEED_STRING_BYTES} bytes"
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
                if depth > MAX_JSONFEED_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "JSON Feed nesting exceeds {MAX_JSONFEED_DEPTH} levels"
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
    fn recognizes_json_feed() {
        assert!(looks_like_prefix(
            br#"{"version":"https://jsonfeed.org/version/1.1","title":"Feed","items":[]}"#
        ));
        assert!(!looks_like_prefix(
            b"{\"version\":\"1\",\"title\":\"x\",\"items\":[]}"
        ));
    }

    #[test]
    fn summarizes_items_without_content_or_url_payloads() {
        let (table, metadata, warnings) = parse(
            r#"{"version":"https://jsonfeed.org/version/1.1","title":"Updates","items":[{"id":"1","title":"Hello","content_html":"<script>very-secret</script>","url":"https://private.example/?token=secret","date_published":"2024-05-12T00:00:00Z","author":{"name":"Alice"},"attachments":[{"url":"https://private.example/file"}]}]}"#,
        )
        .unwrap();
        assert!(metadata.contains("Items: 1"));
        assert_eq!(table.rows[0][2], "html");
        assert_eq!(table.rows[0][3], "2024-05-12");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("private"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("omitted")));
    }
}
