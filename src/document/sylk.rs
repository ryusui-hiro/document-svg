//! Bounded SYLK (Symbolic Link) spreadsheet preview.
//!
//! SYLK is a line-oriented Excel/Multiplan interchange format.  This reader
//! keeps cell coordinates and cached values, renders them through the shared
//! paginated table view, and treats formulas and formatting as inert text.

use std::collections::HashSet;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_SYLK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SYLK_LINES: usize = 1_000_000;
const MAX_SYLK_LINE_BYTES: usize = 1024 * 1024;
const MAX_SYLK_ROWS: usize = 50_000;
const MAX_SYLK_COLUMNS: usize = 256;
const MAX_SYLK_CELLS: usize = 1_000_000;
const MAX_SYLK_VALUE_BYTES: usize = 64 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let Some(first) = lines.next() else {
        return false;
    };
    let first = first.trim_start_matches('\u{feff}').trim();
    first.starts_with("ID;") && lines.any(|line| line.trim_start().starts_with("C;"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SYLK_BYTES),
        "SYLK input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("SYLK input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse_sylk(&text)?;
    let mut page_sink = SylkPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "sylk", options, &mut page_sink)?;
    Ok(warnings)
}

struct SylkPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SylkPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "sylk".into();
        page.title = "SYLK spreadsheet".into();
        page.description = "SYLK cell values rendered as an inert paginated table".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse_sylk(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_SYLK_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SYLK input exceeds {MAX_SYLK_BYTES} bytes"
        )));
    }
    let mut saw_id = false;
    let mut saw_end = false;
    let mut current_x = 1usize;
    let mut current_y = 1usize;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut numeric_columns = HashSet::new();
    let mut formula_seen = false;
    let mut formatting_seen = false;
    let mut unknown_record_seen = false;
    let mut warnings = Vec::new();
    let mut cells = 0usize;

    for (line_number, original) in text.lines().enumerate() {
        if line_number >= MAX_SYLK_LINES {
            return Err(Error::LimitExceeded(format!(
                "SYLK exceeds {MAX_SYLK_LINES} lines"
            )));
        }
        if original.len() > MAX_SYLK_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "SYLK line {} exceeds {MAX_SYLK_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let fields = split_fields(line)?;
        let record = fields.first().map(String::as_str).unwrap_or_default();
        if !saw_id {
            if record != "ID" {
                return Err(Error::InvalidInput(
                    "SYLK must start with an ID record".into(),
                ));
            }
            saw_id = true;
            continue;
        }
        match record {
            "B" => {
                // B bounds are advisory; validate without allocating solely
                // from them because sparse files may advertise large ranges.
                for field in fields.iter().skip(1) {
                    if let Some(value) = field.strip_prefix('X').or_else(|| field.strip_prefix('Y'))
                    {
                        value
                            .parse::<usize>()
                            .map_err(|_| Error::InvalidInput("SYLK B bounds are invalid".into()))?;
                    }
                }
            }
            "C" | "F" => {
                if record == "F" {
                    formatting_seen = true;
                }
                let mut x = None;
                let mut y = None;
                let mut value = None;
                let mut formula = None;
                for field in fields.iter().skip(1) {
                    if let Some(raw) = field.strip_prefix('X') {
                        x = Some(parse_coordinate(raw, "X")?);
                    } else if let Some(raw) = field.strip_prefix('Y') {
                        y = Some(parse_coordinate(raw, "Y")?);
                    } else if let Some(raw) = field.strip_prefix('K') {
                        value = Some(parse_value(raw)?);
                    } else if let Some(raw) = field.strip_prefix('E') {
                        if raw.len() > MAX_SYLK_VALUE_BYTES {
                            return Err(Error::LimitExceeded(format!(
                                "SYLK formula exceeds {MAX_SYLK_VALUE_BYTES} bytes"
                            )));
                        }
                        formula = Some(raw.to_owned());
                    }
                }
                if let Some(value) = x {
                    current_x = value;
                }
                if let Some(value) = y {
                    current_y = value;
                }
                if record == "F" {
                    continue;
                }
                if formula.is_some() {
                    formula_seen = true;
                }
                if value.is_none() && formula.is_none() {
                    continue;
                }
                let row = current_y;
                let column = current_x;
                if row == 0 || row > MAX_SYLK_ROWS || column == 0 || column > MAX_SYLK_COLUMNS {
                    return Err(Error::LimitExceeded(format!(
                        "SYLK cell coordinate ({column},{row}) exceeds table limits"
                    )));
                }
                cells = cells
                    .checked_add(1)
                    .ok_or_else(|| Error::LimitExceeded("SYLK cell count overflowed".into()))?;
                if cells > MAX_SYLK_CELLS {
                    return Err(Error::LimitExceeded(format!(
                        "SYLK exceeds {MAX_SYLK_CELLS} cells"
                    )));
                }
                if rows.len() < row {
                    rows.resize_with(row, Vec::new);
                }
                let target = &mut rows[row - 1];
                if target.len() < column {
                    target.resize(column, String::new());
                }
                let rendered = match (value, formula) {
                    (Some(value), Some(formula)) => format!("{value} [formula: {formula}]"),
                    (Some(value), None) => value,
                    (None, Some(formula)) => format!("[formula: {formula}]"),
                    (None, None) => String::new(),
                };
                if rendered.parse::<f64>().is_ok()
                    || rendered
                        .split_ascii_whitespace()
                        .next()
                        .is_some_and(|token| token.parse::<f64>().is_ok())
                {
                    numeric_columns.insert(column - 1);
                }
                target[column - 1] = rendered;
                max_x = max_x.max(column);
                max_y = max_y.max(row);
            }
            "E" => {
                saw_end = true;
                break;
            }
            _ => {
                // ID/B/C/F/E are the geometry-bearing records. Other records
                // (P, O, NE, formats, print settings) remain inert.
                unknown_record_seen = true;
            }
        }
    }
    if !saw_id {
        return Err(Error::InvalidInput("SYLK ID record is missing".into()));
    }
    if !saw_end {
        warnings.push("SYLK end record was missing; input ended after the last record".into());
    }
    if formula_seen {
        warnings.push("SYLK formulas were preserved as inert text and not evaluated".into());
    }
    if formatting_seen {
        warnings.push("SYLK formatting records were ignored; cell values were retained".into());
    }
    if unknown_record_seen {
        warnings.push("SYLK non-cell metadata records were ignored".into());
    }
    if rows.is_empty() || max_x == 0 || max_y == 0 {
        return Err(Error::InvalidInput("SYLK contains no cell values".into()));
    }
    let has_header = rows
        .first()
        .is_some_and(|row| row.iter().any(|value| !value.is_empty()));
    let headers = if has_header {
        rows.first().cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let data_rows = if has_header {
        if rows.len() > 1 {
            rows[1..].to_vec()
        } else {
            Vec::new()
        }
    } else {
        rows
    };
    let alignments = (0..max_x)
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
            rows: data_rows,
            alignments,
            raw_source: String::new(),
        },
        dedup_warnings(warnings),
    ))
}

fn split_fields(line: &str) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        if quoted {
            if character == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                current.push(character);
            }
        } else if character == ';' {
            fields.push(std::mem::take(&mut current));
        } else if character == '"' {
            quoted = true;
        } else {
            current.push(character);
        }
    }
    if quoted {
        return Err(Error::InvalidInput(
            "SYLK field quote is unterminated".into(),
        ));
    }
    fields.push(current);
    Ok(fields)
}

fn parse_coordinate(raw: &str, axis: &str) -> Result<usize> {
    raw.parse::<usize>()
        .map_err(|_| Error::InvalidInput(format!("SYLK {axis} coordinate {raw:?} is invalid")))
}

fn parse_value(raw: &str) -> Result<String> {
    if raw.len() > MAX_SYLK_VALUE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SYLK cell value exceeds {MAX_SYLK_VALUE_BYTES} bytes"
        )));
    }
    Ok(raw.to_owned())
}

fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
