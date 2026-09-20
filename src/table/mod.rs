//! Markdown and structured table parsing, grid layout, SVG rendering, and reverse extraction.

use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

const MAX_DELIMITED_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DELIMITED_RECORDS: usize = 50_000;
const MAX_DELIMITED_COLUMNS: usize = 256;
const MAX_DELIMITED_CELLS: usize = 1_000_000;
const MAX_DELIMITED_FIELD_BYTES: usize = 64 * 1024;
const MAX_DELIMITED_RECORD_BYTES: usize = 1024 * 1024;
const DELIMITED_ROWS_PER_PAGE: usize = 100;
const DELIMITED_COLUMNS_PER_PAGE: usize = 32;
const MAX_EMBEDDED_DELIMITED_SOURCE_BYTES: usize = 8 * 1024 * 1024;
const MAX_RENDERED_CELL_CHARS: usize = 512;

#[derive(Clone, Debug, Default)]
pub struct TableData {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub alignments: Vec<TableAlign>,
    pub raw_source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TableAlign {
    #[default]
    Left,
    Center,
    Right,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DELIMITED_INPUT_BYTES),
        "table input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("table file is not valid UTF-8: {e}")))?;

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    let (delimiter, format) = match ext.as_str() {
        "csv" => (Some(','), "csv"),
        "tsv" | "tab" => (Some('\t'), "tsv"),
        _ if !text.contains('|') && text.contains('\t') => (Some('\t'), "tsv"),
        _ if !text.contains('|') && text.contains(',') => (Some(','), "csv"),
        _ => (None, "markdown_table"),
    };
    if let Some(delimiter) = delimiter {
        let (mut table, mut warnings) = parse_delimited_table_with_warnings(&text, delimiter)?;
        if table.raw_source.is_empty() {
            warnings.push("the full CSV/TSV source was not embedded because it spans multiple preview pages or exceeds 8 MiB".into());
        }
        convert_table_pages(&mut table, format, options, sink)?;
        return Ok(warnings);
    }

    let table = parse_markdown_table(&text)?;
    let page = layout_and_render_table(&table, options)?;
    sink.consume(page)?;
    Ok(Vec::new())
}

/// Parses CSV or TSV text into a bounded table model.
pub fn parse_delimited_table(source: &str, delimiter: char) -> Result<TableData> {
    parse_delimited_table_with_warnings(source, delimiter).map(|(table, _)| table)
}

fn parse_delimited_table_with_warnings(
    source: &str,
    delimiter: char,
) -> Result<(TableData, Vec<String>)> {
    if source.len() as u64 > MAX_DELIMITED_INPUT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "delimited table input exceeds {MAX_DELIMITED_INPUT_BYTES} bytes"
        )));
    }
    let parse_source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_field = String::new();
    let mut in_quotes = false;
    let mut after_quote = false;
    let mut field_started = false;
    let mut record_started = false;
    let mut record_bytes = 0usize;
    let mut records_seen = 0usize;
    let mut total_cells = 0usize;
    let mut blank_records = 0usize;
    let mut chars = parse_source.chars().peekable();

    while let Some(ch) = chars.next() {
        record_bytes = record_bytes.saturating_add(ch.len_utf8());
        if record_bytes > MAX_DELIMITED_RECORD_BYTES {
            return Err(Error::LimitExceeded(format!(
                "delimited table record exceeds {MAX_DELIMITED_RECORD_BYTES} bytes"
            )));
        }

        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    record_bytes = record_bytes.saturating_add(1);
                    check_delimited_record_bytes(record_bytes)?;
                    push_delimited_character(&mut current_field, '"')?;
                } else {
                    in_quotes = false;
                    after_quote = true;
                }
            } else {
                validate_delimited_character(ch)?;
                push_delimited_character(&mut current_field, ch)?;
            }
        } else if after_quote {
            if ch == delimiter {
                push_delimited_field(&mut current_row, &mut current_field)?;
                field_started = false;
                after_quote = false;
                record_started = true;
            } else if ch == '\n' || ch == '\r' {
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                    record_bytes = record_bytes.saturating_add(1);
                    check_delimited_record_bytes(record_bytes)?;
                }
                push_delimited_field(&mut current_row, &mut current_field)?;
                finish_delimited_record(
                    &mut rows,
                    &mut current_row,
                    &mut total_cells,
                    &mut records_seen,
                    &mut blank_records,
                    record_started,
                )?;
                field_started = false;
                after_quote = false;
                record_started = false;
                record_bytes = 0;
            } else {
                return Err(Error::InvalidInput(
                    "unexpected character after a closing CSV/TSV quote".into(),
                ));
            }
        } else if ch == '"' {
            if field_started || !current_field.is_empty() {
                return Err(Error::InvalidInput(
                    "a quote may only start at the beginning of a CSV/TSV field".into(),
                ));
            }
            in_quotes = true;
            field_started = true;
            record_started = true;
        } else if ch == delimiter {
            push_delimited_field(&mut current_row, &mut current_field)?;
            field_started = false;
            record_started = true;
        } else if ch == '\n' || ch == '\r' {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
                record_bytes = record_bytes.saturating_add(1);
                check_delimited_record_bytes(record_bytes)?;
            }
            push_delimited_field(&mut current_row, &mut current_field)?;
            finish_delimited_record(
                &mut rows,
                &mut current_row,
                &mut total_cells,
                &mut records_seen,
                &mut blank_records,
                record_started,
            )?;
            field_started = false;
            record_started = false;
            record_bytes = 0;
        } else {
            validate_delimited_character(ch)?;
            push_delimited_character(&mut current_field, ch)?;
            field_started = true;
            record_started = true;
        }
    }

    if in_quotes {
        return Err(Error::InvalidInput(
            "CSV/TSV input ended inside a quoted field".into(),
        ));
    }
    if field_started || after_quote || record_started || !current_row.is_empty() {
        push_delimited_field(&mut current_row, &mut current_field)?;
        finish_delimited_record(
            &mut rows,
            &mut current_row,
            &mut total_cells,
            &mut records_seen,
            &mut blank_records,
            record_started,
        )?;
    }

    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "no rows found in delimited table".into(),
        ));
    }

    let headers = rows.remove(0);
    if headers.is_empty() {
        return Err(Error::InvalidInput(
            "delimited table has no header fields".into(),
        ));
    }
    let col_count = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    let ragged_rows = rows.iter().filter(|row| row.len() != headers.len()).count();

    let mut alignments = vec![TableAlign::Left; col_count];
    for (column, alignment) in alignments.iter_mut().enumerate() {
        let is_numeric = !rows.is_empty()
            && rows
                .iter()
                .all(|row| row.get(column).is_none_or(|value| is_numeric_cell(value)));
        if is_numeric {
            *alignment = TableAlign::Right;
        }
    }

    let row_groups = rows.len().max(1).div_ceil(DELIMITED_ROWS_PER_PAGE);
    let column_groups = col_count.div_ceil(DELIMITED_COLUMNS_PER_PAGE);
    let page_count = row_groups.saturating_mul(column_groups);
    let raw_source = if page_count == 1 && source.len() <= MAX_EMBEDDED_DELIMITED_SOURCE_BYTES {
        source.to_owned()
    } else {
        String::new()
    };
    let mut warnings = Vec::new();
    if blank_records > 0 {
        warnings.push(format!(
            "{blank_records} empty CSV/TSV record(s) were skipped"
        ));
    }
    if ragged_rows > 0 {
        warnings.push(format!(
            "{ragged_rows} CSV/TSV record(s) have a different field count than the header; missing cells are blank and extra columns have empty headers"
        ));
    }

    Ok((
        TableData {
            headers,
            rows,
            alignments,
            raw_source,
        },
        warnings,
    ))
}

fn push_delimited_character(field: &mut String, character: char) -> Result<()> {
    if field.len().saturating_add(character.len_utf8()) > MAX_DELIMITED_FIELD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "delimited table field exceeds {MAX_DELIMITED_FIELD_BYTES} bytes"
        )));
    }
    field.push(character);
    Ok(())
}

fn check_delimited_record_bytes(record_bytes: usize) -> Result<()> {
    if record_bytes > MAX_DELIMITED_RECORD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "delimited table record exceeds {MAX_DELIMITED_RECORD_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_delimited_character(character: char) -> Result<()> {
    let codepoint = character as u32;
    if (character.is_control() && !matches!(character, '\t' | '\r' | '\n'))
        || matches!(codepoint, 0xFFFE | 0xFFFF)
    {
        return Err(Error::InvalidInput(format!(
            "delimited table contains a forbidden control character U+{:04X}",
            codepoint
        )));
    }
    Ok(())
}

fn push_delimited_field(row: &mut Vec<String>, field: &mut String) -> Result<()> {
    if row.len() >= MAX_DELIMITED_COLUMNS {
        return Err(Error::LimitExceeded(format!(
            "delimited table exceeds {MAX_DELIMITED_COLUMNS} columns"
        )));
    }
    row.push(std::mem::take(field));
    Ok(())
}

fn finish_delimited_record(
    rows: &mut Vec<Vec<String>>,
    row: &mut Vec<String>,
    total_cells: &mut usize,
    records_seen: &mut usize,
    blank_records: &mut usize,
    record_started: bool,
) -> Result<()> {
    *records_seen = records_seen.saturating_add(1);
    if *records_seen > MAX_DELIMITED_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "delimited table exceeds {MAX_DELIMITED_RECORDS} records including blank lines"
        )));
    }
    if !record_started && row.iter().all(String::is_empty) {
        *blank_records = blank_records.saturating_add(1);
        row.clear();
        return Ok(());
    }
    *total_cells = total_cells
        .checked_add(row.len())
        .ok_or_else(|| Error::LimitExceeded("delimited table cell count overflowed".into()))?;
    if *total_cells > MAX_DELIMITED_CELLS {
        return Err(Error::LimitExceeded(format!(
            "delimited table exceeds {MAX_DELIMITED_CELLS} cells"
        )));
    }
    rows.push(std::mem::take(row));
    Ok(())
}

pub(crate) fn convert_table_pages(
    table: &mut TableData,
    format: &str,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<()> {
    let col_count = table
        .headers
        .len()
        .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
    let row_groups = table.rows.len().max(1).div_ceil(DELIMITED_ROWS_PER_PAGE);
    let column_groups = col_count.div_ceil(DELIMITED_COLUMNS_PER_PAGE);
    let page_count = row_groups
        .checked_mul(column_groups)
        .ok_or_else(|| Error::LimitExceeded("delimited table page count overflowed".into()))?;
    if page_count == 0 || page_count > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "delimited table requires {page_count} pages; maximum is {}",
            options.max_pages
        )));
    }

    let mut page_number = 1usize;
    let row_chunks: Vec<&[Vec<String>]> = if table.rows.is_empty() {
        vec![&[]]
    } else {
        table.rows.chunks(DELIMITED_ROWS_PER_PAGE).collect()
    };
    for (row_group, rows) in row_chunks.iter().enumerate() {
        let first_row = row_group * DELIMITED_ROWS_PER_PAGE + 1;
        let last_row = (first_row + rows.len().saturating_sub(1)).min(table.rows.len());
        for column_group in 0..column_groups {
            let column_start = column_group * DELIMITED_COLUMNS_PER_PAGE;
            let column_end = (column_start + DELIMITED_COLUMNS_PER_PAGE).min(col_count);
            let headers = (column_start..column_end)
                .map(|column| table.headers.get(column).cloned().unwrap_or_default())
                .collect();
            let chunk_rows = rows
                .iter()
                .map(|row| {
                    (column_start..column_end)
                        .map(|column| row.get(column).cloned().unwrap_or_default())
                        .collect()
                })
                .collect();
            let alignments = (column_start..column_end)
                .map(|column| {
                    table
                        .alignments
                        .get(column)
                        .copied()
                        .unwrap_or(TableAlign::Left)
                })
                .collect();
            let page_table = TableData {
                headers,
                rows: chunk_rows,
                alignments,
                raw_source: if page_number == 1 {
                    std::mem::take(&mut table.raw_source)
                } else {
                    String::new()
                },
            };
            let mut page = layout_and_render_table(&page_table, options)?;
            page.number = page_number;
            page.source_format = format.into();
            page.title = if column_groups == 1 {
                format!("{} Table", format.to_ascii_uppercase())
            } else {
                format!(
                    "{} Table — columns {}–{}",
                    format.to_ascii_uppercase(),
                    column_start + 1,
                    column_end
                )
            };
            page.description = if rows.is_empty() {
                format!(
                    "{} table with headers and no data rows",
                    format.to_ascii_uppercase()
                )
            } else {
                format!(
                    "{} table rows {first_row}–{last_row} of {}, columns {}–{}",
                    format.to_ascii_uppercase(),
                    table.rows.len(),
                    column_start + 1,
                    column_end
                )
            };
            sink.consume(page)?;
            page_number += 1;
        }
    }
    Ok(())
}

/// Parses GFM markdown table subset
pub fn parse_markdown_table(source: &str) -> Result<TableData> {
    let mut headers = Vec::new();
    let mut alignments = Vec::new();
    let mut rows = Vec::new();

    let mut state = 0; // 0 = expecting header, 1 = expecting separator, 2 = rows

    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || !line.contains('|') {
            continue;
        }

        let cells = split_markdown_row(line);
        if cells.is_empty() {
            continue;
        }

        match state {
            0 => {
                headers = cells;
                state = 1;
            }
            1 => {
                let is_separator = cells.iter().all(|c| {
                    let trimmed = c.trim();
                    !trimmed.is_empty()
                        && trimmed.contains('-')
                        && trimmed
                            .chars()
                            .all(|ch| ch == '-' || ch == ':' || ch == ' ')
                });

                if is_separator {
                    for cell in &cells {
                        let c = cell.trim();
                        let align = if c.starts_with(':') && c.ends_with(':') {
                            TableAlign::Center
                        } else if c.ends_with(':') {
                            TableAlign::Right
                        } else {
                            TableAlign::Left
                        };
                        alignments.push(align);
                    }
                    while alignments.len() < headers.len() {
                        alignments.push(TableAlign::Left);
                    }
                    state = 2;
                } else {
                    alignments = vec![TableAlign::Left; headers.len()];
                    rows.push(cells);
                    state = 2;
                }
            }
            2 => {
                rows.push(cells);
            }
            _ => {}
        }
    }

    if headers.is_empty() && rows.is_empty() {
        return Err(Error::InvalidInput(
            "no table content found in markdown".into(),
        ));
    }

    // Auto-detect numeric columns when no explicit alignment was specified (i.e. Left default)
    for (c, align) in alignments.iter_mut().enumerate().take(headers.len()) {
        if *align == TableAlign::Left {
            let is_numeric = !rows.is_empty()
                && rows.iter().all(|r| {
                    if let Some(val) = r.get(c) {
                        is_numeric_cell(val)
                    } else {
                        true
                    }
                });
            if is_numeric {
                *align = TableAlign::Right;
            }
        }
    }

    Ok(TableData {
        headers,
        rows,
        alignments,
        raw_source: source.to_string(),
    })
}

/// Determines if a cell value represents a numeric quantity (including formatted currencies,
/// percentages, thousand-separated integers, and accounting parenthesized negatives).
pub(crate) fn is_numeric_cell(val: &str) -> bool {
    let t = val.trim();
    if t.is_empty() {
        return true;
    }
    // Accounting negative e.g. (1,234.56)
    let s = if t.starts_with('(') && t.ends_with(')') && t.len() > 2 {
        &t[1..t.len() - 1]
    } else {
        t
    };
    let s = s.trim();

    // Strip leading currency or sign
    let s = s
        .strip_prefix('$')
        .or_else(|| s.strip_prefix('€'))
        .or_else(|| s.strip_prefix('£'))
        .or_else(|| s.strip_prefix('¥'))
        .or_else(|| s.strip_prefix('₩'))
        .or_else(|| s.strip_prefix('₹'))
        .unwrap_or(s);
    let s = s.trim();

    // Strip trailing percentage or currency sign
    let s = s
        .strip_suffix('%')
        .or_else(|| s.strip_suffix('$'))
        .or_else(|| s.strip_suffix('€'))
        .or_else(|| s.strip_suffix('£'))
        .or_else(|| s.strip_suffix('¥'))
        .unwrap_or(s);
    let s = s.trim();

    // Must contain at least one digit
    if !s.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }

    // Remove commas if thousand separator (e.g. 1,000,000.50)
    let cleaned: String = s.chars().filter(|&c| c != ',').collect();
    cleaned.parse::<f64>().is_ok()
}

pub(crate) fn split_markdown_row(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();

    // Skip leading pipe if present
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '|' {
            chars.next();
            break;
        } else {
            break;
        }
    }

    let mut in_code = false;
    while let Some(c) = chars.next() {
        if c == '`' {
            in_code = !in_code;
            current.push(c);
        } else if c == '\\' && chars.peek() == Some(&'|') {
            // Escaped pipe `\|` -> literal `|`
            chars.next();
            current.push('|');
        } else if c == '|' && !in_code {
            cells.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(c);
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        cells.push(trimmed.to_string());
    }

    cells
}

/// Computes column widths, row heights, and generates clean Page IR for the table.
pub fn layout_and_render_table(table: &TableData, _options: &ConvertOptions) -> Result<Page> {
    let col_count = table
        .headers
        .len()
        .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);

    let font_size = 12.0;
    let header_font_size = 12.5;
    let padding_x = 16.0;
    let cell_height = 36.0;
    let header_height = 40.0;
    let page_margin = 32.0;

    let mut col_widths = vec![80.0f64; col_count];
    for (c, h) in table.headers.iter().enumerate() {
        if c < col_count {
            let width = estimate_cell_text_width(h, header_font_size * 0.65) + padding_x * 2.0;
            col_widths[c] = col_widths[c].max(width);
        }
    }
    for row in &table.rows {
        for (c, cell) in row.iter().enumerate() {
            if c < col_count {
                let cell_clean = sanitize_cell_text(cell);
                let width =
                    estimate_cell_text_width(&cell_clean, font_size * 0.62) + padding_x * 2.0;
                col_widths[c] = col_widths[c].max(width);
            }
        }
    }

    let total_table_width: f64 = col_widths.iter().sum();
    let total_table_height = header_height + (table.rows.len() as f64 * cell_height);
    let page_width = total_table_width + page_margin * 2.0;
    let page_height = total_table_height + page_margin * 2.0;

    let mut page = Page::new(1, page_width, page_height, "markdown_table");
    let cell_text_was_truncated = table
        .headers
        .iter()
        .any(|text| cell_text_is_truncated(text))
        || table
            .rows
            .iter()
            .flatten()
            .any(|text| cell_text_is_truncated(text));
    if cell_text_was_truncated {
        page.warn(format!(
            "table cell text longer than {MAX_RENDERED_CELL_CHARS} characters was truncated in the visual preview"
        ));
    }
    if !table.raw_source.is_empty() {
        page.embedded_source = Some(table.raw_source.clone());
    }

    let start_x = page_margin;
    let start_y = page_margin;

    // 1. Header background
    let header_d = format!(
        "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} Z",
        start_x,
        start_y,
        start_x + total_table_width,
        start_y,
        start_x + total_table_width,
        start_y + header_height,
        start_x,
        start_y + header_height
    );
    page.nodes.push(Node::Path {
        id: String::new(),
        d: header_d,
        fill_rule: String::new(),
        fill: Paint::solid("#f8fafc"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta::default(),
    });

    // 2. Alternating row background
    for (row_idx, _) in table.rows.iter().enumerate() {
        if row_idx % 2 == 1 {
            let row_y = start_y + header_height + (row_idx as f64 * cell_height);
            let row_d = format!(
                "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} Z",
                start_x,
                row_y,
                start_x + total_table_width,
                row_y,
                start_x + total_table_width,
                row_y + cell_height,
                start_x,
                row_y + cell_height
            );
            page.nodes.push(Node::Path {
                id: String::new(),
                d: row_d,
                fill_rule: String::new(),
                fill: Paint::solid("#f1f5f9"),
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }
    }

    // 3. Grid lines
    let mut grid_d = String::new();
    // Horizontal lines
    grid_d.push_str(&format!(
        "M {:.2},{:.2} L {:.2},{:.2} ",
        start_x,
        start_y,
        start_x + total_table_width,
        start_y
    ));
    grid_d.push_str(&format!(
        "M {:.2},{:.2} L {:.2},{:.2} ",
        start_x,
        start_y + header_height,
        start_x + total_table_width,
        start_y + header_height
    ));
    for r in 1..=table.rows.len() {
        let y = start_y + header_height + (r as f64 * cell_height);
        grid_d.push_str(&format!(
            "M {:.2},{:.2} L {:.2},{:.2} ",
            start_x,
            y,
            start_x + total_table_width,
            y
        ));
    }

    // Vertical lines
    let mut cur_col_x = start_x;
    grid_d.push_str(&format!(
        "M {:.2},{:.2} L {:.2},{:.2} ",
        cur_col_x,
        start_y,
        cur_col_x,
        start_y + total_table_height
    ));
    for w in &col_widths {
        cur_col_x += w;
        grid_d.push_str(&format!(
            "M {:.2},{:.2} L {:.2},{:.2} ",
            cur_col_x,
            start_y,
            cur_col_x,
            start_y + total_table_height
        ));
    }

    page.nodes.push(Node::Path {
        id: String::new(),
        d: grid_d,
        fill_rule: String::new(),
        fill: Paint::None,
        stroke: Stroke {
            paint: Paint::solid("#e2e8f0"),
            width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            ..Default::default()
        },
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta::default(),
    });

    // 4. Header text
    let mut col_x = start_x;
    for (c, header) in table.headers.iter().enumerate() {
        let width = col_widths[c];
        let align = table.alignments.get(c).copied().unwrap_or(TableAlign::Left);
        let (text_x, anchor) = match align {
            TableAlign::Right => (col_x + width - padding_x, TextAnchor::End),
            TableAlign::Center => (col_x + width / 2.0, TextAnchor::Middle),
            TableAlign::Left => (col_x + padding_x, TextAnchor::Start),
        };
        let text_y = start_y + header_height / 2.0 + 4.5;

        page.nodes.push(Node::Text {
            id: String::new(),
            x: text_x,
            y: text_y,
            runs: vec![TextRun {
                text: sanitize_cell_text(header),
                font_size: header_font_size,
                font_family: "Helvetica, Arial, sans-serif".to_string(),
                bold: true,
                fill: Paint::solid("#0f172a"),
                ..Default::default()
            }],
            anchor,
            transform: IDENTITY,
            opacity: 1.0,
            stroke: Stroke::default(),
            clip_id: None,
            meta: SourceMeta::default(),
        });
        col_x += width;
    }

    // 5. Row cells
    for (row_idx, row) in table.rows.iter().enumerate() {
        let row_y = start_y + header_height + (row_idx as f64 * cell_height);
        let mut cur_x = start_x;
        for (col_idx, cell) in row.iter().enumerate() {
            if col_idx < col_widths.len() {
                let width = col_widths[col_idx];
                let align = table
                    .alignments
                    .get(col_idx)
                    .copied()
                    .unwrap_or(TableAlign::Left);
                let (text_x, anchor) = match align {
                    TableAlign::Right => (cur_x + width - padding_x, TextAnchor::End),
                    TableAlign::Center => (cur_x + width / 2.0, TextAnchor::Middle),
                    TableAlign::Left => (cur_x + padding_x, TextAnchor::Start),
                };
                let text_y = row_y + cell_height / 2.0 + 4.0;

                page.nodes.push(Node::Text {
                    id: String::new(),
                    x: text_x,
                    y: text_y,
                    runs: vec![TextRun {
                        text: sanitize_cell_text(cell),
                        font_size,
                        font_family: "Helvetica, Arial, sans-serif".to_string(),
                        fill: Paint::solid("#334155"),
                        ..Default::default()
                    }],
                    anchor,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
                cur_x += width;
            }
        }
    }

    Ok(page)
}

/// Reverse extraction: reads SVG containing a table and reconstructs Markdown Table.
pub fn extract_markdown_table_from_svg(svg_bytes: &[u8]) -> Result<String> {
    let svg_text = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    // 1. Check embedded source (content attribute)
    if let Some(decoded) = crate::cad::svg_reader::extract_embedded_source(svg_bytes)
        && !decoded.trim().is_empty()
    {
        return Ok(decoded);
    }

    // 2. Geometric fallback: scan text cells
    let mut reader = Reader::from_str(svg_text);
    reader.config_mut().trim_text(true);

    struct CellEntry {
        x: f64,
        y: f64,
        text: String,
    }

    let mut cells: Vec<CellEntry> = Vec::new();
    let mut current_x = 0.0f64;
    let mut current_y = 0.0f64;
    let mut in_text = false;
    let mut current_text = String::new();

    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) if e.name().as_ref() == b"text" => {
                in_text = true;
                current_text.clear();
                for attr in e.attributes().flatten() {
                    if let Ok(s) = std::str::from_utf8(&attr.value) {
                        if attr.key.as_ref() == b"x" {
                            current_x = s.parse().unwrap_or(0.0);
                        } else if attr.key.as_ref() == b"y" {
                            current_y = s.parse().unwrap_or(0.0);
                        }
                    }
                }
            }
            Event::Text(e) if in_text => {
                let bytes = e.as_ref();
                if let Ok(s) = std::str::from_utf8(bytes) {
                    current_text.push_str(s);
                }
            }
            Event::End(e) if e.name().as_ref() == b"text" => {
                in_text = false;
                let trimmed = current_text.trim();
                if !trimmed.is_empty() {
                    cells.push(CellEntry {
                        x: current_x,
                        y: current_y,
                        text: trimmed.to_string(),
                    });
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if cells.is_empty() {
        return Ok(String::new());
    }

    cells.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));

    let mut rows: Vec<Vec<CellEntry>> = Vec::new();
    for cell in cells {
        if let Some(last_row) = rows.last_mut().filter(|r| (r[0].y - cell.y).abs() < 12.0) {
            last_row.push(cell);
            continue;
        }
        rows.push(vec![cell]);
    }

    for row in &mut rows {
        row.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    }

    // Determine max columns and per-column width
    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return Ok(String::new());
    }

    let mut col_widths = vec![3usize; col_count];
    for row in &rows {
        for (i, c) in row.iter().enumerate() {
            if i < col_count {
                let escaped = c.text.replace('|', r#"\|"#);
                col_widths[i] = col_widths[i].max(escaped.len());
            }
        }
    }

    let mut md = String::new();
    if let Some(headers) = rows.first() {
        md.push_str("| ");
        for (i, &w) in col_widths.iter().enumerate() {
            let text = headers.get(i).map(|c| c.text.as_str()).unwrap_or("");
            let escaped = text.replace('|', r#"\|"#);
            md.push_str(&format!("{escaped:<w$} | "));
        }
        md.push('\n');

        md.push_str("| ");
        for &w in &col_widths {
            let sep = "-".repeat(w.max(3));
            md.push_str(&format!("{sep} | "));
        }
        md.push('\n');

        for row in rows.iter().skip(1) {
            md.push_str("| ");
            for (i, &w) in col_widths.iter().enumerate() {
                let text = row.get(i).map(|c| c.text.as_str()).unwrap_or("");
                let escaped = text.replace('|', r#"\|"#);
                md.push_str(&format!("{escaped:<w$} | "));
            }
            md.push('\n');
        }
    }

    Ok(md)
}

fn is_cjk_or_fullwidth(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | // CJK Symbols and Punctuation
        0x3040..=0x309F | // Hiragana
        0x30A0..=0x30FF | // Katakana
        0x3400..=0x4DBF | // CJK Unified Ideographs Extension A
        0x4E00..=0x9FFF | // CJK Unified Ideographs
        0xF900..=0xFAFF | // CJK Compatibility Ideographs
        0xFF00..=0xFFEF | // Halfwidth and Fullwidth Forms
        0xAC00..=0xD7AF   // Hangul Syllables
    )
}

fn estimate_cell_text_width(text: &str, char_width: f64) -> f64 {
    let mut width = 0.0;
    for c in text.chars() {
        if is_cjk_or_fullwidth(c) {
            width += char_width * 1.8;
        } else {
            width += char_width;
        }
    }
    width
}

fn normalized_cell_text(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .map(|character| {
            if matches!(character, '\r' | '\n' | '\t') {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn cell_text_is_truncated(text: &str) -> bool {
    normalized_cell_text(text)
        .chars()
        .take(MAX_RENDERED_CELL_CHARS + 1)
        .count()
        > MAX_RENDERED_CELL_CHARS
}

fn sanitize_cell_text(text: &str) -> String {
    let normalized = normalized_cell_text(text);
    let mut characters = normalized.chars();
    let mut preview = characters
        .by_ref()
        .take(MAX_RENDERED_CELL_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        preview.push('…');
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc4180_quotes_and_preserves_field_whitespace() {
        let source = "\u{feff}Name,Note\r\n\" Acme, Inc. \",\"She said \"\"hello\"\"\"\r\nAlice,\"line one\r\nline two\"\r\n\r\n";
        let (table, warnings) = parse_delimited_table_with_warnings(source, ',').unwrap();
        assert_eq!(table.headers, ["Name", "Note"]);
        assert_eq!(table.rows[0], [" Acme, Inc. ", "She said \"hello\""]);
        assert_eq!(table.rows[1][1], "line one\r\nline two");
        assert_eq!(warnings, ["1 empty CSV/TSV record(s) were skipped"]);
    }

    #[test]
    fn rejects_malformed_quotes_and_forbidden_controls() {
        for malformed in [
            "Name,Note\nAlice,\"unfinished\n",
            "Name,Note\nAl\"ice,ok\n",
            "Name,Note\n\"Alice\"x,ok\n",
            "Name,Note\nAlice,\0bad\n",
            "Name,Note\nAlice,\u{FFFE}bad\n",
        ] {
            assert!(
                parse_delimited_table(malformed, ',').is_err(),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn preserves_ragged_rows_and_reports_the_shape_difference() {
        let (table, warnings) =
            parse_delimited_table_with_warnings("A,B\n1\n2,3,extra\n", ',').unwrap();
        assert_eq!(table.headers, ["A", "B"]);
        assert_eq!(
            table.rows,
            [
                vec!["1".to_owned()],
                vec!["2".to_owned(), "3".to_owned(), "extra".to_owned()]
            ]
        );
        assert_eq!(table.alignments.len(), 3);
        assert!(warnings[0].contains("different field count"));
    }

    #[test]
    fn enforces_row_column_field_and_cell_budgets() {
        let too_many_columns = format!("{}\n", vec!["x"; MAX_DELIMITED_COLUMNS + 1].join(","));
        assert!(parse_delimited_table(&too_many_columns, ',').is_err());

        let too_long_field = format!("header\n{}\n", "x".repeat(MAX_DELIMITED_FIELD_BYTES + 1));
        assert!(parse_delimited_table(&too_long_field, ',').is_err());

        let row = vec!["x"; MAX_DELIMITED_COLUMNS.min(21)].join(",");
        let too_many_cells = format!(
            "{row}\n{}",
            format!("{row}\n").repeat(MAX_DELIMITED_RECORDS - 1)
        );
        assert!(parse_delimited_table(&too_many_cells, ',').is_err());
    }

    #[test]
    fn truncates_long_display_cells_and_keeps_a_reportable_boundary() {
        let text = "x".repeat(MAX_RENDERED_CELL_CHARS + 20);
        assert!(cell_text_is_truncated(&text));
        let preview = sanitize_cell_text(&text);
        assert_eq!(preview.chars().count(), MAX_RENDERED_CELL_CHARS + 1);
        assert!(preview.ends_with('…'));
    }
}
