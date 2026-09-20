//! Bounded MARC21 ISO 2709 record previews.
//!
//! ISO 2709 records contain a 24-byte leader, a directory and a field area.
//! This adapter validates those boundaries and renders field/subfield metadata
//! without exposing URL payloads or contacting catalog services.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_MARC_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MARC_RECORDS: usize = 100_000;
const MAX_MARC_FIELDS: usize = 500_000;
const MAX_MARC_SUBFIELDS: usize = 1_000_000;
const MAX_MARC_ROWS: usize = 200_000;
const MAX_MARC_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    if bytes.len() < 24 || !bytes[..5].iter().all(u8::is_ascii_digit) {
        return false;
    }
    let record_length = parse_digits(&bytes[..5]);
    let base = parse_digits(&bytes[12..17]);
    if !(25..=MAX_MARC_BYTES as usize).contains(&record_length)
        || !(24..record_length).contains(&base)
        || record_length < base
    {
        return false;
    }
    let directory_end = bytes
        .get(24..bytes.len().min(base))
        .and_then(|directory| directory.iter().position(|byte| *byte == 0x1e))
        .map(|offset| offset + 24);
    directory_end.is_some_and(|end| end > 24 && (end - 24) % 12 == 0)
}

struct MarcPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for MarcPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "marc".into();
        if page.title.is_empty() {
            page.title = "MARC21 ISO 2709 record".into();
        }
        page.description =
            "MARC21 ISO 2709 fields and subfields are rendered inertly; URL payloads and catalog services are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    records: usize,
    leaders: usize,
    fields: usize,
    subfields: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_MARC_BYTES),
        "MARC21 ISO 2709 input",
    )?;
    let mut summary = Summary::default();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if summary.records >= MAX_MARC_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "MARC records exceed {MAX_MARC_RECORDS}"
            )));
        }
        let remaining = &bytes[offset..];
        if remaining.len() < 24 {
            return Err(Error::InvalidInput(
                "MARC record has a truncated leader".into(),
            ));
        }
        let record_length = parse_digits(&remaining[..5]);
        if !(25..=MAX_MARC_BYTES as usize).contains(&record_length)
            || record_length > remaining.len()
        {
            return Err(Error::InvalidInput(
                "MARC record length is invalid or exceeds the input".into(),
            ));
        }
        parse_record(
            &remaining[..record_length],
            summary.records + 1,
            &mut summary,
        )?;
        summary.records = summary.records.saturating_add(1);
        offset = offset.saturating_add(record_length);
    }
    if summary.records == 0 {
        return Err(Error::InvalidInput("MARC input contains no records".into()));
    }
    let metadata = format!(
        "Records: {}\nLeaders: {}\nFields: {}\nSubfields: {}",
        summary.records, summary.leaders, summary.fields, summary.subfields
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "MARC21 ISO 2709 record preview".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Record".into(), "Tag".into(), "Code".into(), "Value".into()],
            rows: summary.rows,
            alignments: vec![
                TableAlign::Right,
                TableAlign::Left,
                TableAlign::Left,
                TableAlign::Left,
            ],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "MARC21 leader, directory and field/subfield metadata are shown; URL values, catalog references and unrecognized payloads are omitted or redacted".into(),
        "ISO 2709 record boundaries and directory offsets are validated; no catalog, schema, script or network operation runs".into(),
    ];
    let mut page_sink = MarcPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_record(record: &[u8], number: usize, summary: &mut Summary) -> Result<()> {
    if record.len() < 25 || record[record.len() - 1] != 0x1d {
        return Err(Error::InvalidInput(
            "MARC record must end with a record terminator".into(),
        ));
    }
    let base = parse_digits(&record[12..17]);
    if !(24..record.len()).contains(&base) {
        return Err(Error::InvalidInput(
            "MARC base address is outside the record".into(),
        ));
    }
    let directory_end = record[24..base]
        .iter()
        .position(|byte| *byte == 0x1e)
        .map(|offset| offset + 24)
        .ok_or_else(|| Error::InvalidInput("MARC directory terminator is missing".into()))?;
    if (directory_end - 24) % 12 != 0 {
        return Err(Error::InvalidInput(
            "MARC directory size is not 12-byte aligned".into(),
        ));
    }
    let mut directory = 24usize;
    summary.leaders = summary.leaders.saturating_add(1);
    push_row(summary, number, "leader", "—", display_bytes(&record[..24]))?;
    while directory < directory_end {
        let entry = &record[directory..directory + 12];
        let tag = display_bytes(&entry[..3]);
        let length = parse_digits(&entry[3..7]);
        let start = parse_digits(&entry[7..12]);
        if length == 0 || start > record.len().saturating_sub(base) {
            return Err(Error::InvalidInput(
                "MARC directory field range is invalid".into(),
            ));
        }
        let field_start = base.saturating_add(start);
        let field_end = field_start.saturating_add(length);
        if field_end > record.len() || record[field_end - 1] != 0x1e {
            return Err(Error::InvalidInput(
                "MARC directory field exceeds record".into(),
            ));
        }
        let field = &record[field_start..field_end - 1];
        if summary.fields >= MAX_MARC_FIELDS {
            return Err(Error::LimitExceeded(format!(
                "MARC fields exceed {MAX_MARC_FIELDS}"
            )));
        }
        summary.fields = summary.fields.saturating_add(1);
        if tag.as_bytes().starts_with(b"00") {
            push_row(
                summary,
                number,
                &tag,
                "—",
                safe_value(&field_bytes(field), &tag),
            )?;
        } else if field.len() < 2 {
            push_row(summary, number, &tag, "—", "missing indicators".into())?;
        } else {
            let indicators = display_bytes(&field[..2]);
            let payload = &field[2..];
            let parts = payload.split(|byte| *byte == 0x1f);
            let mut saw_subfield = false;
            for part in parts {
                if part.is_empty() {
                    continue;
                }
                saw_subfield = true;
                let code = display_bytes(&part[..1]);
                let value = safe_value(&field_bytes(&part[1..]), &tag);
                if summary.subfields >= MAX_MARC_SUBFIELDS {
                    return Err(Error::LimitExceeded(format!(
                        "MARC subfields exceed {MAX_MARC_SUBFIELDS}"
                    )));
                }
                summary.subfields = summary.subfields.saturating_add(1);
                push_row(summary, number, &tag, &code, value)?;
            }
            if !saw_subfield {
                push_row(summary, number, &tag, &indicators, "no subfields".into())?;
            }
        }
        directory += 12;
    }
    Ok(())
}

fn parse_digits(bytes: &[u8]) -> usize {
    bytes.iter().fold(0usize, |value, byte| {
        value
            .saturating_mul(10)
            .saturating_add(byte.saturating_sub(b'0') as usize)
    })
}

fn field_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn display_bytes(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn safe_value(value: &str, tag: &str) -> String {
    if tag == "856" || value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value)
    }
}

fn push_row(
    summary: &mut Summary,
    record: usize,
    tag: &str,
    code: &str,
    value: String,
) -> Result<()> {
    if summary.rows.len() >= MAX_MARC_ROWS {
        return Err(Error::LimitExceeded(format!(
            "MARC rendered rows exceed {MAX_MARC_ROWS}"
        )));
    }
    summary.rows.push(vec![
        record.to_string(),
        truncate(tag),
        truncate(code),
        truncate(&value),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_MARC_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_MARC_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_iso2709_leader_and_directory() {
        let record = minimal_record();
        assert!(looks_like_prefix(&record));
        assert!(!looks_like_prefix(b"not a MARC record"));
    }

    #[test]
    fn parses_directory_and_redacts_urls() {
        let record = minimal_record();
        let mut summary = Summary::default();
        parse_record(&record, 1, &mut summary).unwrap();
        assert_eq!(summary.fields, 2);
        assert_eq!(summary.subfields, 1);
        assert!(
            summary
                .rows
                .iter()
                .flatten()
                .any(|value| value == "[URL omitted]")
        );
    }

    fn minimal_record() -> Vec<u8> {
        build_record(vec![
            ("001", b"record-001".to_vec()),
            (
                "856",
                [
                    b"40".as_slice(),
                    &[0x1f, b'u'],
                    b"https://private.example.invalid",
                ]
                .concat(),
            ),
        ])
    }

    fn build_record(fields: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut directory = Vec::new();
        let mut field_area = Vec::new();
        for (tag, value) in fields {
            let start = field_area.len();
            field_area.extend_from_slice(&value);
            field_area.push(0x1e);
            directory.extend_from_slice(tag.as_bytes());
            directory.extend_from_slice(format!("{:04}", value.len() + 1).as_bytes());
            directory.extend_from_slice(format!("{:05}", start).as_bytes());
        }
        directory.push(0x1e);
        let base = 24 + directory.len();
        let length = base + field_area.len() + 1;
        let mut leader = format!("{:05}nam a2200000 i 4500", length).into_bytes();
        leader[12..17].copy_from_slice(format!("{:05}", base).as_bytes());
        let mut record = leader;
        record.extend_from_slice(&directory);
        record.extend_from_slice(&field_area);
        record.push(0x1d);
        record
    }
}
