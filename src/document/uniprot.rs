//! Bounded UniProtKB/Swiss-Prot flat-file preview.
//!
//! UniProt annotations and protein sequences are displayed as inert text. No
//! accession, cross-reference, taxonomy, or functional database is resolved.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_UNIPROT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_UNIPROT_LINES: usize = 5_000_000;
const MAX_UNIPROT_LINE_BYTES: usize = 1 << 20;
const MAX_UNIPROT_RECORDS: usize = 100_000;
const MAX_UNIPROT_FEATURES: usize = 1_000_000;
const MAX_UNIPROT_SEQUENCE_BYTES: usize = 10_000_000;
const MAX_UNIPROT_TOTAL_SEQUENCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_UNIPROT_FIELD_BYTES: usize = 64 * 1024;
const MAX_UNIPROT_PREVIEW: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| {
            let upper = line.to_ascii_uppercase();
            upper.starts_with("ID   ")
                && (upper.contains("REVIEWED;") || upper.contains("UNREVIEWED;"))
        })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_UNIPROT_BYTES),
        "UniProt input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("UniProt input must be UTF-8/ASCII: {error}"))
    })?;
    let (mut table, warnings) = parse_uniprot(&text)?;
    let mut page_sink = UniprotPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "uniprot", options, &mut page_sink)?;
    Ok(warnings)
}

struct UniprotPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for UniprotPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "uniprot".into();
        page.title = "UniProtKB protein records".into();
        page.description =
            "UniProt annotations and sequence text are displayed inertly without external lookup"
                .into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Record {
    id: String,
    review: String,
    accession: String,
    protein_name: String,
    gene: String,
    organism: String,
    declared_length: String,
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
    ProteinName,
    Gene,
    Organism,
}

fn parse_uniprot(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_UNIPROT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "UniProt input exceeds {MAX_UNIPROT_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_UNIPROT_LINES {
        return Err(Error::LimitExceeded(format!(
            "UniProt input exceeds {MAX_UNIPROT_LINES} lines"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut current = None::<Record>;
    let mut record_number = 0usize;
    let mut total_sequence_bytes = 0usize;
    let mut ignored_metadata = false;

    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_UNIPROT_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "UniProt line {} exceeds {MAX_UNIPROT_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if looks_like_id_line(line) {
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
            current = Some(parse_id(line.get(5..).unwrap_or_default())?);
            continue;
        }
        let Some(record) = current.as_mut() else {
            return Err(Error::InvalidInput(format!(
                "UniProt line {} appeared before ID",
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
        if record.in_sequence {
            append_sequence(record, trimmed, line_number + 1)?;
            continue;
        }
        if let Some(value) = tag_value(line, "AC") {
            append_field(&mut record.accession, value, "UniProt accession")?;
            record.last_field = LastField::Accession;
        } else if let Some(value) = tag_value(line, "DE") {
            append_text(&mut record.protein_name, value, "UniProt protein name")?;
            record.last_field = LastField::ProteinName;
        } else if let Some(value) = tag_value(line, "GN") {
            append_text(&mut record.gene, value, "UniProt gene")?;
            record.last_field = LastField::Gene;
        } else if let Some(value) = tag_value(line, "OS") {
            append_text(&mut record.organism, value, "UniProt organism")?;
            record.last_field = LastField::Organism;
        } else if let Some(value) = tag_value(line, "SQ") {
            record.in_sequence = true;
            record.last_field = LastField::None;
            record.declared_length = sequence_declaration_length(value);
        } else if let Some(value) = line.strip_prefix("FT   ") {
            if is_feature_field(value) {
                record.feature_count = record.feature_count.checked_add(1).ok_or_else(|| {
                    Error::LimitExceeded("UniProt feature count overflowed".into())
                })?;
                if record.feature_count > MAX_UNIPROT_FEATURES {
                    return Err(Error::LimitExceeded(format!(
                        "UniProt record exceeds {MAX_UNIPROT_FEATURES} features"
                    )));
                }
            }
        } else if tag_value(line, "CC").is_some()
            || tag_value(line, "DR").is_some()
            || tag_value(line, "RX").is_some()
            || tag_value(line, "DT").is_some()
            || tag_value(line, "KW").is_some()
            || tag_value(line, "PE").is_some()
        {
            ignored_metadata = true;
            record.last_field = LastField::None;
        } else {
            // Keep unknown two-letter tags inert, but allow wrapped known fields.
            if line
                .as_bytes()
                .get(..5)
                .is_some_and(|prefix| prefix.iter().all(|byte| byte.is_ascii_whitespace()))
            {
                let continuation = line.get(5..).unwrap_or_default().trim();
                if !continuation.is_empty() {
                    match record.last_field {
                        LastField::Accession => {
                            append_field(&mut record.accession, continuation, "UniProt accession")?;
                        }
                        LastField::ProteinName => {
                            append_text(
                                &mut record.protein_name,
                                continuation,
                                "UniProt protein name",
                            )?;
                        }
                        LastField::Gene => {
                            append_text(&mut record.gene, continuation, "UniProt gene")?;
                        }
                        LastField::Organism => {
                            append_text(&mut record.organism, continuation, "UniProt organism")?;
                        }
                        LastField::None => {}
                    }
                }
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
        return Err(Error::InvalidInput(
            "UniProt input contains no records".into(),
        ));
    }
    if ignored_metadata {
        warnings.push(
            "UniProt cross-reference and descriptive metadata lines were ignored or kept inert"
                .into(),
        );
    }
    let headers = [
        "Record",
        "Entry",
        "Review",
        "Accession",
        "Protein name",
        "Gene",
        "Organism",
        "Declared length",
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
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Right,
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

fn looks_like_id_line(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    upper.starts_with("ID   ") && (upper.contains("REVIEWED;") || upper.contains("UNREVIEWED;"))
}

fn parse_id(value: &str) -> Result<Record> {
    let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
    let id = fields.first().copied().unwrap_or_default();
    if id.is_empty() {
        return Err(Error::InvalidInput(
            "UniProt ID line has no entry name".into(),
        ));
    }
    let upper = value.to_ascii_uppercase();
    let review = if upper.contains("UNREVIEWED;") {
        "Unreviewed"
    } else {
        "Reviewed"
    };
    let declared_length = fields
        .iter()
        .position(|field| field.eq_ignore_ascii_case("AA."))
        .and_then(|index| index.checked_sub(1))
        .and_then(|index| fields.get(index).copied())
        .unwrap_or_default();
    let record = Record {
        id: id.to_owned(),
        review: review.into(),
        declared_length: declared_length.to_owned(),
        ..Record::default()
    };
    validate_field(&record.id, "UniProt entry")?;
    validate_field(&record.declared_length, "UniProt declared length")?;
    Ok(record)
}

fn tag_value<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    line.get(..2)
        .filter(|value| value.eq_ignore_ascii_case(tag))
        .and_then(|_| line.get(2..))
        .map(str::trim)
}

fn sequence_declaration_length(value: &str) -> String {
    let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
    fields
        .windows(2)
        .find(|pair| pair[1].eq_ignore_ascii_case("AA;") || pair[1].eq_ignore_ascii_case("BP;"))
        .map(|pair| format!("{} {}", pair[0], pair[1]))
        .unwrap_or_default()
}

fn append_sequence(record: &mut Record, line: &str, line_number: usize) -> Result<()> {
    let mut fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let count = fields
        .pop()
        .ok_or_else(|| Error::InvalidInput(format!("UniProt SQ line {line_number} is empty")))?;
    count.parse::<u64>().map_err(|_| {
        Error::InvalidInput(format!(
            "UniProt SQ line {line_number} has an invalid cumulative count"
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
            "UniProt SQ line {line_number} contains a non-sequence character"
        )));
    }
    record.sequence.push_str(&sequence);
    if record.sequence.len() > MAX_UNIPROT_SEQUENCE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "UniProt SQ sequence exceeds {MAX_UNIPROT_SEQUENCE_BYTES} bytes"
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
        .ok_or_else(|| Error::LimitExceeded("UniProt record count overflowed".into()))?;
    if *record_number > MAX_UNIPROT_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "UniProt exceeds {MAX_UNIPROT_RECORDS} records"
        )));
    }
    if implicit_separator {
        warnings.push(format!(
            "UniProt record {} was not terminated by //; the next ID or EOF finalized it",
            *record_number
        ));
    }
    if record.sequence.is_empty() {
        warnings.push(format!(
            "UniProt record {} has no SQ sequence preview",
            *record_number
        ));
    } else if let Some(declared) = record
        .declared_length
        .split_ascii_whitespace()
        .find_map(|value| value.parse::<usize>().ok())
        && declared != record.sequence.len()
    {
        warnings.push(format!(
            "UniProt record {} sequence length {} differs from declared length {}",
            *record_number,
            record.sequence.len(),
            declared
        ));
    }
    *total_sequence_bytes = (*total_sequence_bytes)
        .checked_add(record.sequence.len())
        .ok_or_else(|| {
            Error::LimitExceeded("UniProt total sequence byte count overflowed".into())
        })?;
    if *total_sequence_bytes > MAX_UNIPROT_TOTAL_SEQUENCE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "UniProt sequences exceed {MAX_UNIPROT_TOTAL_SEQUENCE_BYTES} bytes"
        )));
    }
    let preview = if record.sequence.len() > MAX_UNIPROT_PREVIEW {
        format!("{}…", &record.sequence[..MAX_UNIPROT_PREVIEW])
    } else {
        record.sequence.clone()
    };
    rows.push(vec![
        record_number.to_string(),
        record.id,
        record.review,
        record.accession,
        record.protein_name,
        record.gene,
        record.organism,
        record.declared_length,
        record.feature_count.to_string(),
        record.sequence.len().to_string(),
        preview,
    ]);
    Ok(())
}

fn is_feature_field(value: &str) -> bool {
    value.get(..15).is_some_and(|field| {
        field
            .chars()
            .any(|character| !character.is_whitespace() && character != '/')
    })
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
fn append_text(target: &mut String, value: &str, context: &str) -> Result<()> {
    append_field(target, value, context)
}
fn validate_field(value: &str, context: &str) -> Result<()> {
    if value.len() > MAX_UNIPROT_FIELD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "{context} exceeds {MAX_UNIPROT_FIELD_BYTES} bytes"
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
