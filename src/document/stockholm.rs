//! Bounded Stockholm 1.0 multiple-sequence-alignment preview.
//!
//! Sequence and annotation lines are treated as inert text.  The parser does
//! not contact Pfam/HMMER, resolve sequence identifiers, or run an aligner.

use std::collections::HashMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_STOCKHOLM_BYTES: u64 = 128 * 1024 * 1024;
const MAX_STOCKHOLM_LINES: usize = 2_000_000;
const MAX_STOCKHOLM_LINE_BYTES: usize = 1 << 20;
const MAX_STOCKHOLM_ALIGNMENTS: usize = 100_000;
const MAX_STOCKHOLM_SEQUENCES: usize = 500_000;
const MAX_STOCKHOLM_NAME_BYTES: usize = 4 * 1024;
const MAX_STOCKHOLM_SEQUENCE_BYTES: usize = 10_000_000;
const MAX_STOCKHOLM_TOTAL_SEQUENCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_STOCKHOLM_PREVIEW: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.eq_ignore_ascii_case("# STOCKHOLM 1.0"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_STOCKHOLM_BYTES),
        "Stockholm input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Stockholm input must be UTF-8/ASCII: {error}"))
    })?;
    let (mut table, warnings) = parse_stockholm(&text)?;
    let mut page_sink = StockholmPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "stockholm", options, &mut page_sink)?;
    Ok(warnings)
}

struct StockholmPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for StockholmPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "stockholm".into();
        page.title = "Stockholm multiple alignments".into();
        page.description =
            "Stockholm sequence and annotation text is displayed inertly without external lookup"
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

fn parse_stockholm(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_STOCKHOLM_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Stockholm input exceeds {MAX_STOCKHOLM_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_STOCKHOLM_LINES {
        return Err(Error::LimitExceeded(format!(
            "Stockholm input exceeds {MAX_STOCKHOLM_LINES} lines"
        )));
    }

    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut accumulators = Vec::<SequenceAccumulator>::new();
    let mut indices = HashMap::<String, usize>::new();
    let mut alignment_number = 0usize;
    let mut saw_header = false;
    let mut metadata = false;
    let mut total_sequence_bytes = 0usize;

    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_STOCKHOLM_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Stockholm line {} exceeds {MAX_STOCKHOLM_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        if let Some(version) = line.strip_prefix("# STOCKHOLM ") {
            if !version.starts_with("1.") {
                return Err(Error::InvalidInput(format!(
                    "Stockholm line {} has unsupported version {version:?}",
                    line_number + 1
                )));
            }
            if saw_header && !accumulators.is_empty() {
                finalize_alignment(
                    &mut accumulators,
                    &mut indices,
                    &mut rows,
                    &mut total_sequence_bytes,
                    &mut warnings,
                    &mut alignment_number,
                )?;
                warnings.push(
                    "Stockholm alignment header appeared before //; the previous alignment was finalized"
                        .into(),
                );
            }
            saw_header = true;
            continue;
        }
        if line == "//" {
            if !saw_header {
                return Err(Error::InvalidInput(format!(
                    "Stockholm line {} terminator appeared before a header",
                    line_number + 1
                )));
            }
            if accumulators.is_empty() {
                return Err(Error::InvalidInput(format!(
                    "Stockholm alignment {} contains no sequence rows",
                    alignment_number + 1
                )));
            }
            finalize_alignment(
                &mut accumulators,
                &mut indices,
                &mut rows,
                &mut total_sequence_bytes,
                &mut warnings,
                &mut alignment_number,
            )?;
            saw_header = false;
            continue;
        }
        if line.starts_with('#') {
            if !saw_header {
                return Err(Error::InvalidInput(format!(
                    "Stockholm annotation on line {} appeared before a header",
                    line_number + 1
                )));
            }
            metadata = true;
            continue;
        }
        if !saw_header {
            return Err(Error::InvalidInput(format!(
                "Stockholm sequence row on line {} appeared before a header",
                line_number + 1
            )));
        }
        let mut fields = line.split_ascii_whitespace();
        let name = fields.next().unwrap_or_default();
        let sequence = fields.next().unwrap_or_default();
        if name.is_empty() || sequence.is_empty() || fields.next().is_some() {
            return Err(Error::InvalidInput(format!(
                "Stockholm sequence line {} must contain exactly a name and sequence",
                line_number + 1
            )));
        }
        if name.len() > MAX_STOCKHOLM_NAME_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Stockholm sequence name on line {} exceeds {MAX_STOCKHOLM_NAME_BYTES} bytes",
                line_number + 1
            )));
        }
        if sequence
            .bytes()
            .any(|byte| !(33..=126).contains(&byte) || byte == b'#')
        {
            return Err(Error::InvalidInput(format!(
                "Stockholm sequence on line {} contains a non-printable or '#' character",
                line_number + 1
            )));
        }
        let index = if let Some(index) = indices.get(name).copied() {
            index
        } else {
            if accumulators.len() >= MAX_STOCKHOLM_SEQUENCES {
                return Err(Error::LimitExceeded(format!(
                    "Stockholm alignment exceeds {MAX_STOCKHOLM_SEQUENCES} sequence rows"
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
        target.push_str(sequence);
        if target.len() > MAX_STOCKHOLM_SEQUENCE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Stockholm sequence {name:?} exceeds {MAX_STOCKHOLM_SEQUENCE_BYTES} bytes"
            )));
        }
    }

    if saw_header || !accumulators.is_empty() {
        if accumulators.is_empty() {
            return Err(Error::InvalidInput(
                "Stockholm final alignment contains no sequence rows".into(),
            ));
        }
        finalize_alignment(
            &mut accumulators,
            &mut indices,
            &mut rows,
            &mut total_sequence_bytes,
            &mut warnings,
            &mut alignment_number,
        )?;
    }
    if alignment_number == 0 {
        return Err(Error::InvalidInput(
            "Stockholm input contains no alignments".into(),
        ));
    }
    if metadata {
        warnings.push(
            "Stockholm GF/GS/GC/GR and comment annotation lines were ignored or kept inert".into(),
        );
    }
    let headers = ["Alignment", "Sequence", "Aligned length", "Preview"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let alignments = vec![
        TableAlign::Right,
        TableAlign::Left,
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

fn finalize_alignment(
    accumulators: &mut Vec<SequenceAccumulator>,
    indices: &mut HashMap<String, usize>,
    rows: &mut Vec<Vec<String>>,
    total_sequence_bytes: &mut usize,
    warnings: &mut Vec<String>,
    alignment_number: &mut usize,
) -> Result<()> {
    *alignment_number = alignment_number
        .checked_add(1)
        .ok_or_else(|| Error::LimitExceeded("Stockholm alignment count overflowed".into()))?;
    if *alignment_number > MAX_STOCKHOLM_ALIGNMENTS {
        return Err(Error::LimitExceeded(format!(
            "Stockholm exceeds {MAX_STOCKHOLM_ALIGNMENTS} alignments"
        )));
    }
    let aligned_length = accumulators
        .first()
        .map(|sequence| sequence.sequence.len())
        .unwrap_or(0);
    if aligned_length == 0 {
        return Err(Error::InvalidInput(format!(
            "Stockholm alignment {} has an empty sequence",
            *alignment_number
        )));
    }
    for accumulator in accumulators.iter() {
        if accumulator.sequence.len() != aligned_length {
            return Err(Error::InvalidInput(format!(
                "Stockholm alignment {} has inconsistent sequence lengths",
                *alignment_number
            )));
        }
        *total_sequence_bytes = total_sequence_bytes
            .checked_add(accumulator.sequence.len())
            .ok_or_else(|| {
                Error::LimitExceeded("Stockholm sequence byte count overflowed".into())
            })?;
        if *total_sequence_bytes > MAX_STOCKHOLM_TOTAL_SEQUENCE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Stockholm sequences exceed {MAX_STOCKHOLM_TOTAL_SEQUENCE_BYTES} bytes"
            )));
        }
        let preview = if accumulator.sequence.len() > MAX_STOCKHOLM_PREVIEW {
            format!("{}…", &accumulator.sequence[..MAX_STOCKHOLM_PREVIEW])
        } else {
            accumulator.sequence.clone()
        };
        rows.push(vec![
            alignment_number.to_string(),
            accumulator.name.clone(),
            aligned_length.to_string(),
            preview,
        ]);
    }
    accumulators.clear();
    indices.clear();
    if rows.len() > MAX_STOCKHOLM_SEQUENCES {
        return Err(Error::LimitExceeded(format!(
            "Stockholm exceeds {MAX_STOCKHOLM_SEQUENCES} rendered sequence rows"
        )));
    }
    if aligned_length > MAX_STOCKHOLM_PREVIEW {
        warnings.push(format!(
            "Stockholm sequence previews were truncated to {MAX_STOCKHOLM_PREVIEW} characters"
        ));
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
