//! Bounded UCSC Multiple Alignment Format (MAF) preview.
//!
//! Alignment blocks and `s` sequence rows are shown as inert tabular records.
//! Optional `i`, `e`, `q` and `score` metadata are retained only as text; no
//! reference genome, coordinate lift, or alignment calculation is performed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_MAF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MAF_LINES: usize = 2_000_000;
const MAX_MAF_LINE_BYTES: usize = 1 << 20;
const MAX_MAF_BLOCKS: usize = 100_000;
const MAX_MAF_SEQUENCE_ROWS: usize = 500_000;
const MAX_MAF_SEQUENCE_BYTES: usize = 10_000_000;
const MAX_MAF_TOTAL_SEQUENCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_MAF_PREVIEW: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .any(|line| line.to_ascii_lowercase().starts_with("##maf"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_MAF_BYTES),
        "MAF input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("MAF input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse_maf(&text)?;
    let mut page_sink = MafPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "maf", options, &mut page_sink)?;
    Ok(warnings)
}

struct MafPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for MafPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "maf".into();
        page.title = "MAF multiple alignments".into();
        page.description =
            "MAF alignment sequence rows are displayed inertly without reference lookup".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse_maf(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_MAF_BYTES {
        return Err(Error::LimitExceeded(format!(
            "MAF input exceeds {MAX_MAF_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_MAF_LINES {
        return Err(Error::LimitExceeded(format!(
            "MAF input exceeds {MAX_MAF_LINES} lines"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut block = 0usize;
    let mut current_score = String::new();
    let mut in_block = false;
    let mut metadata = false;
    let mut total_sequence_bytes = 0usize;
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_MAF_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "MAF line {} exceeds {MAX_MAF_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            in_block = false;
            current_score.clear();
            continue;
        }
        if trimmed.starts_with("##") || trimmed.starts_with('#') || trimmed.starts_with("track ") {
            metadata = true;
            continue;
        }
        let fields = trimmed.split_ascii_whitespace().collect::<Vec<_>>();
        let kind = fields.first().copied().unwrap_or_default();
        match kind {
            "a" => {
                if in_block {
                    warnings.push("MAF block began before the previous block separator; rows were kept in source order".into());
                }
                block += 1;
                if block > MAX_MAF_BLOCKS {
                    return Err(Error::LimitExceeded(format!(
                        "MAF exceeds {MAX_MAF_BLOCKS} alignment blocks"
                    )));
                }
                current_score = fields
                    .iter()
                    .find_map(|field| field.strip_prefix("score="))
                    .unwrap_or_default()
                    .to_owned();
                in_block = true;
            }
            "s" => {
                if !in_block || fields.len() != 7 {
                    return Err(Error::InvalidInput(format!(
                        "MAF sequence line {} must contain seven fields inside an alignment block",
                        line_number + 1
                    )));
                }
                if rows.len() >= MAX_MAF_SEQUENCE_ROWS {
                    return Err(Error::LimitExceeded(format!(
                        "MAF exceeds {MAX_MAF_SEQUENCE_ROWS} sequence rows"
                    )));
                }
                let start = fields[2].parse::<u64>().map_err(|_| {
                    Error::InvalidInput(format!("MAF line {} start is invalid", line_number + 1))
                })?;
                let size = fields[3].parse::<u64>().map_err(|_| {
                    Error::InvalidInput(format!("MAF line {} size is invalid", line_number + 1))
                })?;
                let source_size = fields[5].parse::<u64>().map_err(|_| {
                    Error::InvalidInput(format!(
                        "MAF line {} source size is invalid",
                        line_number + 1
                    ))
                })?;
                if fields[4] != "+" && fields[4] != "-" {
                    return Err(Error::InvalidInput(format!(
                        "MAF line {} strand is invalid",
                        line_number + 1
                    )));
                }
                let sequence = fields[6];
                if sequence.len() > MAX_MAF_SEQUENCE_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "MAF line {} sequence exceeds {MAX_MAF_SEQUENCE_BYTES} bytes",
                        line_number + 1
                    )));
                }
                if sequence
                    .bytes()
                    .any(|byte| !byte.is_ascii_alphabetic() && byte != b'-' && byte != b'.')
                {
                    return Err(Error::InvalidInput(format!(
                        "MAF line {} sequence contains an invalid character",
                        line_number + 1
                    )));
                }
                let nongap = sequence.bytes().filter(|byte| *byte != b'-').count() as u64;
                if nongap != size {
                    warnings.push(format!(
                        "MAF line {} ungapped sequence length differs from declared size",
                        line_number + 1
                    ));
                }
                total_sequence_bytes = total_sequence_bytes
                    .checked_add(sequence.len())
                    .ok_or_else(|| {
                        Error::LimitExceeded("MAF sequence byte count overflowed".into())
                    })?;
                if total_sequence_bytes > MAX_MAF_TOTAL_SEQUENCE_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "MAF sequence data exceeds {MAX_MAF_TOTAL_SEQUENCE_BYTES} bytes"
                    )));
                }
                let preview = if sequence.len() > MAX_MAF_PREVIEW {
                    format!("{}…", &sequence[..MAX_MAF_PREVIEW])
                } else {
                    sequence.to_owned()
                };
                rows.push(vec![
                    block.to_string(),
                    fields[1].to_owned(),
                    start.to_string(),
                    size.to_string(),
                    fields[4].to_owned(),
                    source_size.to_string(),
                    current_score.clone(),
                    preview,
                ]);
            }
            "i" | "e" | "q" => {
                metadata = true;
            }
            _ => {
                metadata = true;
            }
        }
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput("MAF contains no sequence rows".into()));
    }
    if metadata {
        warnings.push(
            "MAF comments and optional block/quality metadata were ignored or kept inert".into(),
        );
    }
    let headers = [
        "Block",
        "Source",
        "Start",
        "Size",
        "Strand",
        "Source size",
        "Score",
        "Sequence",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let alignments = vec![
        TableAlign::Right,
        TableAlign::Left,
        TableAlign::Right,
        TableAlign::Right,
        TableAlign::Center,
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

fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
