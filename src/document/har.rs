//! Bounded HTTP Archive (HAR 1.2) network-log previews.
//!
//! HAR files can contain credentials, cookies and response bodies. This
//! adapter intentionally renders only an inert request/response inventory:
//! sensitive query values are masked, headers/cookies/bodies are omitted, and
//! no URL or network transaction is ever followed.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_HAR_BYTES: u64 = 128 * 1024 * 1024;
const MAX_HAR_DEPTH: usize = 100;
const MAX_HAR_VALUES: usize = 400_000;
const MAX_HAR_ENTRIES: usize = 200_000;
const MAX_HAR_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_HAR_TEXT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"log\"")
        && text.contains("\"entries\"")
        && text.contains("\"version\"")
}

struct HarPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for HarPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "har".into();
        if page.title.is_empty() {
            page.title = "HAR network archive".into();
        }
        page.description =
            "HAR request and response metadata are rendered safely; headers, cookies and bodies are omitted".into();
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
        options.max_input_bytes.min(MAX_HAR_BYTES),
        "HAR input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("HAR input must be UTF-8: {error}")))?;
    let (table, metadata, warnings) = parse_har(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "HAR network archive".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = HarPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_har(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_HAR_BYTES || text.len() > MAX_HAR_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "HAR input exceeds {MAX_HAR_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid HAR JSON: {error}")))?;
    let mut count = 0usize;
    count_values(&value, 0, &mut count)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("HAR root must be an object".into()))?;
    let log = root
        .get("log")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::InvalidInput("HAR root requires a log object".into()))?;
    let version = log
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput("HAR log requires a version string".into()))?;
    let entries = log
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidInput("HAR log requires an entries array".into()))?;
    if entries.len() > MAX_HAR_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "HAR entries exceed {MAX_HAR_ENTRIES}"
        )));
    }
    let creator = log
        .get("creator")
        .and_then(Value::as_object)
        .map(|v| {
            let name = v.get("name").and_then(Value::as_str).unwrap_or_default();
            let version = v.get("version").and_then(Value::as_str).unwrap_or_default();
            if name.is_empty() {
                String::new()
            } else if version.is_empty() {
                name.into()
            } else {
                format!("{name} {version}")
            }
        })
        .unwrap_or_default();
    let pages = log
        .get("pages")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let mut rows = Vec::with_capacity(entries.len().min(MAX_HAR_ENTRIES));
    let mut masked_values = 0usize;
    let mut omitted_bodies = 0usize;
    let mut total_time = 0.0f64;
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let request = entry
            .get("request")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::InvalidInput("HAR entry is missing request object".into()))?;
        let response = entry.get("response").and_then(Value::as_object);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_ascii_uppercase();
        let url = request
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("(missing URL)");
        let (safe_url, masked) = mask_url(url);
        masked_values += masked;
        let status = response
            .and_then(|v| v.get("status"))
            .and_then(Value::as_i64)
            .map_or_else(|| "-".into(), |v| v.to_string());
        let mime = response
            .and_then(|v| v.get("content"))
            .and_then(Value::as_object)
            .and_then(|v| v.get("mimeType"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response_size = response
            .and_then(|v| v.get("content"))
            .and_then(Value::as_object)
            .and_then(|v| v.get("size"))
            .and_then(Value::as_i64)
            .or_else(|| {
                response
                    .and_then(|v| v.get("bodySize"))
                    .and_then(Value::as_i64)
            })
            .map_or_else(|| "-".into(), |v| v.to_string());
        let request_size = request
            .get("bodySize")
            .and_then(Value::as_i64)
            .map_or_else(|| "-".into(), |v| v.to_string());
        if request.get("postData").is_some() || response.and_then(|v| v.get("content")).is_some() {
            omitted_bodies += 1;
        }
        let time = entry.get("time").and_then(Value::as_f64).unwrap_or(0.0);
        if time.is_finite() {
            total_time += time;
        }
        rows.push(vec![
            method,
            safe_url,
            if mime.is_empty() {
                status
            } else {
                format!("{status} {mime}")
            },
            format!("{request_size} / {response_size}"),
            format_time(time),
        ]);
    }
    let mut warnings = vec![
        "HAR may contain privacy/security-sensitive data; headers, cookies, request bodies, response bodies and query values with secret-like names are omitted or masked".into(),
        "HAR URLs are displayed only and are never fetched or replayed".into(),
    ];
    if masked_values > 0 {
        warnings.push(format!(
            "{masked_values} HAR URL query value(s) were masked"
        ));
    }
    if omitted_bodies > 0 {
        warnings.push(format!(
            "{omitted_bodies} HAR entr{} had request or response body data omitted",
            if omitted_bodies == 1 { "y" } else { "ies" }
        ));
    }
    let mut metadata = vec![
        format!("HAR version: {version}"),
        format!("Entries: {}", rows.len()),
        format!("Pages: {pages}"),
    ];
    if !creator.is_empty() {
        metadata.push(format!("Creator: {creator}"));
    }
    metadata.push(format!(
        "Total recorded time: {} ms",
        format_time(total_time)
    ));
    Ok((
        TableData {
            headers: vec![
                "Method".into(),
                "URL".into(),
                "Status / MIME".into(),
                "Req / resp bytes".into(),
                "Time ms".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata.join("\n"),
        warnings,
    ))
}

fn mask_url(url: &str) -> (String, usize) {
    let Some((prefix, query)) = url.split_once('?') else {
        return (url.to_owned(), 0);
    };
    let mut count = 0usize;
    let masked = query
        .split('&')
        .map(|part| {
            let Some((key, value)) = part.split_once('=') else {
                return part.to_owned();
            };
            if is_sensitive_name(key) {
                count += 1;
                format!("{key}=***")
            } else {
                format!("{key}={value}")
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    (format!("{prefix}?{masked}"), count)
}

fn is_sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace(['-', '_'], "");
    [
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
    .any(|needle| name.contains(needle))
}

fn format_time(value: f64) -> String {
    if !value.is_finite() || value < 0.0 {
        return "-".into();
    }
    format!("{value:.2}")
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_HAR_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "HAR nesting exceeds {MAX_HAR_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_HAR_VALUES {
        return Err(Error::LimitExceeded(format!(
            "HAR document contains more than {MAX_HAR_VALUES} values"
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
        Value::String(text) if text.len() > MAX_HAR_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "HAR string exceeds {MAX_HAR_STRING_BYTES} bytes"
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
                if depth > MAX_HAR_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "HAR nesting exceeds {MAX_HAR_DEPTH} levels"
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
    fn previews_har_entries_and_masks_sensitive_query_values() {
        let source = r#"{"log":{"version":"1.2","creator":{"name":"Browser","version":"1"},"pages":[],"entries":[{"time":12.345,"request":{"method":"GET","url":"https://api.example.invalid/pets?api_key=secret&limit=2","headers":[{"name":"Authorization","value":"Bearer token"}],"cookies":[{"name":"sid","value":"cookie"}],"bodySize":0,"postData":{"text":"secret body"}},"response":{"status":200,"content":{"mimeType":"application/json","size":42,"text":"secret response"}}}]}}"#;
        let (table, metadata, warnings) = parse_har(source).unwrap();
        assert!(metadata.contains("HAR version: 1.2"));
        assert_eq!(table.rows[0][0], "GET");
        assert!(table.rows[0][1].contains("api_key=***"));
        assert!(!table.rows[0][1].contains("secret"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("never fetched"))
        );
    }

    #[test]
    fn rejects_non_har_json() {
        assert!(parse_har("{\"log\":{\"version\":\"1.2\"}}").is_err());
    }
}
