//! Bounded UCSC BED and bedGraph interval-table preview.
//!
//! Coordinates remain inert genome intervals; no reference genome, remote
//! track, URL, or sequence is resolved. BED12 block lists are checked for
//! consistency while bedGraph values are kept as plain numeric text.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_BED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BED_LINES: usize = 2_000_000;
const MAX_BED_LINE_BYTES: usize = 1 << 20;
const MAX_BED_FEATURES: usize = 100_000;
const MAX_BED_COLUMNS: usize = 12;
const MAX_BED_CELLS: usize = 1_200_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BedFormat {
    Bed,
    BedGraph,
}

pub(crate) fn looks_like_bedgraph_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    if lower
        .lines()
        .any(|line| line.trim_start().starts_with("track") && line.contains("type=bedgraph"))
    {
        return true;
    }
    first_interval_line(text)
        .is_some_and(|fields| fields.len() == 4 && fields[3].parse::<f64>().is_ok())
}

pub(crate) fn looks_like_bed_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    first_interval_line(text).is_some_and(|fields| {
        fields.len() >= 3
            && fields.len() <= 12
            && fields[1].parse::<u64>().is_ok()
            && fields[2].parse::<u64>().is_ok()
    })
}

fn first_interval_line(text: &str) -> Option<Vec<&str>> {
    text.lines().map(str::trim).find_map(|line| {
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with("track")
            || line.starts_with("browser")
        {
            return None;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() >= 3
            && fields.first().is_some_and(|field| {
                field.starts_with("chr")
                    || field.starts_with("scaffold")
                    || field.starts_with("contig")
                    || field.starts_with("NC_")
            })
        {
            Some(fields)
        } else {
            None
        }
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    format: BedFormat,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_BED_BYTES),
        "BED input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("BED input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse(&text, format)?;
    let format_name = match format {
        BedFormat::Bed => "bed",
        BedFormat::BedGraph => "bedgraph",
    };
    let mut page_sink = BedPageSink {
        inner: sink,
        format_name,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, format_name, options, &mut page_sink)?;
    Ok(warnings)
}

struct BedPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    format_name: &'static str,
    warnings: &'a [String],
}
impl PageConsumer for BedPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = self.format_name.into();
        page.title = format!(
            "{} interval annotations",
            self.format_name.to_ascii_uppercase()
        );
        page.description =
            "Genome intervals and values are displayed inertly; no reference data is loaded".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse(text: &str, format: BedFormat) -> Result<(TableData, Vec<String>)> {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_BED_LINES {
        return Err(Error::LimitExceeded(format!(
            "BED input exceeds {MAX_BED_LINES} lines"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut directives = false;
    let mut cells = 0usize;
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_BED_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "BED line {} exceeds {MAX_BED_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("track") || line.starts_with("browser") {
            directives = true;
            continue;
        }
        let fields = line.split_whitespace().collect::<Vec<_>>();
        match format {
            BedFormat::BedGraph => {
                if fields.len() != 4 {
                    return Err(Error::InvalidInput(format!(
                        "bedGraph line {} must contain four columns",
                        line_number + 1
                    )));
                }
                let (start, end) =
                    validate_interval(fields[0], fields[1], fields[2], line_number + 1)?;
                let value = fields[3].parse::<f64>().map_err(|_| {
                    Error::InvalidInput(format!(
                        "bedGraph line {} has invalid dataValue",
                        line_number + 1
                    ))
                })?;
                if !value.is_finite() {
                    return Err(Error::InvalidInput(
                        "bedGraph dataValue is non-finite".into(),
                    ));
                }
                let _ = (start, end);
                rows.push(fields.into_iter().map(str::to_owned).collect::<Vec<_>>());
            }
            BedFormat::Bed => {
                if fields.len() < 3 || fields.len() > MAX_BED_COLUMNS {
                    return Err(Error::InvalidInput(format!(
                        "BED line {} must contain 3–12 columns",
                        line_number + 1
                    )));
                }
                let (start, end) =
                    validate_interval(fields[0], fields[1], fields[2], line_number + 1)?;
                if let Some(score) = fields.get(4)
                    && *score != "."
                {
                    let value = score.parse::<u16>().map_err(|_| {
                        Error::InvalidInput(format!(
                            "BED line {} has invalid score",
                            line_number + 1
                        ))
                    })?;
                    if value > 1000 {
                        return Err(Error::InvalidInput(format!(
                            "BED line {} score exceeds 1000",
                            line_number + 1
                        )));
                    }
                }
                if let Some(strand) = fields.get(5)
                    && !matches!(*strand, "+" | "-" | "." | "?")
                {
                    return Err(Error::InvalidInput(format!(
                        "BED line {} has invalid strand",
                        line_number + 1
                    )));
                }
                for index in [6usize, 7] {
                    if let Some(value) = fields.get(index)
                        && *value != "."
                    {
                        let _ = parse_u64(value, "BED thick coordinate", line_number + 1)?;
                    }
                }
                if let Some(rgb) = fields.get(8)
                    && *rgb != "."
                {
                    validate_rgb(rgb, line_number + 1)?;
                }
                if fields.len() >= 12 {
                    let count = parse_u64(fields[9], "BED blockCount", line_number + 1)? as usize;
                    let sizes = parse_list(fields[10], "BED blockSizes", line_number + 1)?;
                    let starts = parse_list(fields[11], "BED blockStarts", line_number + 1)?;
                    if count == 0 || count != sizes.len() || count != starts.len() {
                        return Err(Error::InvalidInput(format!(
                            "BED line {} blockCount does not match block lists",
                            line_number + 1
                        )));
                    }
                    if starts.first().copied() != Some(0) {
                        return Err(Error::InvalidInput(format!(
                            "BED line {} first blockStart must be 0",
                            line_number + 1
                        )));
                    }
                    let interval_span =
                        usize::try_from(end.saturating_sub(start)).map_err(|_| {
                            Error::LimitExceeded(format!(
                                "BED line {} interval exceeds address space",
                                line_number + 1
                            ))
                        })?;
                    for (block_start, block_size) in starts.iter().zip(&sizes) {
                        let block_end = block_start.checked_add(*block_size).ok_or_else(|| {
                            Error::LimitExceeded("BED block coordinate overflowed".into())
                        })?;
                        if block_end > interval_span {
                            return Err(Error::InvalidInput(format!(
                                "BED line {} block exceeds interval",
                                line_number + 1
                            )));
                        }
                        if *block_size == 0 {
                            return Err(Error::InvalidInput(format!(
                                "BED line {} block has zero size",
                                line_number + 1
                            )));
                        }
                    }
                }
                rows.push(fields.into_iter().map(str::to_owned).collect::<Vec<_>>());
            }
        }
        if rows.len() > MAX_BED_FEATURES {
            return Err(Error::LimitExceeded(format!(
                "BED input exceeds {MAX_BED_FEATURES} features"
            )));
        }
        cells = cells
            .checked_add(rows.last().map(Vec::len).unwrap_or(0))
            .ok_or_else(|| Error::LimitExceeded("BED cell count overflowed".into()))?;
        if cells > MAX_BED_CELLS {
            return Err(Error::LimitExceeded(format!(
                "BED input exceeds {MAX_BED_CELLS} cells"
            )));
        }
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "BED input contains no interval rows".into(),
        ));
    }
    if directives {
        warnings.push("BED track/browser directives were ignored".into());
    }
    let headers = match format {
        BedFormat::BedGraph => vec!["chrom", "chromStart", "chromEnd", "dataValue"],
        BedFormat::Bed => vec![
            "chrom",
            "chromStart",
            "chromEnd",
            "name",
            "score",
            "strand",
            "thickStart",
            "thickEnd",
            "itemRgb",
            "blockCount",
            "blockSizes",
            "blockStarts",
        ],
    };
    let alignments = (0..headers.len())
        .map(|index| {
            if matches!(index, 1 | 2 | 4 | 6 | 7 | 9) {
                TableAlign::Right
            } else {
                TableAlign::Left
            }
        })
        .collect();
    Ok((
        TableData {
            headers: headers.into_iter().map(str::to_owned).collect(),
            rows,
            alignments,
            raw_source: String::new(),
        },
        warnings,
    ))
}

fn validate_interval(chrom: &str, start: &str, end: &str, line: usize) -> Result<(u64, u64)> {
    if chrom.is_empty() || chrom.contains('\t') {
        return Err(Error::InvalidInput(format!(
            "BED line {line} chromosome is empty"
        )));
    }
    let start = start
        .parse::<u64>()
        .map_err(|_| Error::InvalidInput(format!("BED line {line} chromStart is invalid")))?;
    let end = end
        .parse::<u64>()
        .map_err(|_| Error::InvalidInput(format!("BED line {line} chromEnd is invalid")))?;
    if end < start {
        return Err(Error::InvalidInput(format!(
            "BED line {line} chromEnd precedes chromStart"
        )));
    }
    Ok((start, end))
}
fn parse_u64(value: &str, context: &str, line: usize) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| Error::InvalidInput(format!("{context} on line {line} is invalid")))
}
fn parse_list(value: &str, context: &str, line: usize) -> Result<Vec<usize>> {
    value
        .trim_end_matches(',')
        .split(',')
        .filter(|part| !part.is_empty())
        .map(|part| {
            parse_u64(part, context, line).and_then(|number| {
                usize::try_from(number).map_err(|_| {
                    Error::LimitExceeded(format!("{context} on line {line} exceeds address space"))
                })
            })
        })
        .collect()
}
fn validate_rgb(value: &str, line: usize) -> Result<()> {
    let fields = value.split(',').collect::<Vec<_>>();
    if fields.len() != 3 {
        return Err(Error::InvalidInput(format!(
            "BED line {line} itemRgb is invalid"
        )));
    }
    for field in fields {
        if field.parse::<u16>().ok().is_none_or(|number| number > 255) {
            return Err(Error::InvalidInput(format!(
                "BED line {line} itemRgb is outside 0..255"
            )));
        }
    }
    Ok(())
}
