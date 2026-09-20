//! Bounded CLUSTAL multiple-sequence-alignment preview.
//!
//! CLUSTAL output is a block-oriented text format. Sequence fragments are
//! concatenated by name while consensus and residue-number lines remain inert.
//! No aligner, profile builder, identifier lookup, or external resource is run.

use std::collections::HashMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_CLUSTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CLUSTAL_LINES: usize = 2_000_000;
const MAX_CLUSTAL_LINE_BYTES: usize = 1 << 20;
const MAX_CLUSTAL_SEQUENCES: usize = 500_000;
const MAX_CLUSTAL_NAME_BYTES: usize = 4 * 1024;
const MAX_CLUSTAL_SEQUENCE_BYTES: usize = 10_000_000;
const MAX_CLUSTAL_TOTAL_SEQUENCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLUSTAL_PREVIEW: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.to_ascii_uppercase().starts_with("CLUSTAL"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_CLUSTAL_BYTES),
        "CLUSTAL input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("CLUSTAL input must be UTF-8/ASCII: {error}"))
    })?;
    let (mut table, warnings) = parse_clustal(&text)?;
    let mut page_sink = ClustalPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "clustal", options, &mut page_sink)?;
    Ok(warnings)
}

struct ClustalPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for ClustalPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "clustal".into();
        page.title = "CLUSTAL multiple alignments".into();
        page.description =
            "CLUSTAL sequence and consensus text is displayed inertly without external lookup"
                .into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

struct SequenceAccumulator {
    name: String,
    sequence: String,
}

fn parse_clustal(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_CLUSTAL_BYTES {
        return Err(Error::LimitExceeded(format!(
            "CLUSTAL input exceeds {MAX_CLUSTAL_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_CLUSTAL_LINES {
        return Err(Error::LimitExceeded(format!(
            "CLUSTAL input exceeds {MAX_CLUSTAL_LINES} lines"
        )));
    }

    let mut saw_header = false;
    let mut metadata = false;
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut accumulators = Vec::<SequenceAccumulator>::new();
    let mut indices = HashMap::<String, usize>::new();
    let mut total_sequence_bytes = 0usize;

    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_CLUSTAL_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "CLUSTAL line {} exceeds {MAX_CLUSTAL_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !saw_header {
            if trimmed.to_ascii_uppercase().starts_with("CLUSTAL") {
                saw_header = true;
                continue;
            }
            return Err(Error::InvalidInput(format!(
                "CLUSTAL line {} must start with a CLUSTAL header",
                line_number + 1
            )));
        }
        if is_consensus_line(trimmed) {
            metadata = true;
            continue;
        }
        let fields = trimmed.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() < 2 || fields.len() > 3 {
            return Err(Error::InvalidInput(format!(
                "CLUSTAL sequence line {} must contain a name, segment, and optional residue count",
                line_number + 1
            )));
        }
        if fields.len() == 3 && fields[2].parse::<u64>().is_err() {
            return Err(Error::InvalidInput(format!(
                "CLUSTAL line {} has an invalid residue count",
                line_number + 1
            )));
        }
        if fields[0].parse::<u64>().is_ok() && fields[1].parse::<u64>().is_ok() {
            metadata = true;
            continue;
        }
        let name = fields[0];
        let segment = fields[1];
        if name.len() > MAX_CLUSTAL_NAME_BYTES {
            return Err(Error::LimitExceeded(format!(
                "CLUSTAL sequence name on line {} exceeds {MAX_CLUSTAL_NAME_BYTES} bytes",
                line_number + 1
            )));
        }
        if segment
            .bytes()
            .any(|byte| !(33..=126).contains(&byte) || byte == b'#')
        {
            return Err(Error::InvalidInput(format!(
                "CLUSTAL sequence on line {} contains a non-printable or '#' character",
                line_number + 1
            )));
        }
        let index = if let Some(index) = indices.get(name).copied() {
            index
        } else {
            if accumulators.len() >= MAX_CLUSTAL_SEQUENCES {
                return Err(Error::LimitExceeded(format!(
                    "CLUSTAL exceeds {MAX_CLUSTAL_SEQUENCES} sequence rows"
                )));
            }
            let index = accumulators.len();
            indices.insert(name.to_owned(), index);
            accumulators.push(SequenceAccumulator {
                name: name.to_owned(),
                sequence: String::new(),
            });
            index
        };
        let target = &mut accumulators[index].sequence;
        target.push_str(segment);
        if target.len() > MAX_CLUSTAL_SEQUENCE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "CLUSTAL sequence {name:?} exceeds {MAX_CLUSTAL_SEQUENCE_BYTES} bytes"
            )));
        }
    }

    if !saw_header || accumulators.is_empty() {
        return Err(Error::InvalidInput(
            "CLUSTAL input contains no sequence alignment".into(),
        ));
    }
    let aligned_length = accumulators[0].sequence.len();
    if aligned_length == 0 {
        return Err(Error::InvalidInput(
            "CLUSTAL alignment has empty sequences".into(),
        ));
    }
    for accumulator in &accumulators {
        if accumulator.sequence.len() != aligned_length {
            return Err(Error::InvalidInput(
                "CLUSTAL sequences have inconsistent aligned lengths".into(),
            ));
        }
        total_sequence_bytes = total_sequence_bytes
            .checked_add(accumulator.sequence.len())
            .ok_or_else(|| Error::LimitExceeded("CLUSTAL sequence byte count overflowed".into()))?;
        if total_sequence_bytes > MAX_CLUSTAL_TOTAL_SEQUENCE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "CLUSTAL sequences exceed {MAX_CLUSTAL_TOTAL_SEQUENCE_BYTES} bytes"
            )));
        }
        let preview = if accumulator.sequence.len() > MAX_CLUSTAL_PREVIEW {
            format!("{}…", &accumulator.sequence[..MAX_CLUSTAL_PREVIEW])
        } else {
            accumulator.sequence.clone()
        };
        rows.push(vec![
            accumulator.name.clone(),
            aligned_length.to_string(),
            preview,
        ]);
    }
    if metadata {
        warnings
            .push("CLUSTAL header numbering and consensus lines were ignored or kept inert".into());
    }
    let headers = ["Sequence", "Aligned length", "Preview"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let alignments = vec![TableAlign::Left, TableAlign::Right, TableAlign::Left];
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

fn is_consensus_line(line: &str) -> bool {
    !line.is_empty()
        && line
            .bytes()
            .all(|byte| matches!(byte, b'*' | b':' | b'.' | b'+' | b' '))
}

fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
