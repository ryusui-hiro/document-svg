//! Bounded WACZ (Web Archive Collection Zipped) package previews.
//!
//! A WACZ package contains a manifest, page metadata, indexes and WARC
//! payloads. This adapter reads only the manifest and `pages/pages.jsonl`.
//! Archived WARC bytes, indexes, URLs and replay resources are never opened.

use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ooxml::ZipPackage;
use crate::table::{TableAlign, TableData};

const MAX_WACZ_BYTES: u64 = 256 * 1024 * 1024;
const MAX_WACZ_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WACZ_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_WACZ_PAGES_BYTES: u64 = 128 * 1024 * 1024;
const MAX_WACZ_PAGE_LINE_BYTES: usize = 1024 * 1024;
const MAX_WACZ_PAGES: usize = 200_000;
const MAX_WACZ_DEPTH: usize = 100;
const MAX_WACZ_VALUES: usize = 300_000;
const MAX_WACZ_STRING_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn looks_like_archive(path: &Path) -> bool {
    let Ok(mut package) = ZipPackage::open(path, MAX_WACZ_MANIFEST_BYTES) else {
        return false;
    };
    package.contains("datapackage.json") && package.contains("pages/pages.jsonl")
}

struct WaczPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for WaczPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "wacz".into();
        if page.title.is_empty() {
            page.title = "WACZ web archive".into();
        }
        page.description =
            "WACZ manifest and page metadata are rendered safely; archive payloads are not replayed".into();
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
        options.max_input_bytes.min(MAX_WACZ_BYTES),
        "WACZ input",
    )?;
    let mut package = ZipPackage::from_bytes(
        &bytes,
        options.max_zip_entry_bytes.min(MAX_WACZ_ENTRY_BYTES),
    )
    .map_err(|error| Error::InvalidInput(format!("invalid WACZ ZIP package: {error}")))?;
    if package.entry_count() > crate::ooxml::MAX_ZIP_PACKAGE_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "WACZ package contains more than {} entries",
            crate::ooxml::MAX_ZIP_PACKAGE_ENTRIES
        )));
    }
    let manifest_bytes = package
        .read_limited("datapackage.json", MAX_WACZ_MANIFEST_BYTES)
        .map_err(|error| Error::InvalidInput(format!("WACZ manifest is unreadable: {error}")))?;
    let manifest_text = std::str::from_utf8(&manifest_bytes).map_err(|error| {
        Error::InvalidInput(format!("WACZ manifest must be UTF-8 JSON: {error}"))
    })?;
    let manifest: Value = serde_json::from_str(manifest_text)
        .map_err(|error| Error::InvalidInput(format!("invalid WACZ datapackage.json: {error}")))?;
    let mut value_count = 0usize;
    count_values(&manifest, 0, &mut value_count)?;
    let pages_bytes = package
        .read_limited("pages/pages.jsonl", MAX_WACZ_PAGES_BYTES)
        .map_err(|error| {
            Error::InvalidInput(format!("WACZ pages metadata is unreadable: {error}"))
        })?;
    let (rows, page_count, masked_count) = parse_pages(&pages_bytes)?;
    if page_count == 0 {
        return Err(Error::InvalidInput(
            "WACZ pages/pages.jsonl contains no page records".into(),
        ));
    }
    let title = manifest
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let description = manifest
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let resources = manifest
        .get("resources")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let warc_resources = manifest
        .get("resources")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("path")
                        .and_then(Value::as_str)
                        .is_some_and(|path| path.to_ascii_lowercase().ends_with(".warc"))
                })
                .count()
        })
        .unwrap_or(0);
    let mut warnings = vec![
        "WACZ archive WARC payloads, CDXJ indexes, headers, cookies, scripts and replay resources are not opened or executed".into(),
        "WACZ page URLs are displayed only and are never fetched or replayed".into(),
    ];
    if masked_count > 0 {
        warnings.push(format!(
            "{masked_count} WACZ page URL query value(s) were masked"
        ));
    }
    let mut metadata = vec![
        format!("Pages: {page_count}"),
        format!("Manifest resources: {resources}"),
    ];
    if warc_resources > 0 {
        metadata.push(format!("WARC resources: {warc_resources}"));
    }
    if !title.is_empty() {
        metadata.push(format!("Title: {title}"));
    }
    if !description.is_empty() {
        metadata.push(format!("Description: {}", truncate(description)));
    }
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "WACZ web archive".into(),
        },
        HtmlBlock::Paragraph {
            text: metadata.join("\n"),
        },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Page".into(),
                "URL".into(),
                "Status / MIME".into(),
                "Title / time".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = WaczPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_pages(bytes: &[u8]) -> Result<(Vec<Vec<String>>, usize, usize)> {
    let mut rows = Vec::new();
    let mut count = 0usize;
    let mut masked = 0usize;
    for (line_number, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        if line.len() > MAX_WACZ_PAGE_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "WACZ page metadata line {} exceeds {MAX_WACZ_PAGE_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        count += 1;
        if count > MAX_WACZ_PAGES {
            return Err(Error::LimitExceeded(format!(
                "WACZ page count exceeds {MAX_WACZ_PAGES}"
            )));
        }
        let value: Value = serde_json::from_slice(line).map_err(|error| {
            Error::InvalidInput(format!(
                "invalid WACZ pages.jsonl line {}: {error}",
                line_number + 1
            ))
        })?;
        let object = value.as_object().ok_or_else(|| {
            Error::InvalidInput(format!(
                "WACZ page metadata line {} is not an object",
                line_number + 1
            ))
        })?;
        let id = string(object, "id")
            .or_else(|| string(object, "pageId"))
            .unwrap_or_else(|| count.to_string());
        let url = string(object, "url")
            .or_else(|| string(object, "originalUrl"))
            .unwrap_or_default();
        let (url, masked_here) = mask_url(&url);
        masked += masked_here;
        let title = string(object, "title").unwrap_or_default();
        let timestamp = string(object, "timestamp")
            .or_else(|| string(object, "ts"))
            .unwrap_or_default();
        let status = object.get("status").map(value_text).unwrap_or_default();
        let mime = string(object, "mime")
            .or_else(|| string(object, "mimeType"))
            .unwrap_or_default();
        let status_mime = if status.is_empty() {
            mime
        } else if mime.is_empty() {
            status
        } else {
            format!("{status} {mime}")
        };
        let title_time = if title.is_empty() {
            timestamp
        } else if timestamp.is_empty() {
            title
        } else {
            format!("{title} @ {timestamp}")
        };
        rows.push(vec![
            truncate(&id),
            truncate(&url),
            truncate(&status_mime),
            truncate(&title_time),
        ]);
    }
    Ok((rows, count, masked))
}

fn string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn value_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
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
    if value.len() <= MAX_WACZ_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_WACZ_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_WACZ_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "WACZ manifest nesting exceeds {MAX_WACZ_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_WACZ_VALUES {
        return Err(Error::LimitExceeded(format!(
            "WACZ manifest contains more than {MAX_WACZ_VALUES} values"
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
        Value::String(text) if text.len() > MAX_WACZ_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "WACZ manifest string exceeds {MAX_WACZ_STRING_BYTES} bytes"
            )));
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wacz_page_metadata_and_masks_urls() {
        let manifest: Value =
            serde_json::json!({"title":"Archive","resources":[{"path":"archive/data.warc"}]});
        let pages = br#"{"id":"p1","url":"https://example.invalid/?token=secret","title":"Home","timestamp":"2026-09-16T00:00:00Z","status":200,"mime":"text/html"}
"#;
        let (rows, count, masked) = parse_pages(pages).unwrap();
        assert_eq!(count, 1);
        assert_eq!(rows[0][0], "p1");
        assert!(rows[0][1].contains("token=***"));
        assert_eq!(masked, 1);
        assert_eq!(manifest["title"], "Archive");
    }

    #[test]
    fn rejects_malformed_pages_jsonl() {
        assert!(parse_pages(b"not-json\n").is_err());
    }
}
