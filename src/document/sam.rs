//! Bounded SAM (Sequence Alignment/Map) text alignment preview.
//!
//! Header lines, the 11 mandatory alignment columns, and optional tags are
//! rendered as inert table data. Reference lookup, CIGAR projection, genotype
//! interpretation, and external resource access are intentionally omitted.

use std::collections::HashSet;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_SAM_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SAM_LINES: usize = 2_000_000;
const MAX_SAM_LINE_BYTES: usize = 1 << 20;
const MAX_SAM_RECORDS: usize = 100_000;
const MAX_SAM_COLUMNS: usize = 256;
const MAX_SAM_VALUE_BYTES: usize = 64 * 1024;
const MAX_SAM_CELLS: usize = 2_000_000;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    if text
        .lines()
        .any(|line| line.starts_with("@HD\t") || line.starts_with("@SQ\t"))
    {
        return true;
    }
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('@'))
        .find_map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            (fields.len() >= 11
                && fields[1].parse::<u16>().is_ok()
                && fields[3].parse::<u64>().is_ok())
            .then_some(())
        })
        .is_some()
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SAM_BYTES),
        "SAM input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("SAM input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse_sam(&text)?;
    let mut page_sink = SamPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "sam", options, &mut page_sink)?;
    Ok(warnings)
}

struct SamPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for SamPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "sam".into();
        page.title = "SAM alignment records".into();
        page.description = "SAM alignment columns are displayed inertly; no reference or alignment analysis is performed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse_sam(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_SAM_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SAM input exceeds {MAX_SAM_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_SAM_LINES {
        return Err(Error::LimitExceeded(format!(
            "SAM input exceeds {MAX_SAM_LINES} lines"
        )));
    }
    let mut header = vec![
        "QNAME", "FLAG", "RNAME", "POS", "MAPQ", "CIGAR", "RNEXT", "PNEXT", "TLEN", "SEQ", "QUAL",
        "OPTIONAL",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut metadata = false;
    let mut records = 0usize;
    let mut cells = 0usize;
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_SAM_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "SAM line {} exceeds {MAX_SAM_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.starts_with('@') {
            metadata = true;
            if line.starts_with("@CO\t") && line.to_ascii_lowercase().contains("http") {
                warnings.push(
                    "SAM header comments containing URLs were retained as inert metadata".into(),
                );
            }
            continue;
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() < 11 || fields.len() > MAX_SAM_COLUMNS {
            return Err(Error::InvalidInput(format!(
                "SAM alignment line {} must contain 11–{} tab-separated columns",
                line_number + 1,
                MAX_SAM_COLUMNS
            )));
        }
        records += 1;
        if records > MAX_SAM_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "SAM exceeds {MAX_SAM_RECORDS} alignment records"
            )));
        }
        if fields[0].is_empty() || fields[0].chars().any(char::is_whitespace) {
            return Err(Error::InvalidInput(format!(
                "SAM line {} QNAME is invalid",
                line_number + 1
            )));
        }
        let _flag = fields[1].parse::<u16>().map_err(|_| {
            Error::InvalidInput(format!("SAM line {} FLAG is invalid", line_number + 1))
        })?;
        let pos = fields[3].parse::<u64>().map_err(|_| {
            Error::InvalidInput(format!("SAM line {} POS is invalid", line_number + 1))
        })?;
        if fields[2] != "*" && fields[2].is_empty() {
            return Err(Error::InvalidInput(format!(
                "SAM line {} RNAME is invalid",
                line_number + 1
            )));
        }
        if fields[4] != "*" {
            let mapq = fields[4].parse::<u16>().map_err(|_| {
                Error::InvalidInput(format!("SAM line {} MAPQ is invalid", line_number + 1))
            })?;
            if mapq > 255 {
                return Err(Error::InvalidInput(format!(
                    "SAM line {} MAPQ exceeds 255",
                    line_number + 1
                )));
            }
        }
        validate_cigar(fields[5], line_number + 1)?;
        if fields[7] != "*" {
            let _ = fields[7].parse::<u64>().map_err(|_| {
                Error::InvalidInput(format!("SAM line {} PNEXT is invalid", line_number + 1))
            })?;
        }
        if fields[8] != "*" {
            let _ = fields[8].parse::<i64>().map_err(|_| {
                Error::InvalidInput(format!("SAM line {} TLEN is invalid", line_number + 1))
            })?;
        }
        if fields[9] != "*" {
            validate_sequence(fields[9], "SAM SEQ", line_number + 1)?;
            if fields[10] != "*" && fields[10].len() != fields[9].len() {
                return Err(Error::InvalidInput(format!(
                    "SAM line {} QUAL length does not match SEQ",
                    line_number + 1
                )));
            }
        } else if fields[10] != "*" {
            return Err(Error::InvalidInput(format!(
                "SAM line {} QUAL is present while SEQ is '* '",
                line_number + 1
            )));
        }
        let mut seen_tags = HashSet::new();
        for tag in fields.iter().skip(11) {
            validate_tag(tag, line_number + 1, &mut seen_tags)?;
        }
        for field in &fields {
            if field.len() > MAX_SAM_VALUE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "SAM line {} field exceeds {MAX_SAM_VALUE_BYTES} bytes",
                    line_number + 1
                )));
            }
            if field.chars().any(char::is_control) {
                return Err(Error::InvalidInput(format!(
                    "SAM line {} contains a control character",
                    line_number + 1
                )));
            }
        }
        cells = cells
            .checked_add(fields.len())
            .ok_or_else(|| Error::LimitExceeded("SAM cell count overflowed".into()))?;
        if cells > MAX_SAM_CELLS {
            return Err(Error::LimitExceeded(format!(
                "SAM exceeds {MAX_SAM_CELLS} cells"
            )));
        }
        let optional = fields
            .iter()
            .skip(11)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let mut row = fields[..11]
            .iter()
            .map(|field| (*field).to_owned())
            .collect::<Vec<_>>();
        row.push(optional);
        rows.push(row);
        let _ = pos;
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "SAM contains no alignment records".into(),
        ));
    }
    if metadata {
        warnings.push("SAM header metadata was ignored for rendering".into());
    }
    let alignments = (0..header.len())
        .map(|index| {
            if matches!(index, 1 | 3 | 4 | 7 | 8) {
                TableAlign::Right
            } else {
                TableAlign::Left
            }
        })
        .collect();
    Ok((
        TableData {
            headers: std::mem::take(&mut header),
            rows,
            alignments,
            raw_source: String::new(),
        },
        dedup_warnings(warnings),
    ))
}

fn validate_sequence(sequence: &str, context: &str, line: usize) -> Result<()> {
    if sequence
        .bytes()
        .any(|byte| !byte.is_ascii_alphabetic() && byte != b'=' && byte != b'.')
    {
        return Err(Error::InvalidInput(format!(
            "{context} on line {line} contains an invalid base"
        )));
    }
    Ok(())
}

fn validate_cigar(cigar: &str, line: usize) -> Result<()> {
    if cigar == "*" {
        return Ok(());
    }
    let mut digits = 0usize;
    let mut operators = 0usize;
    for byte in cigar.bytes() {
        if byte.is_ascii_digit() {
            digits += 1;
            continue;
        }
        if digits == 0
            || !matches!(
                byte,
                b'M' | b'I' | b'D' | b'N' | b'S' | b'H' | b'P' | b'=' | b'X'
            )
        {
            return Err(Error::InvalidInput(format!(
                "SAM line {line} CIGAR is invalid"
            )));
        }
        operators += 1;
        if operators > 65_535 {
            return Err(Error::LimitExceeded(format!(
                "SAM line {line} CIGAR has too many operators"
            )));
        }
        digits = 0;
    }
    if digits != 0 || operators == 0 {
        return Err(Error::InvalidInput(format!(
            "SAM line {line} CIGAR is incomplete"
        )));
    }
    Ok(())
}

fn validate_tag(tag: &str, line: usize, seen: &mut HashSet<String>) -> Result<()> {
    let mut parts = tag.splitn(3, ':');
    let name = parts.next().unwrap_or_default();
    let kind = parts.next().unwrap_or_default();
    let value = parts.next().unwrap_or_default();
    if name.len() != 2
        || !name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        || !name
            .as_bytes()
            .get(1)
            .copied()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        || kind.len() != 1
        || value.is_empty()
        || !seen.insert(name.to_owned())
    {
        return Err(Error::InvalidInput(format!(
            "SAM line {line} optional tag is invalid or duplicated"
        )));
    }
    Ok(())
}

fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
