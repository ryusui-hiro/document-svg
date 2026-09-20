//! Bounded DIF (Data Interchange Format) spreadsheet preview.
//!
//! DIF stores a small header followed by two-line values grouped into BOT
//! (beginning-of-tuple) rows and an EOD marker.  Cached values are rendered as
//! inert table text; no formulas, links, macros, or external resources run.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_DIF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DIF_LINES: usize = 1_000_000;
const MAX_DIF_LINE_BYTES: usize = 1024 * 1024;
const MAX_DIF_ROWS: usize = 50_000;
const MAX_DIF_COLUMNS: usize = 256;
const MAX_DIF_CELLS: usize = 1_000_000;
const MAX_DIF_VALUE_BYTES: usize = 64 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    matches!(lines.next(), Some("TABLE")) && lines.take(16).any(|line| line == "DATA")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DIF_BYTES),
        "DIF input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("DIF input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse_dif(&text)?;
    let mut page_sink = DifPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "dif", options, &mut page_sink)?;
    Ok(warnings)
}

struct DifPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for DifPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "dif".into();
        page.title = "DIF spreadsheet".into();
        page.description = "DIF data interchange table with inert cached values".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse_dif(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_DIF_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DIF input exceeds {MAX_DIF_BYTES} bytes"
        )));
    }
    let raw_lines = text.lines().collect::<Vec<_>>();
    if raw_lines.len() > MAX_DIF_LINES {
        return Err(Error::LimitExceeded(format!(
            "DIF input exceeds {MAX_DIF_LINES} lines"
        )));
    }
    if !raw_lines
        .iter()
        .all(|line| line.len() <= MAX_DIF_LINE_BYTES)
    {
        return Err(Error::LimitExceeded(format!(
            "DIF line exceeds {MAX_DIF_LINE_BYTES} bytes"
        )));
    }
    let lines = raw_lines
        .iter()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    let mut index = 0usize;
    let mut saw_table = false;
    let mut saw_vectors = false;
    let mut saw_tuples = false;
    let mut vector_count = None;
    let mut tuple_count = None;
    let mut warnings = Vec::new();
    let mut unknown_header_seen = false;
    while index < lines.len() {
        let keyword = lines[index].trim();
        if keyword.eq_ignore_ascii_case("DATA") {
            index += 1;
            break;
        }
        if !keyword
            .chars()
            .all(|character| character.is_ascii_uppercase() || character == '_')
        {
            return Err(Error::InvalidInput(format!(
                "DIF header keyword {keyword:?} is invalid"
            )));
        }
        index += 1;
        let (pair, string) = read_chunk(&lines, &mut index, "header")?;
        match keyword {
            "TABLE" => saw_table = true,
            "VECTORS" => {
                saw_vectors = true;
                vector_count = Some(validate_dimension(pair.1, "vectors")?);
            }
            "TUPLES" => {
                saw_tuples = true;
                tuple_count = Some(validate_dimension(pair.1, "tuples")?);
            }
            _ => unknown_header_seen = true,
        }
        let _ = string;
    }
    if !saw_table || !saw_vectors || !saw_tuples || index == 0 {
        return Err(Error::InvalidInput(
            "DIF header must contain TABLE, VECTORS, TUPLES, and DATA".into(),
        ));
    }
    let _ = read_chunk(&lines, &mut index, "DATA preamble")?;
    let mut rows = Vec::<Vec<String>>::new();
    let mut current = None::<Vec<String>>;
    let mut saw_eod = false;
    let mut cells = 0usize;
    let mut numeric_columns = std::collections::HashSet::new();
    let mut unknown_directive_seen = false;
    while index < lines.len() {
        let (pair, string) = read_chunk(&lines, &mut index, "data value")?;
        match pair.0 {
            -1 => match string.as_str() {
                "BOT" => {
                    if let Some(row) = current.take() {
                        rows.push(row);
                    }
                    if rows.len() >= MAX_DIF_ROWS {
                        return Err(Error::LimitExceeded(format!(
                            "DIF exceeds {MAX_DIF_ROWS} rows"
                        )));
                    }
                    current = Some(Vec::new());
                }
                "EOD" => {
                    if let Some(row) = current.take() {
                        rows.push(row);
                    }
                    saw_eod = true;
                    break;
                }
                _other => unknown_directive_seen = true,
            },
            0 => {
                let rendered = match string.as_str() {
                    "NA" | "ERROR" => string,
                    "TRUE" => "true".into(),
                    "FALSE" => "false".into(),
                    _ => pair.1.to_string(),
                };
                append_cell(
                    &mut current,
                    rendered,
                    &mut cells,
                    &mut numeric_columns,
                    true,
                )?;
            }
            1 => append_cell(
                &mut current,
                string,
                &mut cells,
                &mut numeric_columns,
                false,
            )?,
            other => {
                return Err(Error::Unsupported(format!(
                    "DIF value type {other} is unsupported"
                )));
            }
        }
    }
    if !saw_eod {
        warnings.push("DIF EOD marker was missing; input ended after data".into());
    }
    if unknown_header_seen {
        warnings.push("DIF nonstandard header records were ignored".into());
    }
    if unknown_directive_seen {
        warnings.push("DIF nonstandard data directives were ignored".into());
    }
    if let Some(count) = tuple_count
        && rows.len() > count
    {
        warnings.push("DIF data rows exceeded the advisory TUPLES count".into());
    }
    if let Some(count) = vector_count
        && rows.iter().any(|row| row.len() > count)
    {
        warnings.push("DIF data columns exceeded the advisory VECTORS count".into());
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput("DIF contains no data tuples".into()));
    }
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let headers = rows.remove(0);
    let alignments = (0..width)
        .map(|column| {
            if numeric_columns.contains(&column) {
                TableAlign::Right
            } else {
                TableAlign::Left
            }
        })
        .collect();
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

fn append_cell(
    current: &mut Option<Vec<String>>,
    value: String,
    cells: &mut usize,
    numeric_columns: &mut std::collections::HashSet<usize>,
    numeric: bool,
) -> Result<()> {
    if value.len() > MAX_DIF_VALUE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DIF cell value exceeds {MAX_DIF_VALUE_BYTES} bytes"
        )));
    }
    let row = current
        .as_mut()
        .ok_or_else(|| Error::InvalidInput("DIF data value appears before BOT".into()))?;
    if row.len() >= MAX_DIF_COLUMNS {
        return Err(Error::LimitExceeded(format!(
            "DIF exceeds {MAX_DIF_COLUMNS} columns"
        )));
    }
    *cells = cells
        .checked_add(1)
        .ok_or_else(|| Error::LimitExceeded("DIF cell count overflowed".into()))?;
    if *cells > MAX_DIF_CELLS {
        return Err(Error::LimitExceeded(format!(
            "DIF exceeds {MAX_DIF_CELLS} cells"
        )));
    }
    if numeric {
        numeric_columns.insert(row.len());
    }
    row.push(value);
    Ok(())
}

fn read_chunk(lines: &[&str], index: &mut usize, context: &str) -> Result<((i32, f64), String)> {
    while *index < lines.len() && lines[*index].trim().is_empty() {
        *index += 1;
    }
    let pair_line = *lines
        .get(*index)
        .ok_or_else(|| Error::InvalidInput(format!("DIF {context} pair is missing")))?;
    *index += 1;
    let pair = pair_line.split_once(',').ok_or_else(|| {
        Error::InvalidInput(format!("DIF {context} pair {pair_line:?} is invalid"))
    })?;
    let kind = pair
        .0
        .trim()
        .parse::<i32>()
        .map_err(|_| Error::InvalidInput(format!("DIF {context} type is invalid")))?;
    let number = pair
        .1
        .trim()
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("DIF {context} numeric value is invalid")))?;
    if !number.is_finite() {
        return Err(Error::InvalidInput(format!(
            "DIF {context} value is non-finite"
        )));
    }
    let string_line = *lines
        .get(*index)
        .ok_or_else(|| Error::InvalidInput(format!("DIF {context} string is missing")))?;
    *index += 1;
    Ok(((kind, number), decode_string(string_line, context)?))
}

fn decode_string(line: &str, context: &str) -> Result<String> {
    let line = line.trim();
    let value = if line.starts_with('"') {
        if !line.ends_with('"') || line.len() < 2 {
            return Err(Error::InvalidInput(format!(
                "DIF {context} string quote is unterminated"
            )));
        }
        line[1..line.len() - 1].replace("\"\"", "\"")
    } else {
        line.to_owned()
    };
    if value.len() > MAX_DIF_VALUE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DIF {context} string exceeds {MAX_DIF_VALUE_BYTES} bytes"
        )));
    }
    Ok(value)
}

fn validate_dimension(value: f64, context: &str) -> Result<usize> {
    if value < 0.0 || value.fract() != 0.0 {
        return Err(Error::InvalidInput(format!(
            "DIF {context} count is invalid"
        )));
    }
    let count = value as usize;
    if context == "vectors" && count > MAX_DIF_COLUMNS
        || context == "tuples" && count > MAX_DIF_ROWS
    {
        return Err(Error::LimitExceeded(format!(
            "DIF {context} count exceeds configured limit"
        )));
    }
    Ok(count)
}

fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
