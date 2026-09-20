//! Bounded RIS bibliography interchange preview.
//!
//! RIS records are rendered as inert citation rows. DOI/URL fields and other
//! identifiers are never fetched, resolved, or interpreted as commands.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_RIS_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RIS_LINES: usize = 2_000_000;
const MAX_RIS_LINE_BYTES: usize = 1 << 20;
const MAX_RIS_RECORDS: usize = 100_000;
const MAX_RIS_FIELD_BYTES: usize = 64 * 1024;
const MAX_RIS_RENDERED_BYTES: usize = 64 * 1024 * 1024;
const MAX_RIS_AUTHORS: usize = 512;
const MAX_RIS_KEYWORDS: usize = 512;
const MAX_RIS_PREVIEW: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| is_tag_line(line, "TY"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_RIS_BYTES),
        "RIS input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("RIS input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse_ris(&text)?;
    let mut page_sink = RisPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "ris", options, &mut page_sink)?;
    Ok(warnings)
}

struct RisPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for RisPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "ris".into();
        page.title = "RIS bibliography".into();
        page.description =
            "RIS citation fields are displayed inertly without DOI or URL lookup".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Record {
    kind: String,
    title: String,
    authors: Vec<String>,
    year: String,
    journal: String,
    doi: String,
    url: String,
    abstract_text: String,
    keywords: Vec<String>,
    pages: String,
    unknown_fields: usize,
    last_tag: Option<String>,
}

fn parse_ris(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_RIS_BYTES {
        return Err(Error::LimitExceeded(format!(
            "RIS input exceeds {MAX_RIS_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_RIS_LINES {
        return Err(Error::LimitExceeded(format!(
            "RIS input exceeds {MAX_RIS_LINES} lines"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut current = None::<Record>;
    let mut record_number = 0usize;
    let mut rendered_bytes = 0usize;
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_RIS_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "RIS line {} exceeds {MAX_RIS_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if is_tag_line(line, "TY") {
            if let Some(previous) = current.take() {
                warnings.push(format!(
                    "RIS record before line {} was missing ER; it was finalized at the next TY",
                    line_number + 1
                ));
                finalize_record(
                    previous,
                    &mut rows,
                    &mut warnings,
                    &mut record_number,
                    &mut rendered_bytes,
                )?;
            }
            current = Some(Record {
                kind: tag_value(line).to_owned(),
                ..Record::default()
            });
            if current
                .as_ref()
                .is_some_and(|record| record.kind.is_empty())
            {
                return Err(Error::InvalidInput(format!(
                    "RIS line {} TY has an empty reference type",
                    line_number + 1
                )));
            }
            current.as_mut().unwrap().last_tag = Some("TY".into());
            continue;
        }
        let Some(record) = current.as_mut() else {
            return Err(Error::InvalidInput(format!(
                "RIS line {} appeared before TY",
                line_number + 1
            )));
        };
        if is_tag_line(line, "ER") {
            let record = current.take().expect("current record exists");
            finalize_record(
                record,
                &mut rows,
                &mut warnings,
                &mut record_number,
                &mut rendered_bytes,
            )?;
            continue;
        }
        if line.as_bytes().starts_with(b"      ") {
            let continuation = line.get(6..).unwrap_or_default().trim();
            if continuation.is_empty() {
                continue;
            }
            append_continuation(record, continuation)?;
            continue;
        }
        if !valid_tag_line(line) {
            return Err(Error::InvalidInput(format!(
                "RIS line {} is not a canonical XX  - value field",
                line_number + 1
            )));
        }
        let tag = line.get(..2).unwrap_or_default().to_ascii_uppercase();
        let value = tag_value(line).trim();
        validate_field(value, "RIS field")?;
        apply_field(record, &tag, value)?;
        record.last_tag = Some(tag);
    }
    if let Some(record) = current.take() {
        warnings.push("RIS final record was missing ER and was finalized at EOF".into());
        finalize_record(
            record,
            &mut rows,
            &mut warnings,
            &mut record_number,
            &mut rendered_bytes,
        )?;
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput("RIS input contains no records".into()));
    }
    if warnings
        .iter()
        .any(|warning| warning.contains("unknown RIS"))
    { /* retained for dedup */ }
    let headers = [
        "Record", "Type", "Title", "Authors", "Year", "Journal", "Pages", "DOI", "URL", "Keywords",
        "Abstract",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let alignments = vec![
        TableAlign::Right,
        TableAlign::Center,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Right,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
    ];
    Ok((
        TableData {
            headers,
            rows,
            alignments,
            raw_source: String::new(),
        },
        dedup_warnings(warnings),
    ))
}

fn apply_field(record: &mut Record, tag: &str, value: &str) -> Result<()> {
    match tag {
        "TY" => record.kind = value.to_owned(),
        "TI" | "T1" => append_value(&mut record.title, value, "RIS title")?,
        "AU" | "A1" => {
            if record.authors.len() >= MAX_RIS_AUTHORS {
                return Err(Error::LimitExceeded(format!(
                    "RIS exceeds {MAX_RIS_AUTHORS} authors per record"
                )));
            }
            record.authors.push(value.to_owned());
        }
        "PY" | "Y1" => append_value(&mut record.year, value, "RIS year")?,
        "JO" | "JF" | "T2" | "J2" => append_value(&mut record.journal, value, "RIS journal")?,
        "DO" => append_value(&mut record.doi, value, "RIS DOI")?,
        "UR" => append_value(&mut record.url, value, "RIS URL")?,
        "AB" | "N2" => append_value(&mut record.abstract_text, value, "RIS abstract")?,
        "KW" => {
            if record.keywords.len() >= MAX_RIS_KEYWORDS {
                return Err(Error::LimitExceeded(format!(
                    "RIS exceeds {MAX_RIS_KEYWORDS} keywords per record"
                )));
            }
            record.keywords.push(value.to_owned());
        }
        "SP" => append_value(&mut record.pages, value, "RIS start page")?,
        "EP" => {
            if !record.pages.is_empty() {
                record.pages.push('-');
            }
            record.pages.push_str(value);
            validate_field(&record.pages, "RIS pages")?;
        }
        "ER" => {}
        _ => record.unknown_fields = record.unknown_fields.saturating_add(1),
    }
    Ok(())
}

fn append_continuation(record: &mut Record, value: &str) -> Result<()> {
    match record.last_tag.as_deref() {
        Some("TI") | Some("T1") => append_value(&mut record.title, value, "RIS title"),
        Some("PY") | Some("Y1") => append_value(&mut record.year, value, "RIS year"),
        Some("JO") | Some("JF") | Some("T2") | Some("J2") => {
            append_value(&mut record.journal, value, "RIS journal")
        }
        Some("DO") => append_value(&mut record.doi, value, "RIS DOI"),
        Some("UR") => append_value(&mut record.url, value, "RIS URL"),
        Some("AB") | Some("N2") => append_value(&mut record.abstract_text, value, "RIS abstract"),
        _ => Ok(()),
    }
}

fn finalize_record(
    record: Record,
    rows: &mut Vec<Vec<String>>,
    warnings: &mut Vec<String>,
    record_number: &mut usize,
    rendered_bytes: &mut usize,
) -> Result<()> {
    *record_number = (*record_number)
        .checked_add(1)
        .ok_or_else(|| Error::LimitExceeded("RIS record count overflowed".into()))?;
    if *record_number > MAX_RIS_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "RIS exceeds {MAX_RIS_RECORDS} records"
        )));
    }
    if record.unknown_fields > 0 {
        warnings.push(format!(
            "unknown RIS fields in record {} were kept inert",
            *record_number
        ));
    }
    let authors = record.authors.join("; ");
    let keywords = record.keywords.join("; ");
    let abstract_preview = preview_text(&record.abstract_text);
    let values = vec![
        record_number.to_string(),
        record.kind,
        record.title,
        authors,
        record.year,
        record.journal,
        record.pages,
        record.doi,
        record.url,
        keywords,
        abstract_preview,
    ];
    let bytes = values.iter().map(String::len).sum::<usize>();
    *rendered_bytes = (*rendered_bytes)
        .checked_add(bytes)
        .ok_or_else(|| Error::LimitExceeded("RIS rendered text byte count overflowed".into()))?;
    if *rendered_bytes > MAX_RIS_RENDERED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "RIS rendered text exceeds {MAX_RIS_RENDERED_BYTES} bytes"
        )));
    }
    rows.push(values);
    Ok(())
}

fn valid_tag_line(line: &str) -> bool {
    line.len() >= 6
        && line.as_bytes().get(0..2).is_some_and(|tag| {
            tag.iter()
                .all(|byte| byte.is_ascii_alphabetic() || byte.is_ascii_digit())
        })
        && line.as_bytes().get(2..4) == Some(b"  ")
        && line.as_bytes().get(4) == Some(&b'-')
        && line.as_bytes().get(5) == Some(&b' ')
}
fn is_tag_line(line: &str, tag: &str) -> bool {
    line.len() >= 5
        && line
            .get(..2)
            .is_some_and(|value| value.eq_ignore_ascii_case(tag))
        && line.as_bytes().get(2..4) == Some(b"  ")
        && line.as_bytes().get(4) == Some(&b'-')
}
fn tag_value(line: &str) -> &str {
    line.get(5..).unwrap_or_default()
}
fn append_value(target: &mut String, value: &str, context: &str) -> Result<()> {
    if !target.is_empty() {
        target.push(' ');
    }
    target.push_str(value);
    validate_field(target, context)
}
fn preview_text(value: &str) -> String {
    if value.len() <= MAX_RIS_PREVIEW {
        value.to_owned()
    } else {
        let mut end = MAX_RIS_PREVIEW;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
fn validate_field(value: &str, context: &str) -> Result<()> {
    if value.len() > MAX_RIS_FIELD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "{context} exceeds {MAX_RIS_FIELD_BYTES} bytes"
        )));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(Error::InvalidInput(format!(
            "{context} contains a control character"
        )));
    }
    Ok(())
}
fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
