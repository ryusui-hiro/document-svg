//! Bounded previews of legacy binary Excel workbooks.
//!
//! BIFF workbooks are dimension-checked before Calamine allocates worksheet
//! ranges. Cell values are paged into table previews; formulas use cached
//! results, and Excel formatting, charts, macros and embedded objects are not
//! reproduced.

use std::io::{Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use calamine::{Data, Reader, SheetVisible};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::Page;
use crate::table::{TableAlign, TableData, layout_and_render_table};

const MAX_XLS_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XLS_WORKBOOK_STREAM_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XLS_SHEETS: usize = 256;
const MAX_XLS_RECORDS: usize = 2_000_000;
const MAX_BIFF_RECORD_BYTES: usize = 8_224;
const MAX_XLS_CELLS_TOTAL: usize = 1_000_000;
const MAX_XLS_DENSE_CELLS_PER_SHEET: usize = 500_000;
const MAX_XLS_DENSE_CELLS_TOTAL: usize = 2_000_000;
const MAX_XLS_MERGED_RANGES: usize = 100_000;
const MAX_XLS_EXPANDED_TEXT_BYTES: usize = 128 * 1024 * 1024;
const XLS_MAX_ROWS: u32 = 65_536;
const XLS_MAX_COLUMNS: u32 = 256;
const PAGE_ROWS: usize = 40;
const PAGE_COLUMNS: usize = 12;

struct XlsPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    source_format: &'static str,
    warnings: &'a [String],
}

impl PageConsumer for XlsPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = self.source_format.into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &std::path::Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let input_limit = options.max_input_bytes.min(MAX_XLS_INPUT_BYTES);
    let bytes = read_limited_file(path, input_limit, "legacy Excel workbook")?;
    preflight_biff(&bytes)?;
    let source_format = "xls";
    let mut workbook = catch_unwind(AssertUnwindSafe(|| {
        calamine::open_workbook_auto_from_rs(Cursor::new(Arc::<[u8]>::from(bytes)))
    }))
    .map_err(|_| Error::InvalidInput("Excel parser rejected a malformed BIFF record".into()))?
    .map_err(|error| Error::InvalidInput(format!("invalid Excel workbook: {error}")))?;
    let sheet_names = workbook.sheet_names();
    if sheet_names.is_empty() {
        return Err(Error::InvalidInput(
            "Excel workbook contains no worksheets".into(),
        ));
    }
    if sheet_names.len() > MAX_XLS_SHEETS {
        return Err(Error::LimitExceeded(format!(
            "Excel workbook contains {} sheets; maximum is {MAX_XLS_SHEETS}",
            sheet_names.len()
        )));
    }
    let mut warnings = vec![
        "Excel cell formatting, merged-cell layout, charts, drawings, and embedded objects are not reproduced".into(),
        "Formula cells use cached results; formulas are not recalculated, and macros are never executed".into(),
    ];
    if workbook
        .sheets_metadata()
        .iter()
        .any(|sheet| sheet.visible != SheetVisible::Visible)
    {
        warnings.push("hidden and very-hidden worksheets are included in the preview".into());
    }
    let mut page_sink = XlsPageSink {
        inner: sink,
        source_format,
        warnings: &warnings,
    };
    let mut page_number = 0usize;
    for sheet_name in sheet_names {
        let range = catch_unwind(AssertUnwindSafe(|| workbook.worksheet_range(&sheet_name)))
            .map_err(|_| {
                Error::InvalidInput(format!(
                    "Excel parser rejected malformed cells in worksheet '{sheet_name}'"
                ))
            })?
            .map_err(|error| {
                Error::InvalidInput(format!(
                    "cannot read Excel worksheet '{sheet_name}': {error}"
                ))
            })?;
        let (rows, columns) = range.get_size();
        if rows.saturating_mul(columns) > MAX_XLS_DENSE_CELLS_PER_SHEET {
            return Err(Error::LimitExceeded(format!(
                "Excel worksheet '{sheet_name}' spans {} cells; maximum is {MAX_XLS_DENSE_CELLS_PER_SHEET}",
                rows.saturating_mul(columns)
            )));
        }
        let row_pages = rows.max(1).div_ceil(PAGE_ROWS);
        let column_pages = columns.max(1).div_ceil(PAGE_COLUMNS);
        let page_count = row_pages
            .checked_mul(column_pages)
            .and_then(|pages| page_number.checked_add(pages))
            .ok_or_else(|| Error::LimitExceeded("Excel page count overflowed".into()))?;
        if page_count > options.max_pages {
            return Err(Error::LimitExceeded(format!(
                "Excel workbook pagination would create {page_count} pages; maximum is {}",
                options.max_pages
            )));
        }

        if rows == 0 || columns == 0 {
            page_number += 1;
            let table = TableData {
                headers: vec!["Worksheet".into()],
                rows: vec![vec!["No populated cells".into()]],
                alignments: vec![TableAlign::Left],
                raw_source: String::new(),
            };
            let mut page = layout_and_render_table(&table, options)?;
            page.number = page_number;
            page.title = format!("{sheet_name} (empty worksheet)");
            page.description = format!("Empty Excel worksheet '{sheet_name}'");
            page_sink.consume(page)?;
            continue;
        }

        let mut source_rows = range.rows();
        let (base_row, base_column) = range.start().unwrap_or_default();
        for row_start in (0..rows).step_by(PAGE_ROWS) {
            let visible_rows = source_rows.by_ref().take(PAGE_ROWS).collect::<Vec<_>>();
            for column_start in (0..columns).step_by(PAGE_COLUMNS) {
                page_number += 1;
                let visible_columns = (columns - column_start).min(PAGE_COLUMNS);
                let mut table = TableData {
                    headers: (0..visible_columns)
                        .map(|column| {
                            excel_column_label(base_column as usize + column_start + column)
                        })
                        .collect(),
                    rows: Vec::with_capacity(visible_rows.len()),
                    alignments: vec![TableAlign::Left; visible_columns],
                    raw_source: String::new(),
                };
                for row in &visible_rows {
                    let mut values = Vec::with_capacity(visible_columns);
                    for cell in row.iter().skip(column_start).take(visible_columns) {
                        let value = excel_value(cell);
                        if value.len() > MAX_XLS_CELL_TEXT_BYTES {
                            return Err(Error::LimitExceeded(format!(
                                "Excel worksheet '{sheet_name}' contains a cell larger than {MAX_XLS_CELL_TEXT_BYTES} bytes"
                            )));
                        }
                        values.push(value);
                    }
                    while values.len() < visible_columns {
                        values.push(String::new());
                    }
                    table.rows.push(values);
                }
                let mut page = layout_and_render_table(&table, options)?;
                page.number = page_number;
                page.title = format!(
                    "{sheet_name} — rows {}–{}, columns {}–{}",
                    base_row as usize + row_start + 1,
                    base_row as usize + row_start + visible_rows.len(),
                    excel_column_label(base_column as usize + column_start),
                    excel_column_label(base_column as usize + column_start + visible_columns - 1)
                );
                page.description = format!(
                    "Excel worksheet '{sheet_name}', rows {}–{}, columns {}–{}",
                    base_row as usize + row_start + 1,
                    base_row as usize + row_start + visible_rows.len(),
                    excel_column_label(base_column as usize + column_start),
                    excel_column_label(base_column as usize + column_start + visible_columns - 1)
                );
                page_sink.consume(page)?;
            }
        }
    }
    Ok(warnings)
}

const MAX_XLS_CELL_TEXT_BYTES: usize = 64 * 1024;

fn excel_value(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        value => value.to_string(),
    }
}

fn excel_column_label(mut column: usize) -> String {
    let mut label = Vec::new();
    loop {
        let digit = column % 26;
        label.push((b'A' + digit as u8) as char);
        if column < 26 {
            break;
        }
        column = column / 26 - 1;
    }
    label.iter().rev().collect()
}

fn preflight_biff(file_bytes: &[u8]) -> Result<()> {
    let mut compound = cfb::CompoundFile::open(Cursor::new(file_bytes)).map_err(|error| {
        Error::InvalidInput(format!(
            "legacy Excel file is not a valid compound workbook: {error}"
        ))
    })?;
    let stream_name = ["Workbook", "Book"]
        .into_iter()
        .find(|name| compound.is_stream(name))
        .ok_or_else(|| {
            Error::InvalidInput("Excel compound file is missing Workbook stream".into())
        })?;
    let mut stream = compound.open_stream(stream_name).map_err(|error| {
        Error::InvalidInput(format!("cannot open Excel Workbook stream: {error}"))
    })?;
    let stream_len = stream.len();
    if stream_len > MAX_XLS_WORKBOOK_STREAM_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Excel Workbook stream is {stream_len} bytes; maximum is {MAX_XLS_WORKBOOK_STREAM_BYTES}"
        )));
    }
    let mut bytes = Vec::with_capacity(stream_len as usize);
    stream.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != stream_len {
        return Err(Error::InvalidInput(
            "Excel Workbook stream ended before its declared length".into(),
        ));
    }
    validate_biff_stream(&bytes)
}

#[derive(Default)]
struct BiffBounds {
    min_row: u32,
    max_row: u32,
    min_column: u32,
    max_column: u32,
    cell_records: usize,
    sst_references: Vec<usize>,
}

impl BiffBounds {
    fn add_cell(&mut self, row: u32, column: u32) -> Result<()> {
        if row >= XLS_MAX_ROWS || column >= XLS_MAX_COLUMNS {
            return Err(Error::InvalidInput(format!(
                "Excel cell coordinate ({row}, {column}) exceeds the BIFF worksheet bounds"
            )));
        }
        if self.cell_records == 0 {
            self.min_row = row;
            self.max_row = row;
            self.min_column = column;
            self.max_column = column;
        } else {
            self.min_row = self.min_row.min(row);
            self.max_row = self.max_row.max(row);
            self.min_column = self.min_column.min(column);
            self.max_column = self.max_column.max(column);
        }
        self.cell_records = self.cell_records.saturating_add(1);
        if self.cell_records > MAX_XLS_CELLS_TOTAL {
            return Err(Error::LimitExceeded(format!(
                "Excel workbook contains more than {MAX_XLS_CELLS_TOTAL} cell records"
            )));
        }
        Ok(())
    }

    fn check_dense_bounds(&self, sheet_name: usize) -> Result<()> {
        if self.cell_records == 0 {
            return Ok(());
        }
        let rows = (self.max_row - self.min_row + 1) as usize;
        let columns = (self.max_column - self.min_column + 1) as usize;
        let cells = rows.saturating_mul(columns);
        if cells > MAX_XLS_DENSE_CELLS_PER_SHEET {
            return Err(Error::LimitExceeded(format!(
                "Excel worksheet {sheet_name} spans {cells} cells; maximum is {MAX_XLS_DENSE_CELLS_PER_SHEET}"
            )));
        }
        Ok(())
    }
}

fn validate_biff_stream(stream: &[u8]) -> Result<()> {
    let mut sheet_offsets = Vec::new();
    let mut sst_lengths = Vec::new();
    let mut total_sst_bytes = 0usize;
    let mut total_inline_text = 0usize;
    let mut total_cells = 0usize;
    let mut total_declared_cells = 0usize;
    let mut total_merged_ranges = 0usize;
    let mut record_count = 0usize;
    let mut offset = 0usize;
    let mut found_global_eof = false;
    while let Some((kind, payload)) = next_biff_record(stream, &mut offset)? {
        record_count += 1;
        if record_count > MAX_XLS_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "Excel workbook exceeds {MAX_XLS_RECORDS} BIFF records"
            )));
        }
        let continuation_segments =
            collect_continuations(stream, &mut offset, &mut record_count, payload)?;
        let record_bytes =
            payload
                .len()
                .saturating_add(continuation_segments.as_ref().map_or(0, |segments| {
                    segments.iter().skip(1).map(|segment| segment.len()).sum()
                }));
        match kind {
            0x0809 if payload.len() < 2 => {
                return Err(Error::InvalidInput("Excel BOF record is truncated".into()));
            }
            0x0085 => {
                if payload.len() < 6 {
                    return Err(Error::InvalidInput(
                        "Excel BoundSheet record is truncated".into(),
                    ));
                }
                sheet_offsets.push(u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize);
                if sheet_offsets.len() > MAX_XLS_SHEETS {
                    return Err(Error::LimitExceeded(format!(
                        "Excel workbook contains more than {MAX_XLS_SHEETS} sheets"
                    )));
                }
            }
            0x00fc => {
                let segments = continuation_segments.unwrap_or_else(|| vec![payload]);
                sst_lengths = parse_sst_lengths(&segments)?;
                total_sst_bytes = sst_lengths.iter().try_fold(0usize, |sum, length| {
                    sum.checked_add(*length).ok_or_else(|| {
                        Error::LimitExceeded("Excel shared string size overflowed".into())
                    })
                })?;
                if total_sst_bytes > MAX_XLS_EXPANDED_TEXT_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "Excel shared strings expand beyond {MAX_XLS_EXPANDED_TEXT_BYTES} bytes"
                    )));
                }
            }
            0x0204 | 0x00d6 | 0x0207 => {
                total_inline_text =
                    total_inline_text.saturating_add(record_bytes.saturating_mul(3));
            }
            0x000a => {
                found_global_eof = true;
                break;
            }
            _ => {}
        }
    }
    if !found_global_eof || sheet_offsets.is_empty() {
        return Err(Error::InvalidInput(
            "Excel BIFF workbook is missing its sheet directory or global EOF".into(),
        ));
    }
    sheet_offsets.sort_unstable();
    if sheet_offsets
        .windows(2)
        .any(|window| window[0] == window[1])
    {
        return Err(Error::InvalidInput(
            "Excel workbook contains duplicate sheet offsets".into(),
        ));
    }
    if sheet_offsets
        .iter()
        .any(|sheet_offset| *sheet_offset >= stream.len())
    {
        return Err(Error::InvalidInput(
            "Excel sheet offset points outside the Workbook stream".into(),
        ));
    }

    for (sheet_index, sheet_offset) in sheet_offsets.iter().copied().enumerate() {
        let sheet_end = sheet_offsets
            .get(sheet_index + 1)
            .copied()
            .unwrap_or(stream.len());
        let mut sheet_offset = sheet_offset;
        let mut bounds = BiffBounds::default();
        let mut found_sheet_eof = false;
        while let Some((kind, payload)) =
            next_biff_record_until(stream, &mut sheet_offset, sheet_end)?
        {
            record_count += 1;
            if record_count > MAX_XLS_RECORDS {
                return Err(Error::LimitExceeded(format!(
                    "Excel workbook exceeds {MAX_XLS_RECORDS} BIFF records"
                )));
            }
            let continuation_segments =
                collect_continuations(stream, &mut sheet_offset, &mut record_count, payload)?;
            let record_bytes = payload.len().saturating_add(
                continuation_segments.as_ref().map_or(0, |segments| {
                    segments.iter().skip(1).map(|segment| segment.len()).sum()
                }),
            );
            match kind {
                0x0809 if payload.len() < 2 => {
                    return Err(Error::InvalidInput(format!(
                        "Excel worksheet {sheet_index} BOF record is truncated"
                    )));
                }
                0x0200 => {
                    let (first_row, last_row, first_column, last_column) =
                        parse_dimensions(payload)?;
                    if last_row > first_row && last_column > first_column {
                        let cells = (last_row - first_row)
                            .checked_mul(last_column - first_column)
                            .ok_or_else(|| {
                                Error::LimitExceeded("Excel sheet dimensions overflowed".into())
                            })? as usize;
                        if cells > MAX_XLS_DENSE_CELLS_PER_SHEET {
                            return Err(Error::LimitExceeded(format!(
                                "Excel worksheet {sheet_index} declares {cells} cells; maximum is {MAX_XLS_DENSE_CELLS_PER_SHEET}"
                            )));
                        }
                        total_declared_cells = total_declared_cells.saturating_add(cells);
                        if total_declared_cells > MAX_XLS_DENSE_CELLS_TOTAL {
                            return Err(Error::LimitExceeded(format!(
                                "Excel workbook declares more than {MAX_XLS_DENSE_CELLS_TOTAL} worksheet cells"
                            )));
                        }
                    }
                }
                0x0203 | 0x0204 | 0x00d6 | 0x0205 | 0x0207 | 0x027e | 0x00fd | 0x0006 => {
                    if payload.len() < 4 {
                        return Err(Error::InvalidInput(
                            "Excel worksheet cell record is truncated".into(),
                        ));
                    }
                    let row = u16::from_le_bytes(payload[0..2].try_into().unwrap()) as u32;
                    let column = u16::from_le_bytes(payload[2..4].try_into().unwrap()) as u32;
                    bounds.add_cell(row, column)?;
                    total_cells += 1;
                    if kind == 0x00fd {
                        if payload.len() < 10 {
                            return Err(Error::InvalidInput(
                                "Excel LABELSST record is truncated".into(),
                            ));
                        }
                        bounds
                            .sst_references
                            .push(u32::from_le_bytes(payload[6..10].try_into().unwrap()) as usize);
                    }
                    if kind == 0x0204 || kind == 0x00d6 || kind == 0x0207 || kind == 0x0006 {
                        total_inline_text =
                            total_inline_text.saturating_add(record_bytes.saturating_mul(3));
                    }
                }
                0x00bd => {
                    if payload.len() < 12 {
                        return Err(Error::InvalidInput(
                            "Excel MULRK record is truncated".into(),
                        ));
                    }
                    let row = u16::from_le_bytes(payload[0..2].try_into().unwrap()) as u32;
                    let first_column = u16::from_le_bytes(payload[2..4].try_into().unwrap()) as u32;
                    let last_column =
                        u16::from_le_bytes(payload[payload.len() - 2..].try_into().unwrap()) as u32;
                    if last_column < first_column
                        || payload.len() != 6 + 6 * (last_column - first_column + 1) as usize
                    {
                        return Err(Error::InvalidInput(
                            "invalid Excel MULRK record size".into(),
                        ));
                    }
                    for column in first_column..=last_column {
                        bounds.add_cell(row, column)?;
                    }
                    total_cells =
                        total_cells.saturating_add((last_column - first_column + 1) as usize);
                }
                0x00e5 => {
                    if payload.len() < 2 {
                        return Err(Error::InvalidInput(
                            "Excel merged-cell record is truncated".into(),
                        ));
                    }
                    let count = u16::from_le_bytes(payload[..2].try_into().unwrap()) as usize;
                    if payload.len() != 2 + count * 8 {
                        return Err(Error::InvalidInput(
                            "Excel merged-cell record has an invalid length".into(),
                        ));
                    }
                    total_merged_ranges = total_merged_ranges.saturating_add(count);
                    if total_merged_ranges > MAX_XLS_MERGED_RANGES {
                        return Err(Error::LimitExceeded(format!(
                            "Excel workbook contains more than {MAX_XLS_MERGED_RANGES} merged ranges"
                        )));
                    }
                    for region in payload[2..].chunks_exact(8) {
                        let first_row = u16::from_le_bytes(region[0..2].try_into().unwrap()) as u32;
                        let last_row = u16::from_le_bytes(region[2..4].try_into().unwrap()) as u32;
                        let first_column =
                            u16::from_le_bytes(region[4..6].try_into().unwrap()) as u32;
                        let last_column =
                            u16::from_le_bytes(region[6..8].try_into().unwrap()) as u32;
                        if first_row > last_row
                            || last_row >= XLS_MAX_ROWS
                            || first_column > last_column
                            || last_column >= XLS_MAX_COLUMNS
                        {
                            return Err(Error::InvalidInput(
                                "Excel merged-cell range exceeds BIFF sheet bounds".into(),
                            ));
                        }
                    }
                }
                0x000a => {
                    found_sheet_eof = true;
                    break;
                }
                _ => {}
            }
            if total_cells > MAX_XLS_CELLS_TOTAL {
                return Err(Error::LimitExceeded(format!(
                    "Excel workbook contains more than {MAX_XLS_CELLS_TOTAL} cell records"
                )));
            }
        }
        if !found_sheet_eof {
            return Err(Error::InvalidInput(format!(
                "Excel worksheet {sheet_index} is missing its EOF record"
            )));
        }
        bounds.check_dense_bounds(sheet_index)?;
        let sheet_sst_bytes = bounds
            .sst_references
            .iter()
            .try_fold(0usize, |sum, index| {
                let length = sst_lengths.get(*index).copied().unwrap_or(0);
                sum.checked_add(length)
                    .ok_or_else(|| Error::LimitExceeded("Excel cell text size overflowed".into()))
            })?;
        if total_inline_text
            .checked_add(sheet_sst_bytes)
            .is_none_or(|bytes| bytes > MAX_XLS_EXPANDED_TEXT_BYTES)
        {
            return Err(Error::LimitExceeded(format!(
                "Excel worksheet text expands beyond {MAX_XLS_EXPANDED_TEXT_BYTES} bytes"
            )));
        }
        total_inline_text = total_inline_text.saturating_add(sheet_sst_bytes);
    }
    if total_inline_text.saturating_add(total_sst_bytes) > MAX_XLS_EXPANDED_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Excel workbook strings expand beyond {MAX_XLS_EXPANDED_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn peek_biff_record(stream: &[u8], offset: usize) -> Result<Option<(u16, &[u8])>> {
    if offset == stream.len() {
        return Ok(None);
    }
    if stream.len().saturating_sub(offset) < 4 {
        return Err(Error::InvalidInput(
            "truncated Excel BIFF record header".into(),
        ));
    }
    let kind = u16::from_le_bytes(stream[offset..offset + 2].try_into().unwrap());
    let length = u16::from_le_bytes(stream[offset + 2..offset + 4].try_into().unwrap()) as usize;
    if length > MAX_BIFF_RECORD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Excel BIFF record length {length} exceeds {MAX_BIFF_RECORD_BYTES} bytes"
        )));
    }
    let start = offset + 4;
    let end = start
        .checked_add(length)
        .ok_or_else(|| Error::InvalidInput("Excel BIFF record size overflowed".into()))?;
    if end > stream.len() {
        return Err(Error::InvalidInput(
            "Excel BIFF record extends beyond the stream".into(),
        ));
    }
    Ok(Some((kind, &stream[start..end])))
}

fn next_biff_record<'a>(stream: &'a [u8], offset: &mut usize) -> Result<Option<(u16, &'a [u8])>> {
    let record = peek_biff_record(stream, *offset)?;
    if let Some((_, payload)) = record {
        *offset = offset
            .checked_add(4 + payload.len())
            .ok_or_else(|| Error::InvalidInput("Excel BIFF record offset overflowed".into()))?;
    }
    Ok(record)
}

fn next_biff_record_until<'a>(
    stream: &'a [u8],
    offset: &mut usize,
    end: usize,
) -> Result<Option<(u16, &'a [u8])>> {
    if *offset == end {
        return Ok(None);
    }
    if *offset > end || end > stream.len() || end - *offset < 4 {
        return Err(Error::InvalidInput(
            "invalid Excel worksheet record boundary".into(),
        ));
    }
    let record = next_biff_record(stream, offset)?;
    if *offset > end {
        return Err(Error::InvalidInput(
            "Excel worksheet record crosses sheet boundary".into(),
        ));
    }
    Ok(record)
}

fn collect_continuations<'a>(
    stream: &'a [u8],
    offset: &mut usize,
    record_count: &mut usize,
    first_segment: &'a [u8],
) -> Result<Option<Vec<&'a [u8]>>> {
    let mut segments = Vec::new();
    while let Some((0x003c, _)) = peek_biff_record(stream, *offset)? {
        let (_, payload) = next_biff_record(stream, offset)?.unwrap();
        *record_count = record_count.saturating_add(1);
        if *record_count > MAX_XLS_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "Excel workbook exceeds {MAX_XLS_RECORDS} BIFF records"
            )));
        }
        segments.push(payload);
    }
    if segments.is_empty() {
        Ok(None)
    } else {
        segments.insert(0, first_segment);
        Ok(Some(segments))
    }
}

fn parse_dimensions(payload: &[u8]) -> Result<(u32, u32, u32, u32)> {
    let (first_row, last_row, mut first_column, last_column) = match payload.len() {
        10 => (
            u16::from_le_bytes(payload[0..2].try_into().unwrap()) as u32,
            u16::from_le_bytes(payload[2..4].try_into().unwrap()) as u32,
            u16::from_le_bytes(payload[4..6].try_into().unwrap()) as u32,
            u16::from_le_bytes(payload[6..8].try_into().unwrap()) as u32,
        ),
        14 => (
            u32::from_le_bytes(payload[0..4].try_into().unwrap()),
            u32::from_le_bytes(payload[4..8].try_into().unwrap()),
            u16::from_le_bytes(payload[8..10].try_into().unwrap()) as u32,
            u16::from_le_bytes(payload[10..12].try_into().unwrap()) as u32,
        ),
        _ => {
            return Err(Error::InvalidInput(format!(
                "Excel DIMENSIONS record length {} is invalid",
                payload.len()
            )));
        }
    };
    if first_column > 0xff || last_column < first_column {
        first_column = 0;
    }
    if last_row < first_row || last_row > XLS_MAX_ROWS || last_column > XLS_MAX_COLUMNS {
        return Err(Error::InvalidInput(
            "Excel DIMENSIONS record is outside BIFF bounds".into(),
        ));
    }
    Ok((first_row, last_row, first_column, last_column))
}

fn parse_sst_lengths(segments: &[&[u8]]) -> Result<Vec<usize>> {
    let mut cursor = SstCursor {
        segments,
        segment_index: 0,
        position: 0,
    };
    let _total = cursor.read_u32()?;
    let unique = cursor.read_u32()? as usize;
    if unique > MAX_XLS_CELLS_TOTAL {
        return Err(Error::LimitExceeded(format!(
            "Excel shared string table contains more than {MAX_XLS_CELLS_TOTAL} unique strings"
        )));
    }
    let mut lengths = Vec::with_capacity(unique);
    let mut total_bytes = 0usize;
    for _ in 0..unique {
        let character_count = cursor.read_u16()? as usize;
        let flags = cursor.read_u8()?;
        let runs = if flags & 0x08 != 0 {
            cursor.read_u16()? as usize
        } else {
            0
        };
        let extension = if flags & 0x04 != 0 {
            cursor.read_u32()? as usize
        } else {
            0
        };
        let utf8_upper_bound = character_count.saturating_mul(3);
        total_bytes = total_bytes.saturating_add(utf8_upper_bound);
        if total_bytes > MAX_XLS_EXPANDED_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Excel shared string table expands beyond {MAX_XLS_EXPANDED_TEXT_BYTES} bytes"
            )));
        }
        cursor.skip_characters(character_count, flags & 0x01 != 0)?;
        cursor.skip(runs.saturating_mul(4))?;
        cursor.skip(extension)?;
        lengths.push(utf8_upper_bound);
    }
    Ok(lengths)
}

struct SstCursor<'a, 'b> {
    segments: &'b [&'a [u8]],
    segment_index: usize,
    position: usize,
}

impl SstCursor<'_, '_> {
    fn read_u8(&mut self) -> Result<u8> {
        if self.segment_index >= self.segments.len()
            || self.position >= self.segments[self.segment_index].len()
        {
            return Err(Error::InvalidInput(
                "truncated Excel shared string header".into(),
            ));
        }
        let value = self.segments[self.segment_index][self.position];
        self.position += 1;
        Ok(value)
    }

    fn read_u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.read_array::<2>()?))
    }

    fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read_array::<4>()?))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        if self.segment_index >= self.segments.len()
            || self.position + N > self.segments[self.segment_index].len()
        {
            return Err(Error::InvalidInput(
                "truncated Excel shared string header".into(),
            ));
        }
        let value = self.segments[self.segment_index][self.position..self.position + N]
            .try_into()
            .unwrap();
        self.position += N;
        Ok(value)
    }

    fn skip(&mut self, mut bytes: usize) -> Result<()> {
        while bytes > 0 {
            if self.segment_index >= self.segments.len() {
                return Err(Error::InvalidInput(
                    "truncated Excel shared string payload".into(),
                ));
            }
            let remaining = self.segments[self.segment_index]
                .len()
                .saturating_sub(self.position);
            if remaining == 0 {
                self.segment_index += 1;
                self.position = 0;
                continue;
            }
            let skipped = bytes.min(remaining);
            self.position += skipped;
            bytes -= skipped;
        }
        Ok(())
    }

    fn skip_characters(&mut self, mut characters: usize, mut high_byte: bool) -> Result<()> {
        while characters > 0 {
            if self.segment_index >= self.segments.len() {
                return Err(Error::InvalidInput(
                    "truncated Excel shared string characters".into(),
                ));
            }
            let width = if high_byte { 2 } else { 1 };
            let remaining_bytes = self.segments[self.segment_index]
                .len()
                .saturating_sub(self.position);
            let available = remaining_bytes / width;
            if available == 0 {
                self.segment_index += 1;
                self.position = 0;
                if self.segment_index >= self.segments.len() {
                    return Err(Error::InvalidInput(
                        "truncated Excel shared string continuation".into(),
                    ));
                }
                high_byte = self.read_u8()? & 0x01 != 0;
                continue;
            }
            let consumed = characters.min(available);
            self.position += consumed * width;
            characters -= consumed;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(payload.len() + 4);
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn workbook_with_sheet(global_records: Vec<Vec<u8>>, sheet_records: Vec<Vec<u8>>) -> Vec<u8> {
        let bof = record(0x0809, &[0x00, 0x06, 0x05, 0x00]);
        let eof = record(0x000a, &[]);
        let provisional_boundsheet = record(0x0085, &[0; 9]);
        let sheet_offset = bof.len()
            + provisional_boundsheet.len()
            + global_records.iter().map(Vec::len).sum::<usize>()
            + eof.len();
        let mut boundsheet = Vec::new();
        boundsheet.extend_from_slice(&(sheet_offset as u32).to_le_bytes());
        boundsheet.extend_from_slice(&[0, 0, 1, 0, b'S']);
        let mut workbook = bof;
        workbook.extend_from_slice(&record(0x0085, &boundsheet));
        for bytes in global_records {
            workbook.extend_from_slice(&bytes);
        }
        workbook.extend_from_slice(&eof);
        for bytes in sheet_records {
            workbook.extend_from_slice(&bytes);
        }
        workbook
    }

    fn dimensions(first_row: u32, last_row: u32, first_column: u16, last_column: u16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&first_row.to_le_bytes());
        bytes.extend_from_slice(&last_row.to_le_bytes());
        bytes.extend_from_slice(&first_column.to_le_bytes());
        bytes.extend_from_slice(&last_column.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes
    }

    #[test]
    fn rejects_legacy_xls_dimensions_before_allocating_the_dense_range() {
        let sheet = vec![
            record(0x0809, &[0x00, 0x06, 0x10, 0x00]),
            record(
                0x0200,
                &dimensions(0, XLS_MAX_ROWS, 0, XLS_MAX_COLUMNS as u16),
            ),
            record(0x000a, &[]),
        ];
        let workbook = workbook_with_sheet(Vec::new(), sheet);
        let error = validate_biff_stream(&workbook).unwrap_err();
        assert!(format!("{error}").contains("declares"));
    }

    #[test]
    fn bounds_repeated_references_to_large_shared_strings() {
        let mut sst = Vec::new();
        sst.extend_from_slice(&3_000u32.to_le_bytes());
        sst.extend_from_slice(&1u32.to_le_bytes());
        sst.extend_from_slice(&40_000u16.to_le_bytes());
        sst.push(0);
        sst.extend(std::iter::repeat_n(b'x', 8_213));
        let mut globals = vec![record(0x00fc, &sst)];
        let remaining = 40_000 - 8_213;
        let mut chars = vec![b'x'; remaining];
        while !chars.is_empty() {
            let chunk_size = chars.len().min(MAX_BIFF_RECORD_BYTES - 1);
            let mut continuation = vec![0];
            continuation.extend(chars.drain(..chunk_size));
            globals.push(record(0x003c, &continuation));
        }

        let mut sheet = vec![
            record(0x0809, &[0x00, 0x06, 0x10, 0x00]),
            record(0x0200, &dimensions(0, 3_000, 0, 1)),
        ];
        for row in 0..3_000u16 {
            let mut cell = Vec::with_capacity(10);
            cell.extend_from_slice(&row.to_le_bytes());
            cell.extend_from_slice(&0u16.to_le_bytes());
            cell.extend_from_slice(&0u16.to_le_bytes());
            cell.extend_from_slice(&0u32.to_le_bytes());
            sheet.push(record(0x00fd, &cell));
        }
        sheet.push(record(0x000a, &[]));
        let workbook = workbook_with_sheet(std::mem::take(&mut globals), sheet);
        let error = validate_biff_stream(&workbook).unwrap_err();
        assert!(format!("{error}").contains("text expands"));
    }

    #[test]
    fn formats_excel_column_labels() {
        assert_eq!(excel_column_label(0), "A");
        assert_eq!(excel_column_label(25), "Z");
        assert_eq!(excel_column_label(26), "AA");
        assert_eq!(excel_column_label(27), "AB");
        assert_eq!(excel_column_label(255), "IV");
    }
}
