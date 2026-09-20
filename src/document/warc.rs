//! Bounded WARC 1.0/1.1 web-archive record previews.
//!
//! WARC records may contain arbitrary HTTP payloads and private metadata. The
//! reader validates record framing and content lengths, then renders only
//! record headers and a bounded HTTP status/MIME summary. Payloads are skipped
//! and no archived URL or embedded resource is opened.

use std::io::Read;
use std::path::Path;

use flate2::read::MultiGzDecoder;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_WARC_BYTES: u64 = 128 * 1024 * 1024;
const MAX_WARC_DECOMPRESSED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_WARC_RECORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WARC_RECORDS: usize = 200_000;
const MAX_WARC_HEADER_BYTES: usize = 1024 * 1024;
const MAX_WARC_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_WARC_TEXT_BYTES: usize = 128 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    prefix
        .split(|byte| *byte == b'\n')
        .map(|line| String::from_utf8_lossy(line))
        .map(|line| line.trim_end_matches('\r').trim().to_owned())
        .any(|line| line.starts_with("WARC/1."))
}

pub(crate) fn looks_like_gzip_file(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut decoder = MultiGzDecoder::new(file);
    let mut prefix = Vec::new();
    if Read::take(&mut decoder, 4096)
        .read_to_end(&mut prefix)
        .is_err()
    {
        return false;
    }
    looks_like_prefix(&prefix)
}

struct WarcPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for WarcPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "warc".into();
        if page.title.is_empty() {
            page.title = "WARC web archive".into();
        }
        page.description =
            "WARC record metadata is rendered safely; archived payloads and resources are not opened".into();
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
    let compressed = path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".warc.gz"));
    let bytes = if compressed {
        let file = std::fs::File::open(path)?;
        let mut decoder = MultiGzDecoder::new(file);
        let mut data = Vec::new();
        Read::take(
            &mut decoder,
            options
                .max_input_bytes
                .min(MAX_WARC_DECOMPRESSED_BYTES)
                .saturating_add(1),
        )
        .read_to_end(&mut data)?;
        if data.len() as u64 > options.max_input_bytes.min(MAX_WARC_DECOMPRESSED_BYTES) {
            return Err(Error::LimitExceeded(format!(
                "decompressed WARC input exceeds {} bytes",
                options.max_input_bytes.min(MAX_WARC_DECOMPRESSED_BYTES)
            )));
        }
        data
    } else {
        read_limited_file(
            path,
            options.max_input_bytes.min(MAX_WARC_BYTES),
            "WARC input",
        )?
    };
    let (table, metadata, warnings) = parse_warc(&bytes)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "WARC web archive".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = WarcPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_warc(bytes: &[u8]) -> Result<(TableData, String, Vec<String>)> {
    if bytes.len() as u64 > MAX_WARC_BYTES || bytes.len() > MAX_WARC_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "WARC input exceeds {MAX_WARC_BYTES} bytes"
        )));
    }
    let mut rows = Vec::new();
    let mut position = 0usize;
    let mut versions = Vec::new();
    let mut payload_records = 0usize;
    let mut target_mask_count = 0usize;
    while position < bytes.len() {
        while bytes
            .get(position..)
            .is_some_and(|tail| tail.starts_with(b"\r\n") || tail.starts_with(b"\n"))
        {
            position += if bytes[position] == b'\r' { 2 } else { 1 };
        }
        if position >= bytes.len() {
            break;
        }
        if rows.len() >= MAX_WARC_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "WARC record count exceeds {MAX_WARC_RECORDS}"
            )));
        }
        let header_end = find_header_end(bytes, position).ok_or_else(|| {
            Error::InvalidInput("WARC record header is missing its blank-line terminator".into())
        })?;
        if header_end.0.saturating_sub(position) > MAX_WARC_HEADER_BYTES {
            return Err(Error::LimitExceeded(format!(
                "WARC header exceeds {MAX_WARC_HEADER_BYTES} bytes"
            )));
        }
        let header = std::str::from_utf8(&bytes[position..header_end.0])
            .map_err(|error| Error::InvalidInput(format!("WARC header must be UTF-8: {error}")))?;
        let mut lines = header.lines();
        let version = lines
            .next()
            .map(str::trim)
            .ok_or_else(|| Error::InvalidInput("WARC record header is empty".into()))?;
        if !version.starts_with("WARC/1.") {
            return Err(Error::InvalidInput(format!(
                "unsupported WARC record version `{version}`"
            )));
        }
        versions.push(version.to_owned());
        let mut record_type = String::new();
        let mut target = String::new();
        let mut date = String::new();
        let mut content_type = String::new();
        let mut content_length = None::<u64>;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "warc-type" => record_type = value.to_owned(),
                "warc-target-uri" => target = value.to_owned(),
                "warc-date" => date = value.to_owned(),
                "content-type" => content_type = value.to_owned(),
                "content-length" => {
                    content_length = Some(value.parse::<u64>().map_err(|_| {
                        Error::InvalidInput(format!("invalid WARC Content-Length `{value}`"))
                    })?);
                }
                _ => {}
            }
        }
        let length = content_length
            .ok_or_else(|| Error::InvalidInput("WARC record is missing Content-Length".into()))?;
        if length > MAX_WARC_RECORD_BYTES {
            return Err(Error::LimitExceeded(format!(
                "WARC record payload exceeds {MAX_WARC_RECORD_BYTES} bytes"
            )));
        }
        let body_start = header_end.0 + header_end.1;
        let body_end = body_start
            .checked_add(usize::try_from(length).map_err(|_| {
                Error::LimitExceeded("WARC Content-Length does not fit in memory".into())
            })?)
            .ok_or_else(|| Error::LimitExceeded("WARC record offset overflowed".into()))?;
        if body_end > bytes.len() {
            return Err(Error::InvalidInput(format!(
                "WARC record payload is truncated: need {length} bytes"
            )));
        }
        let body = &bytes[body_start..body_end];
        let (status, body_mime) = summarize_http_body(body);
        let mime = if body_mime.is_empty() {
            content_type.clone()
        } else {
            body_mime
        };
        let (safe_target, masked) = mask_url(&target);
        target_mask_count += masked;
        if !body.is_empty() {
            payload_records += 1;
        }
        rows.push(vec![
            if record_type.is_empty() {
                "unknown".into()
            } else {
                record_type
            },
            safe_target,
            status,
            mime,
            date,
            length.to_string(),
        ]);
        position = body_end;
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput("WARC input contains no records".into()));
    }
    let mut warnings = vec![
        "WARC target URLs and embedded payloads are displayed inertly; no archived URL, header, cookie, script, or body is opened or replayed".into(),
    ];
    if payload_records > 0 {
        warnings.push(format!(
            "{payload_records} WARC payload record(s) were skipped after length validation"
        ));
    }
    if target_mask_count > 0 {
        warnings.push(format!(
            "{target_mask_count} WARC target URI query value(s) were masked"
        ));
    }
    let distinct_versions = versions
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(", ");
    let metadata = format!(
        "WARC version(s): {distinct_versions}\nRecords: {}",
        rows.len()
    );
    Ok((
        TableData {
            headers: vec![
                "Type".into(),
                "Target URI".into(),
                "HTTP status".into(),
                "MIME".into(),
                "Date".into(),
                "Length".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 6],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn find_header_end(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let tail = bytes.get(start..)?;
    let crlf = tail.windows(4).position(|window| window == b"\r\n\r\n");
    let lf = tail.windows(2).position(|window| window == b"\n\n");
    match (crlf, lf) {
        (Some(a), Some(b)) if a <= b => Some((start + a, 4)),
        (Some(a), _) => Some((start + a, 4)),
        (_, Some(b)) => Some((start + b, 2)),
        _ => None,
    }
}

fn summarize_http_body(body: &[u8]) -> (String, String) {
    let prefix = &body[..body.len().min(64 * 1024)];
    let text = String::from_utf8_lossy(prefix);
    let mut lines = text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .filter(|value| value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .unwrap_or("-")
        .to_owned();
    let mut mime = String::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-type")
        {
            mime = value.trim().to_owned();
            break;
        }
        if line.trim().is_empty() {
            break;
        }
    }
    (status, mime)
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
    if value.len() <= MAX_WARC_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_WARC_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_warc_records_and_skips_payloads() {
        let source = b"WARC/1.1\r\nWARC-Type: response\r\nWARC-Target-URI: https://example.invalid/x?token=secret&ok=1\r\nWARC-Date: 2026-09-16T00:00:00Z\r\nContent-Type: application/http; msgtype=response\r\nContent-Length: 57\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nsecret body\n";
        let (table, metadata, warnings) = parse_warc(source).unwrap();
        assert!(metadata.contains("WARC/1.1"));
        assert_eq!(table.rows[0][2], "200");
        assert!(table.rows[0][1].contains("token=***"));
        assert!(warnings.iter().any(|warning| warning.contains("skipped")));
    }

    #[test]
    fn rejects_truncated_warc_payload() {
        let source = b"WARC/1.0\nWARC-Type: metadata\nContent-Length: 4\n\nxx";
        assert!(parse_warc(source).is_err());
    }
}
