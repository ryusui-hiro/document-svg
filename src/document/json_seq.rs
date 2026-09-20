//! Bounded RFC 7464 JSON Text Sequence and newline-delimited previews.

use std::path::Path;
use std::str;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::Page;

const MAX_JSON_SEQUENCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSON_SEQUENCE_RECORD_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_SEQUENCE_RECORDS: usize = 100_000;
const MAX_JSON_SEQUENCE_BLOCKS: usize = 200_000;
const RFC7464_RECORD_SEPARATOR: u8 = 0x1e;

struct JsonSequencePageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
    next_page_number: usize,
}

impl PageConsumer for JsonSequencePageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = self.next_page_number;
        page.source_format = "jsonseq".into();
        page.title = "JSON Text Sequence".into();
        self.next_page_number += 1;
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.first() == Some(&RFC7464_RECORD_SEPARATOR)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_JSON_SEQUENCE_BYTES),
        "JSON Text Sequence input",
    )?;
    let newline_delimited = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"));
    let mut blocks = Vec::new();
    let mut warning_counts = SequenceWarnings::default();
    let record_count = if newline_delimited {
        parse_newline_delimited(&bytes, &mut blocks, &mut warning_counts)?
    } else {
        parse_rfc7464(&bytes, &mut blocks, &mut warning_counts)?
    };
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "JSON Text Sequence contains no valid JSON records".into(),
        ));
    }
    let mut warnings = warning_counts.json_warnings.clone();
    if newline_delimited {
        warnings.push(
            "newline-delimited JSON was read in compatibility mode; RFC 7464 uses RS-prefixed records".into(),
        );
    }
    if warning_counts.empty_records > 0 {
        warnings.push(format!(
            "{} empty JSON sequence record(s) were skipped",
            warning_counts.empty_records
        ));
    }
    if warning_counts.invalid_records > 0 {
        warnings.push(format!(
            "{} invalid JSON sequence record(s) were skipped; check the sequence before relying on its contents",
            warning_counts.invalid_records
        ));
    }
    warnings.push(format!(
        "{record_count} JSON sequence record(s) were laid out in order as inert content"
    ));

    let mut page_sink = JsonSequencePageSink {
        inner: sink,
        warnings: &warnings,
        next_page_number: 1,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct SequenceWarnings {
    empty_records: usize,
    invalid_records: usize,
    json_warnings: Vec<String>,
}

fn parse_rfc7464(
    bytes: &[u8],
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut SequenceWarnings,
) -> Result<usize> {
    if !looks_like_prefix(bytes) {
        return Err(Error::InvalidInput(
            "RFC 7464 JSON Text Sequence must begin with an ASCII Record Separator (0x1E)".into(),
        ));
    }
    let mut cursor = 0usize;
    let mut record_count = 0usize;
    let mut record_number = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != RFC7464_RECORD_SEPARATOR {
            return Err(Error::InvalidInput(
                "expected an RFC 7464 Record Separator".into(),
            ));
        }
        let body_start = cursor + 1;
        let next_separator = bytes[body_start..]
            .iter()
            .position(|byte| *byte == RFC7464_RECORD_SEPARATOR)
            .map(|offset| body_start + offset)
            .unwrap_or(bytes.len());
        let record = &bytes[body_start..next_separator];
        record_number += 1;
        record_count += 1;
        if record_count > MAX_JSON_SEQUENCE_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "JSON Text Sequence exceeds {MAX_JSON_SEQUENCE_RECORDS} records"
            )));
        }
        append_record(record, record_number, blocks, warnings)?;
        cursor = next_separator;
    }
    Ok(record_count)
}

fn parse_newline_delimited(
    bytes: &[u8],
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut SequenceWarnings,
) -> Result<usize> {
    let mut record_count = 0usize;
    let mut record_number = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        let record = line.strip_suffix(b"\r").unwrap_or(line);
        if record.iter().all(u8::is_ascii_whitespace) {
            warnings.empty_records += 1;
            continue;
        }
        if record.first() == Some(&RFC7464_RECORD_SEPARATOR) {
            return Err(Error::InvalidInput(
                "newline-delimited JSON cannot contain RFC 7464 Record Separators".into(),
            ));
        }
        record_number += 1;
        record_count += 1;
        if record_count > MAX_JSON_SEQUENCE_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "newline-delimited JSON exceeds {MAX_JSON_SEQUENCE_RECORDS} records"
            )));
        }
        append_record(record, record_number, blocks, warnings)?;
    }
    if record_count == 0 {
        return Err(Error::InvalidInput(
            "newline-delimited JSON contains no records".into(),
        ));
    }
    Ok(record_count)
}

fn append_record(
    record: &[u8],
    record_number: usize,
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut SequenceWarnings,
) -> Result<()> {
    let mut record = record;
    while record.last().is_some_and(u8::is_ascii_whitespace) {
        record = &record[..record.len() - 1];
    }
    if record.is_empty() {
        warnings.empty_records += 1;
        return Ok(());
    }
    if record.len() > MAX_JSON_SEQUENCE_RECORD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON sequence record exceeds {MAX_JSON_SEQUENCE_RECORD_BYTES} bytes"
        )));
    }
    let text = match str::from_utf8(record) {
        Ok(text) => text,
        Err(_) => {
            warnings.invalid_records += 1;
            return Ok(());
        }
    };
    let (mut record_blocks, record_warnings) = match crate::document::json::parse_json_blocks(text)
    {
        Ok(result) => result,
        Err(Error::InvalidInput(_)) => {
            warnings.invalid_records += 1;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if let Some(HtmlBlock::Heading { level: 1, text }) = record_blocks.first_mut() {
        *text = format!("JSON sequence record {record_number}");
    }
    let output_blocks = blocks
        .len()
        .checked_add(record_blocks.len())
        .ok_or_else(|| {
            Error::LimitExceeded("JSON Text Sequence output block count overflowed".into())
        })?;
    if output_blocks > MAX_JSON_SEQUENCE_BLOCKS {
        return Err(Error::LimitExceeded(format!(
            "JSON Text Sequence exceeds {MAX_JSON_SEQUENCE_BLOCKS} rendered blocks"
        )));
    }
    blocks.append(&mut record_blocks);
    for warning in record_warnings {
        if !warnings.json_warnings.contains(&warning) {
            warnings.json_warnings.push(warning);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_json_text_sequence_record_separator() {
        assert!(looks_like_prefix(b"\x1e{\"event\":\"load\"}\n"));
        assert!(!looks_like_prefix(b"{\"event\":\"load\"}"));
    }

    #[test]
    fn parses_mixed_values_and_labels_each_record() {
        let bytes = b"\x1e{\"event\":\"load\",\"rows\":3}\n\x1e[1,2,true]\n\x1enull\n";
        let mut blocks = Vec::new();
        let mut warnings = SequenceWarnings::default();
        let records = parse_rfc7464(bytes, &mut blocks, &mut warnings).unwrap();
        assert_eq!(records, 3);
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::Heading { text, .. } if text == "JSON sequence record 2"
        )));
        assert!(blocks.len() > 6);
    }

    #[test]
    fn skips_invalid_rfc7464_records_with_a_warning_count() {
        let mut blocks = Vec::new();
        let mut warnings = SequenceWarnings::default();
        let records = parse_rfc7464(
            b"\x1e{\"ok\":true}\n\x1e{invalid}\n\x1e2\n",
            &mut blocks,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(records, 3);
        assert_eq!(warnings.invalid_records, 1);
        assert!(!blocks.is_empty());
    }

    #[test]
    fn parses_newline_delimited_compatibility_records() {
        let mut blocks = Vec::new();
        let mut warnings = SequenceWarnings::default();
        let records = parse_newline_delimited(
            b"{\"key\":\"value\"}\n[\"a\",\"b\"]\n",
            &mut blocks,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(records, 2);
        assert!(blocks.len() > 4);
    }

    #[test]
    fn skips_empty_and_invalid_rfc7464_records_without_hiding_warnings() {
        let mut blocks = Vec::new();
        let mut warnings = SequenceWarnings::default();
        let records = parse_rfc7464(
            b"\x1e\x1e{\"event\":\"ok\"}\n\x1e{broken}\n",
            &mut blocks,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(records, 3);
        assert_eq!(warnings.empty_records, 1);
        assert_eq!(warnings.invalid_records, 1);
        assert_eq!(blocks.len(), 2);
    }
}
