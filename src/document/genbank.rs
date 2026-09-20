//! Bounded GenBank flat-file record preview.
//!
//! The reader summarizes the fixed-column record structure without resolving
//! accessions, fetching references, evaluating feature locations, or analyzing
//! the ORIGIN sequence.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_GENBANK_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GENBANK_LINES: usize = 5_000_000;
const MAX_GENBANK_LINE_BYTES: usize = 1 << 20;
const MAX_GENBANK_RECORDS: usize = 100_000;
const MAX_GENBANK_FEATURES: usize = 1_000_000;
const MAX_GENBANK_SEQUENCE_BYTES: usize = 10_000_000;
const MAX_GENBANK_TOTAL_SEQUENCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_GENBANK_FIELD_BYTES: usize = 64 * 1024;
const MAX_GENBANK_PREVIEW: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| {
            line.eq_ignore_ascii_case("LOCUS") || line.to_ascii_uppercase().starts_with("LOCUS ")
        })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GENBANK_BYTES),
        "GenBank input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("GenBank input must be UTF-8/ASCII: {error}"))
    })?;
    let (mut table, warnings) = parse_genbank(&text)?;
    let mut page_sink = GenbankPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "genbank", options, &mut page_sink)?;
    Ok(warnings)
}

struct GenbankPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for GenbankPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "genbank".into();
        page.title = "GenBank records".into();
        page.description =
            "GenBank metadata and sequence text are displayed inertly without external lookup"
                .into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Record {
    locus: String,
    length: String,
    molecule: String,
    topology: String,
    accession: String,
    definition: String,
    feature_count: usize,
    sequence: String,
    in_features: bool,
    in_origin: bool,
    last_field: LastField,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum LastField {
    #[default]
    None,
    Definition,
    Accession,
}

fn parse_genbank(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_GENBANK_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GenBank input exceeds {MAX_GENBANK_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_GENBANK_LINES {
        return Err(Error::LimitExceeded(format!(
            "GenBank input exceeds {MAX_GENBANK_LINES} lines"
        )));
    }
    let mut records = Vec::<Vec<String>>::new();
    let mut warnings = Vec::new();
    let mut current = None::<Record>;
    let mut record_number = 0usize;
    let mut total_sequence_bytes = 0usize;

    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_GENBANK_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "GenBank line {} exceeds {MAX_GENBANK_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if line.to_ascii_uppercase().starts_with("LOCUS ") || trimmed.eq_ignore_ascii_case("LOCUS")
        {
            if let Some(record) = current.take() {
                finalize_record(
                    record,
                    &mut records,
                    &mut warnings,
                    &mut record_number,
                    &mut total_sequence_bytes,
                    true,
                )?;
            }
            let fields = trimmed.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.len() < 2 {
                return Err(Error::InvalidInput(format!(
                    "GenBank LOCUS line {} is missing a locus name",
                    line_number + 1
                )));
            }
            let record = Record {
                locus: fields[1].to_owned(),
                length: fields.get(2).copied().unwrap_or_default().to_owned(),
                molecule: fields.get(4).copied().unwrap_or_default().to_owned(),
                topology: fields.get(5).copied().unwrap_or_default().to_owned(),
                ..Record::default()
            };
            validate_field(&record.locus, "GenBank locus")?;
            validate_field(&record.length, "GenBank length")?;
            current = Some(record);
            continue;
        }
        let Some(record) = current.as_mut() else {
            if trimmed.starts_with("##") {
                warnings.push("GenBank preamble metadata was ignored".into());
                continue;
            }
            return Err(Error::InvalidInput(format!(
                "GenBank line {} appeared before LOCUS",
                line_number + 1
            )));
        };
        if trimmed == "//" {
            let record = current.take().expect("current record exists");
            finalize_record(
                record,
                &mut records,
                &mut warnings,
                &mut record_number,
                &mut total_sequence_bytes,
                false,
            )?;
            continue;
        }
        if line.starts_with("DEFINITION") {
            record.definition = line[12..].trim().to_owned();
            validate_field(&record.definition, "GenBank definition")?;
            record.last_field = LastField::Definition;
            continue;
        }
        if line.starts_with("ACCESSION") {
            record.accession = line[12..]
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            validate_field(&record.accession, "GenBank accession")?;
            record.last_field = LastField::Accession;
            continue;
        }
        if line.starts_with("FEATURES") {
            record.in_features = true;
            record.in_origin = false;
            record.last_field = LastField::None;
            continue;
        }
        if line.starts_with("ORIGIN") {
            record.in_origin = true;
            record.in_features = false;
            record.last_field = LastField::None;
            continue;
        }
        if record.in_origin {
            append_origin(record, trimmed, line_number + 1)?;
            continue;
        }
        if record.in_features {
            if is_feature_line(line) {
                record.feature_count = record.feature_count.checked_add(1).ok_or_else(|| {
                    Error::LimitExceeded("GenBank feature count overflowed".into())
                })?;
                if record.feature_count > MAX_GENBANK_FEATURES {
                    return Err(Error::LimitExceeded(format!(
                        "GenBank record exceeds {MAX_GENBANK_FEATURES} features"
                    )));
                }
            }
            continue;
        }
        if line
            .as_bytes()
            .get(..12)
            .is_some_and(|prefix| prefix.iter().all(|byte| byte.is_ascii_whitespace()))
        {
            let continuation = line.get(12..).unwrap_or_default().trim();
            if !continuation.is_empty() {
                match record.last_field {
                    LastField::Definition => {
                        record.definition.push(' ');
                        record.definition.push_str(continuation);
                        validate_field(&record.definition, "GenBank definition")?;
                    }
                    LastField::Accession => {
                        record.accession.push(' ');
                        record.accession.push_str(continuation);
                        validate_field(&record.accession, "GenBank accession")?;
                    }
                    LastField::None => {}
                }
            }
        } else {
            record.last_field = LastField::None;
        }
    }
    if let Some(record) = current.take() {
        finalize_record(
            record,
            &mut records,
            &mut warnings,
            &mut record_number,
            &mut total_sequence_bytes,
            true,
        )?;
    }
    if records.is_empty() {
        return Err(Error::InvalidInput(
            "GenBank input contains no records".into(),
        ));
    }
    let headers = [
        "Record",
        "Locus",
        "Length",
        "Molecule",
        "Topology",
        "Accession",
        "Definition",
        "Features",
        "Sequence length",
        "Origin preview",
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
            rows: records,
            alignments,
            raw_source: String::new(),
        },
        dedup_warnings(warnings),
    ))
}

fn finalize_record(
    record: Record,
    rows: &mut Vec<Vec<String>>,
    warnings: &mut Vec<String>,
    record_number: &mut usize,
    total_sequence_bytes: &mut usize,
    implicit_separator: bool,
) -> Result<()> {
    *record_number = record_number
        .checked_add(1)
        .ok_or_else(|| Error::LimitExceeded("GenBank record count overflowed".into()))?;
    if *record_number > MAX_GENBANK_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "GenBank exceeds {MAX_GENBANK_RECORDS} records"
        )));
    }
    if implicit_separator {
        warnings.push(format!(
            "GenBank record {} was not terminated by //; the next LOCUS or EOF finalized it",
            *record_number
        ));
    }
    if record.sequence.is_empty() {
        warnings.push(format!(
            "GenBank record {} has no ORIGIN sequence preview",
            *record_number
        ));
    } else if let Ok(declared_length) = record.length.parse::<usize>()
        && declared_length != record.sequence.len()
    {
        warnings.push(format!(
            "GenBank record {} ORIGIN length {} differs from LOCUS length {}",
            *record_number,
            record.sequence.len(),
            declared_length
        ));
    }
    *total_sequence_bytes = total_sequence_bytes
        .checked_add(record.sequence.len())
        .ok_or_else(|| {
            Error::LimitExceeded("GenBank total sequence byte count overflowed".into())
        })?;
    if *total_sequence_bytes > MAX_GENBANK_TOTAL_SEQUENCE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GenBank sequences exceed {MAX_GENBANK_TOTAL_SEQUENCE_BYTES} bytes"
        )));
    }
    let preview = if record.sequence.len() > MAX_GENBANK_PREVIEW {
        format!("{}…", &record.sequence[..MAX_GENBANK_PREVIEW])
    } else {
        record.sequence.clone()
    };
    rows.push(vec![
        record_number.to_string(),
        record.locus,
        record.length,
        record.molecule,
        record.topology,
        record.accession,
        record.definition,
        record.feature_count.to_string(),
        record.sequence.len().to_string(),
        preview,
    ]);
    Ok(())
}

fn append_origin(record: &mut Record, line: &str, line_number: usize) -> Result<()> {
    let mut fields = line.split_ascii_whitespace();
    let position = fields.next().ok_or_else(|| {
        Error::InvalidInput(format!("GenBank ORIGIN line {line_number} is empty"))
    })?;
    position.parse::<u64>().map_err(|_| {
        Error::InvalidInput(format!(
            "GenBank ORIGIN line {line_number} has an invalid position"
        ))
    })?;
    let sequence = fields.collect::<String>();
    if sequence
        .bytes()
        .any(|byte| !byte.is_ascii_alphabetic() && byte != b'-')
    {
        return Err(Error::InvalidInput(format!(
            "GenBank ORIGIN line {line_number} contains a non-sequence character"
        )));
    }
    record.sequence.push_str(&sequence);
    if record.sequence.len() > MAX_GENBANK_SEQUENCE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GenBank ORIGIN sequence exceeds {MAX_GENBANK_SEQUENCE_BYTES} bytes"
        )));
    }
    Ok(())
}

fn is_feature_line(line: &str) -> bool {
    if line.len() < 21 || !line.starts_with("     ") {
        return false;
    }
    line.get(5..21)
        .is_some_and(|field| field.chars().any(|character| !character.is_whitespace()))
}

fn validate_field(value: &str, context: &str) -> Result<()> {
    if value.len() > MAX_GENBANK_FIELD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "{context} exceeds {MAX_GENBANK_FIELD_BYTES} bytes"
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
