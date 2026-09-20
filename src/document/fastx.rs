//! Bounded FASTA and FASTQ sequence/quality previews.
//!
//! Sequence data is displayed as inert table text.  The converter never runs
//! aligners or interprets identifiers as paths; it reports length, GC ratio,
//! ambiguity and (for FASTQ) printable quality-code ranges.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_FASTX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FASTX_LINES: usize = 5_000_000;
const MAX_FASTX_LINE_BYTES: usize = 1 << 20;
const MAX_FASTX_RECORDS: usize = 100_000;
const MAX_FASTX_BASES: usize = 100_000_000;
const MAX_FASTX_RECORD_BASES: usize = 10_000_000;
const MAX_FASTX_PREVIEW: usize = 512;

pub(crate) fn looks_like_fasta_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with('>'))
}

pub(crate) fn looks_like_fastq_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let Some(header) = lines.next() else {
        return false;
    };
    let Some(sequence) = lines.next() else {
        return false;
    };
    let Some(plus) = lines.next() else {
        return false;
    };
    let Some(quality) = lines.next() else {
        return false;
    };
    header.starts_with('@')
        && !sequence.is_empty()
        && plus.starts_with('+')
        && quality.len() >= sequence.len()
}

pub(crate) fn convert_fasta(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert(path, options, sink, SequenceFormat::Fasta)
}

pub(crate) fn convert_fastq(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert(path, options, sink, SequenceFormat::Fastq)
}

#[derive(Clone, Copy)]
enum SequenceFormat {
    Fasta,
    Fastq,
}

fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    format: SequenceFormat,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_FASTX_BYTES),
        "sequence input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("sequence input must be UTF-8/ASCII: {error}"))
    })?;
    let (mut table, warnings) = match format {
        SequenceFormat::Fasta => parse_fasta(&text)?,
        SequenceFormat::Fastq => parse_fastq(&text)?,
    };
    let format_name = match format {
        SequenceFormat::Fasta => "fasta",
        SequenceFormat::Fastq => "fastq",
    };
    let mut page_sink = SequencePageSink {
        inner: sink,
        format_name,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, format_name, options, &mut page_sink)?;
    Ok(warnings)
}

struct SequencePageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    format_name: &'static str,
    warnings: &'a [String],
}

impl PageConsumer for SequencePageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = self.format_name.into();
        page.title = format!("{} sequence records", self.format_name.to_ascii_uppercase());
        page.description =
            "Sequence text is displayed inertly; no alignment or execution is performed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse_fasta(text: &str) -> Result<(TableData, Vec<String>)> {
    let lines = checked_lines(text)?;
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut id = None::<String>;
    let mut description = String::new();
    let mut sequence = String::new();
    let mut total_bases = 0usize;
    let mut truncated_preview = false;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('>') {
            if let Some(previous_id) = id.take() {
                let row = sequence_row(
                    &previous_id,
                    &description,
                    &sequence,
                    None,
                    &mut truncated_preview,
                )?;
                total_bases = total_bases
                    .checked_add(sequence.len())
                    .ok_or_else(|| Error::LimitExceeded("FASTA base count overflowed".into()))?;
                rows.push(row);
                sequence.clear();
            }
            if rows.len() >= MAX_FASTX_RECORDS {
                return Err(Error::LimitExceeded(format!(
                    "FASTA exceeds {MAX_FASTX_RECORDS} records"
                )));
            }
            let mut parts = header.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or_default();
            if name.is_empty() {
                return Err(Error::InvalidInput(
                    "FASTA definition line has no identifier".into(),
                ));
            }
            id = Some(name.to_owned());
            description = parts.next().unwrap_or_default().trim().to_owned();
        } else {
            if id.is_none() {
                return Err(Error::InvalidInput(
                    "FASTA sequence appears before a definition line".into(),
                ));
            }
            append_sequence(&mut sequence, line, "FASTA")?;
            if sequence.len() > MAX_FASTX_RECORD_BASES {
                return Err(Error::LimitExceeded(format!(
                    "FASTA record exceeds {MAX_FASTX_RECORD_BASES} bases"
                )));
            }
        }
    }
    if let Some(previous_id) = id {
        let row = sequence_row(
            &previous_id,
            &description,
            &sequence,
            None,
            &mut truncated_preview,
        )?;
        total_bases = total_bases
            .checked_add(sequence.len())
            .ok_or_else(|| Error::LimitExceeded("FASTA base count overflowed".into()))?;
        rows.push(row);
    }
    if total_bases > MAX_FASTX_BASES {
        return Err(Error::LimitExceeded(format!(
            "FASTA exceeds {MAX_FASTX_BASES} bases"
        )));
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "FASTA contains no sequence records".into(),
        ));
    }
    if truncated_preview {
        warnings.push(format!(
            "FASTA sequence text was truncated to {MAX_FASTX_PREVIEW} characters per record"
        ));
    }
    Ok((sequence_table(rows, SequenceFormat::Fasta), warnings))
}

fn parse_fastq(text: &str) -> Result<(TableData, Vec<String>)> {
    let lines = checked_lines(text)?;
    let mut index = 0usize;
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut total_bases = 0usize;
    let mut truncated_preview = false;
    while index < lines.len() {
        while index < lines.len() && lines[index].trim().is_empty() {
            index += 1;
        }
        if index >= lines.len() {
            break;
        }
        let header = lines[index].trim();
        if !header.starts_with('@') {
            return Err(Error::InvalidInput(format!(
                "FASTQ record {} must start with '@'",
                rows.len() + 1
            )));
        }
        index += 1;
        let mut sequence = String::new();
        while index < lines.len() && !lines[index].trim_start().starts_with('+') {
            append_sequence(&mut sequence, lines[index].trim(), "FASTQ")?;
            index += 1;
            if sequence.len() > MAX_FASTX_RECORD_BASES {
                return Err(Error::LimitExceeded(format!(
                    "FASTQ record exceeds {MAX_FASTX_RECORD_BASES} bases"
                )));
            }
        }
        if index >= lines.len() {
            return Err(Error::InvalidInput(format!(
                "FASTQ record {} is missing '+' line",
                rows.len() + 1
            )));
        }
        index += 1;
        let mut quality = String::new();
        while index < lines.len() && quality.len() < sequence.len() {
            let line = lines[index].trim();
            if !line.bytes().all(|byte| (33..=126).contains(&byte)) {
                return Err(Error::InvalidInput(
                    "FASTQ quality contains a non-printable character".into(),
                ));
            }
            quality.push_str(line);
            index += 1;
            if quality.len() > sequence.len() {
                return Err(Error::InvalidInput(format!(
                    "FASTQ quality length exceeds sequence length in record {}",
                    rows.len() + 1
                )));
            }
        }
        if quality.len() != sequence.len() {
            return Err(Error::InvalidInput(format!(
                "FASTQ quality length does not match sequence length in record {}",
                rows.len() + 1
            )));
        }
        let header = header.strip_prefix('@').unwrap_or_default();
        let mut parts = header.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or_default();
        if name.is_empty() {
            return Err(Error::InvalidInput(
                "FASTQ definition line has no identifier".into(),
            ));
        }
        let description = parts.next().unwrap_or_default().trim();
        let row = sequence_row(
            name,
            description,
            &sequence,
            Some(&quality),
            &mut truncated_preview,
        )?;
        total_bases = total_bases
            .checked_add(sequence.len())
            .ok_or_else(|| Error::LimitExceeded("FASTQ base count overflowed".into()))?;
        rows.push(row);
        if rows.len() > MAX_FASTX_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "FASTQ exceeds {MAX_FASTX_RECORDS} records"
            )));
        }
    }
    if total_bases > MAX_FASTX_BASES {
        return Err(Error::LimitExceeded(format!(
            "FASTQ exceeds {MAX_FASTX_BASES} bases"
        )));
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "FASTQ contains no sequence records".into(),
        ));
    }
    if truncated_preview {
        warnings.push(format!(
            "FASTQ sequence text was truncated to {MAX_FASTX_PREVIEW} characters per record"
        ));
    }
    Ok((sequence_table(rows, SequenceFormat::Fastq), warnings))
}

fn checked_lines(text: &str) -> Result<Vec<&str>> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_FASTX_LINES {
        return Err(Error::LimitExceeded(format!(
            "sequence input exceeds {MAX_FASTX_LINES} lines"
        )));
    }
    if lines.iter().any(|line| line.len() > MAX_FASTX_LINE_BYTES) {
        return Err(Error::LimitExceeded(format!(
            "sequence line exceeds {MAX_FASTX_LINE_BYTES} bytes"
        )));
    }
    Ok(lines)
}

fn append_sequence(sequence: &mut String, line: &str, format: &str) -> Result<()> {
    for byte in line.bytes() {
        if !byte.is_ascii_alphabetic() && !matches!(byte, b'-' | b'*' | b'?' | b'.') {
            return Err(Error::InvalidInput(format!(
                "{format} sequence contains invalid character {:?}",
                byte as char
            )));
        }
        sequence.push(byte as char);
    }
    Ok(())
}

fn sequence_row(
    id: &str,
    description: &str,
    sequence: &str,
    quality: Option<&str>,
    truncated: &mut bool,
) -> Result<Vec<String>> {
    if sequence.is_empty() {
        return Err(Error::InvalidInput(format!(
            "sequence record {id:?} is empty"
        )));
    }
    let length = sequence.len();
    let gc = sequence
        .bytes()
        .filter(|byte| matches!(byte.to_ascii_uppercase(), b'G' | b'C'))
        .count();
    let canonical = sequence.bytes().filter(u8::is_ascii_alphabetic).count();
    let gc_ratio = if canonical == 0 {
        "n/a".into()
    } else {
        format!("{:.2}%", gc as f64 * 100.0 / canonical as f64)
    };
    let ambiguous = sequence
        .bytes()
        .filter(|byte| !matches!(byte.to_ascii_uppercase(), b'A' | b'C' | b'G' | b'T' | b'U'))
        .count();
    let preview = if sequence.len() > MAX_FASTX_PREVIEW {
        *truncated = true;
        format!("{}…", &sequence[..MAX_FASTX_PREVIEW])
    } else {
        sequence.to_owned()
    };
    let mut row = vec![
        id.to_owned(),
        description.to_owned(),
        length.to_string(),
        gc_ratio,
        ambiguous.to_string(),
        preview,
    ];
    if let Some(quality) = quality {
        let min = quality.bytes().min().unwrap_or(0);
        let max = quality.bytes().max().unwrap_or(0);
        row.insert(5, format!("{}..{}", min, max));
    }
    Ok(row)
}

fn sequence_table(rows: Vec<Vec<String>>, format: SequenceFormat) -> TableData {
    let headers = match format {
        SequenceFormat::Fasta => vec!["ID", "Description", "Length", "GC", "Ambiguous", "Sequence"],
        SequenceFormat::Fastq => vec![
            "ID",
            "Description",
            "Length",
            "GC",
            "Ambiguous",
            "Quality ASCII range",
            "Sequence",
        ],
    };
    let width = headers.len();
    let mut alignments = vec![TableAlign::Left; width];
    for index in [2usize, 3, 4] {
        alignments[index] = TableAlign::Right;
    }
    TableData {
        headers: headers.into_iter().map(str::to_owned).collect(),
        rows,
        alignments,
        raw_source: String::new(),
    }
}
