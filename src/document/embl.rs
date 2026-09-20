//! Bounded EMBL-Bank flat-file record preview.
//!
//! ID/AC/DE/FT/SQ tags are summarized as inert text. No accession lookup,
//! feature evaluation, sequence analysis, or external resource access occurs.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_EMBL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_EMBL_LINES: usize = 5_000_000;
const MAX_EMBL_LINE_BYTES: usize = 1 << 20;
const MAX_EMBL_RECORDS: usize = 100_000;
const MAX_EMBL_FEATURES: usize = 1_000_000;
const MAX_EMBL_SEQUENCE_BYTES: usize = 10_000_000;
const MAX_EMBL_TOTAL_SEQUENCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_EMBL_FIELD_BYTES: usize = 64 * 1024;
const MAX_EMBL_PREVIEW: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.to_ascii_uppercase().starts_with("ID   "))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_EMBL_BYTES),
        "EMBL input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("EMBL input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse_embl(&text)?;
    let mut page_sink = EmblPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "embl", options, &mut page_sink)?;
    Ok(warnings)
}

struct EmblPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for EmblPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "embl".into();
        page.title = "EMBL-Bank records".into();
        page.description =
            "EMBL metadata and sequence text are displayed inertly without external lookup".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Record {
    id: String,
    declared_length: String,
    molecule: String,
    topology: String,
    accession: String,
    description: String,
    feature_count: usize,
    sequence: String,
    in_sequence: bool,
    last_field: LastField,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum LastField {
    #[default]
    None,
    Accession,
    Description,
}

fn parse_embl(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_EMBL_BYTES {
        return Err(Error::LimitExceeded(format!(
            "EMBL input exceeds {MAX_EMBL_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_EMBL_LINES {
        return Err(Error::LimitExceeded(format!(
            "EMBL input exceeds {MAX_EMBL_LINES} lines"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut current = None::<Record>;
    let mut record_number = 0usize;
    let mut total_sequence_bytes = 0usize;
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_EMBL_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "EMBL line {} exceeds {MAX_EMBL_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if line.to_ascii_uppercase().starts_with("ID   ") {
            if let Some(record) = current.take() {
                finalize_record(
                    record,
                    &mut rows,
                    &mut warnings,
                    &mut record_number,
                    &mut total_sequence_bytes,
                    true,
                )?;
            }
            current = Some(parse_id(line.strip_prefix("ID   ").unwrap_or_default())?);
            continue;
        }
        let Some(record) = current.as_mut() else {
            if line.starts_with("XX") || line.starts_with("CC") {
                warnings.push("EMBL preamble metadata was ignored".into());
                continue;
            }
            return Err(Error::InvalidInput(format!(
                "EMBL line {} appeared before ID",
                line_number + 1
            )));
        };
        if trimmed == "//" {
            let record = current.take().expect("current record exists");
            finalize_record(
                record,
                &mut rows,
                &mut warnings,
                &mut record_number,
                &mut total_sequence_bytes,
                false,
            )?;
            continue;
        }
        if let Some(value) = line.strip_prefix("DE   ") {
            record.description = value.trim().to_owned();
            validate_field(&record.description, "EMBL description")?;
            record.last_field = LastField::Description;
            continue;
        }
        if let Some(value) = line.strip_prefix("AC   ") {
            append_field(&mut record.accession, value, "EMBL accession")?;
            record.last_field = LastField::Accession;
            continue;
        }
        if let Some(value) = line.strip_prefix("FT   ") {
            if is_feature_field(value) {
                record.feature_count = record
                    .feature_count
                    .checked_add(1)
                    .ok_or_else(|| Error::LimitExceeded("EMBL feature count overflowed".into()))?;
                if record.feature_count > MAX_EMBL_FEATURES {
                    return Err(Error::LimitExceeded(format!(
                        "EMBL record exceeds {MAX_EMBL_FEATURES} features"
                    )));
                }
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("SQ   ") {
            record.in_sequence = true;
            record.last_field = LastField::None;
            record.declared_length = sequence_declaration_length(value);
            continue;
        }
        if record.in_sequence {
            append_sequence(record, trimmed, line_number + 1)?;
            continue;
        }
        if line
            .as_bytes()
            .get(..5)
            .is_some_and(|prefix| prefix.iter().all(|byte| byte.is_ascii_whitespace()))
        {
            let continuation = line.get(5..).unwrap_or_default().trim();
            if continuation.is_empty() {
                continue;
            }
            match record.last_field {
                LastField::Description => {
                    record.description.push(' ');
                    record.description.push_str(continuation);
                    validate_field(&record.description, "EMBL description")?;
                }
                LastField::Accession => {
                    append_field(&mut record.accession, continuation, "EMBL accession")?
                }
                LastField::None => {}
            }
        }
    }
    if let Some(record) = current.take() {
        finalize_record(
            record,
            &mut rows,
            &mut warnings,
            &mut record_number,
            &mut total_sequence_bytes,
            true,
        )?;
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput("EMBL input contains no records".into()));
    }
    let headers = [
        "Record",
        "ID",
        "Declared length",
        "Molecule",
        "Topology",
        "Accession",
        "Description",
        "Features",
        "Sequence length",
        "SQ preview",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let alignments = vec![
        TableAlign::Right,
        TableAlign::Left,
        TableAlign::Right,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Right,
        TableAlign::Right,
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

fn parse_id(value: &str) -> Result<Record> {
    let parts = value.split(';').map(str::trim).collect::<Vec<_>>();
    let id = parts.first().copied().unwrap_or_default();
    if id.is_empty() {
        return Err(Error::InvalidInput("EMBL ID line has no identifier".into()));
    }
    let mut record = Record {
        id: id.to_owned(),
        ..Record::default()
    };
    for part in parts.iter().skip(1) {
        let upper = part.to_ascii_uppercase();
        if upper == "LINEAR" || upper == "CIRCULAR" {
            record.topology = (*part).to_owned();
        }
        if upper.contains("DNA")
            || upper.contains("RNA")
            || upper.contains("PROTEIN")
            || upper.contains("PEPTIDE")
        {
            record.molecule = (*part).to_owned();
        }
        if upper.ends_with(" BP") || upper.ends_with(" AA") {
            record.declared_length = (*part).to_owned();
        }
    }
    validate_field(&record.id, "EMBL identifier")?;
    Ok(record)
}

fn sequence_declaration_length(value: &str) -> String {
    value
        .split(';')
        .map(str::trim)
        .find(|part| {
            let upper = part.to_ascii_uppercase();
            upper.starts_with("SEQUENCE ") && (upper.ends_with(" BP") || upper.ends_with(" AA"))
        })
        .unwrap_or_default()
        .to_owned()
}

fn append_sequence(record: &mut Record, line: &str, line_number: usize) -> Result<()> {
    let mut fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let count = fields
        .pop()
        .ok_or_else(|| Error::InvalidInput(format!("EMBL SQ line {line_number} is empty")))?;
    count.parse::<u64>().map_err(|_| {
        Error::InvalidInput(format!(
            "EMBL SQ line {line_number} has an invalid cumulative count"
        ))
    })?;
    if fields
        .first()
        .is_some_and(|value| value.parse::<u64>().is_ok())
    {
        fields.remove(0);
    }
    let sequence = fields.into_iter().collect::<String>();
    if sequence
        .bytes()
        .any(|byte| !byte.is_ascii_alphabetic() && byte != b'-' && byte != b'*')
    {
        return Err(Error::InvalidInput(format!(
            "EMBL SQ line {line_number} contains a non-sequence character"
        )));
    }
    record.sequence.push_str(&sequence);
    if record.sequence.len() > MAX_EMBL_SEQUENCE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "EMBL SQ sequence exceeds {MAX_EMBL_SEQUENCE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn finalize_record(
    record: Record,
    rows: &mut Vec<Vec<String>>,
    warnings: &mut Vec<String>,
    record_number: &mut usize,
    total_sequence_bytes: &mut usize,
    implicit_separator: bool,
) -> Result<()> {
    *record_number = (*record_number)
        .checked_add(1)
        .ok_or_else(|| Error::LimitExceeded("EMBL record count overflowed".into()))?;
    if *record_number > MAX_EMBL_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "EMBL exceeds {MAX_EMBL_RECORDS} records"
        )));
    }
    if implicit_separator {
        warnings.push(format!(
            "EMBL record {} was not terminated by //; the next ID or EOF finalized it",
            *record_number
        ));
    }
    if record.sequence.is_empty() {
        warnings.push(format!(
            "EMBL record {} has no SQ sequence preview",
            *record_number
        ));
    } else if let Some(declared) = record
        .declared_length
        .split_ascii_whitespace()
        .find_map(|value| value.parse::<usize>().ok())
        && declared != record.sequence.len()
    {
        warnings.push(format!(
            "EMBL record {} SQ length {} differs from declared length {}",
            *record_number,
            record.sequence.len(),
            declared
        ));
    }
    *total_sequence_bytes = (*total_sequence_bytes)
        .checked_add(record.sequence.len())
        .ok_or_else(|| Error::LimitExceeded("EMBL total sequence byte count overflowed".into()))?;
    if *total_sequence_bytes > MAX_EMBL_TOTAL_SEQUENCE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "EMBL sequences exceed {MAX_EMBL_TOTAL_SEQUENCE_BYTES} bytes"
        )));
    }
    let preview = if record.sequence.len() > MAX_EMBL_PREVIEW {
        format!("{}…", &record.sequence[..MAX_EMBL_PREVIEW])
    } else {
        record.sequence.clone()
    };
    rows.push(vec![
        record_number.to_string(),
        record.id,
        record.declared_length,
        record.molecule,
        record.topology,
        record.accession,
        record.description,
        record.feature_count.to_string(),
        record.sequence.len().to_string(),
        preview,
    ]);
    Ok(())
}

fn is_feature_field(value: &str) -> bool {
    value
        .get(..16)
        .is_some_and(|field| field.chars().any(|character| !character.is_whitespace()))
}

fn append_field(target: &mut String, value: &str, context: &str) -> Result<()> {
    let value = value.trim();
    if !value.is_empty() {
        if !target.is_empty() {
            target.push(' ');
        }
        target.push_str(value);
    }
    validate_field(target, context)
}

fn validate_field(value: &str, context: &str) -> Result<()> {
    if value.len() > MAX_EMBL_FIELD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "{context} exceeds {MAX_EMBL_FIELD_BYTES} bytes"
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
