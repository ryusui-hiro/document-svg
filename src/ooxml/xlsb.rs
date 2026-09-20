//! Bounded previews of Excel Binary Workbook (`.xlsb`) packages.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use calamine::{Data, DataRef, Reader, SheetType, SheetVisible, Xlsb};
use quick_xml::Reader as XmlReader;
use quick_xml::events::Event;
use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::Page;
use crate::table::{TableAlign, TableData, layout_and_render_table};

const MAX_XLSB_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XLSB_ENTRY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XLSB_TOTAL_UNCOMPRESSED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XLSB_WORKBOOK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_XLSB_STRING_TABLE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_XLSB_STYLES_BYTES: u64 = 16 * 1024 * 1024;
const MAX_XLSB_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_XLSB_RECORDS: usize = 2_000_000;
const MAX_XLSB_DEFINED_NAMES: usize = 10_000;
const MAX_XLSB_NAME_FORMULA_BYTES: usize = 1024 * 1024;
const MAX_XLSB_EXTERN_SHEET_REFS: usize = 100_000;
const MAX_XLSB_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_XLSB_SHEETS: usize = 1_000;
const MAX_XLSB_CELLS_PER_SHEET: usize = 500_000;
const MAX_XLSB_CELLS_TOTAL: usize = 1_000_000;
const MAX_XLSB_DENSE_CELLS_TOTAL: usize = 2_000_000;
const MAX_XLSB_EXPANDED_TEXT_BYTES: usize = 128 * 1024 * 1024;
const MAX_XLSB_CELL_TEXT_CHARS: usize = 32_767;
const MAX_RELATIONSHIP_XML_BYTES: u64 = 1024 * 1024;
const MAX_RELATIONSHIP_EVENTS: usize = 100_000;
const PAGE_ROWS: usize = 40;
const PAGE_COLUMNS: usize = 12;
const XLSB_MAX_ROWS: u32 = 1_048_576;
const XLSB_MAX_COLUMNS: u32 = 16_384;

struct XlsbPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for XlsbPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "xlsb".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Clone, Debug)]
struct CellValue {
    row: u32,
    column: u32,
    value: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct Dimensions {
    first_row: u32,
    last_row: u32,
    first_column: u32,
    last_column: u32,
}

impl Dimensions {
    fn rows(self) -> usize {
        (self.last_row - self.first_row + 1) as usize
    }

    fn columns(self) -> usize {
        (self.last_column - self.first_column + 1) as usize
    }

    fn area(self) -> usize {
        self.rows().saturating_mul(self.columns())
    }
}

pub(crate) fn convert(
    path: &std::path::Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_input = options.max_input_bytes.min(MAX_XLSB_INPUT_BYTES);
    let bytes = read_limited_file(path, max_input, "XLSB workbook")?;
    preflight(&bytes)?;
    let workbook_bytes = Arc::<[u8]>::from(bytes);
    let cursor = Cursor::new(workbook_bytes);
    let mut workbook: Xlsb<_> = catch_unwind(AssertUnwindSafe(|| Xlsb::new(cursor)))
        .map_err(|_| {
            Error::InvalidInput("XLSB parser rejected a malformed workbook record".into())
        })?
        .map_err(|error| Error::InvalidInput(format!("invalid XLSB workbook: {error}")))?;
    let workbook_sheets = workbook.sheets_metadata().to_vec();
    if workbook_sheets.is_empty() {
        return Err(Error::InvalidInput(
            "XLSB workbook contains no sheets".into(),
        ));
    }
    if workbook_sheets.len() > MAX_XLSB_SHEETS {
        return Err(Error::LimitExceeded(format!(
            "XLSB workbook contains {} sheets; maximum is {MAX_XLSB_SHEETS}",
            workbook_sheets.len()
        )));
    }
    let worksheet_names = workbook_sheets
        .iter()
        .filter(|sheet| sheet.typ == SheetType::WorkSheet)
        .map(|sheet| sheet.name.clone())
        .collect::<Vec<_>>();
    if worksheet_names.is_empty() {
        return Err(Error::Unsupported(
            "XLSB file has no standard worksheets to preview".into(),
        ));
    }

    let mut warnings = vec![
        "XLSB cells are shown as values; formula recalculation, cell styles, merged-cell layout, charts, and drawings are not reproduced".into(),
        "VBA macros are never executed; chartsheets and dialog sheets are omitted".into(),
    ];
    if workbook_sheets
        .iter()
        .any(|sheet| sheet.typ != SheetType::WorkSheet)
    {
        warnings.push("non-worksheet Excel sheet types were omitted".into());
    }
    if workbook_sheets
        .iter()
        .any(|sheet| sheet.visible != SheetVisible::Visible)
    {
        warnings.push("hidden and very-hidden worksheets are included in the preview".into());
    }
    let mut page_sink = XlsbPageSink {
        inner: sink,
        warnings: &warnings,
    };
    let mut page_number = 0usize;
    let mut total_cells = 0usize;
    let mut total_text_bytes = 0usize;
    for sheet_name in worksheet_names {
        let (dimensions, mut cells, sheet_text_bytes) = catch_unwind(AssertUnwindSafe(|| {
            read_xlsb_sheet(&mut workbook, &sheet_name)
        }))
        .map_err(|_| {
            Error::InvalidInput(format!(
                "XLSB worksheet '{sheet_name}' contains a malformed binary record"
            ))
        })??;
        total_cells = total_cells.saturating_add(cells.len());
        total_text_bytes = total_text_bytes.saturating_add(sheet_text_bytes);
        if total_cells > MAX_XLSB_CELLS_TOTAL {
            return Err(Error::LimitExceeded(format!(
                "XLSB workbook contains more than {MAX_XLSB_CELLS_TOTAL} populated cells"
            )));
        }
        if total_text_bytes > MAX_XLSB_EXPANDED_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XLSB cell text exceeds {MAX_XLSB_EXPANDED_TEXT_BYTES} expanded bytes"
            )));
        }
        if dimensions.area() > MAX_XLSB_CELLS_PER_SHEET {
            return Err(Error::LimitExceeded(format!(
                "XLSB worksheet '{sheet_name}' spans {} cells; maximum is {MAX_XLSB_CELLS_PER_SHEET}",
                dimensions.area()
            )));
        }
        let row_pages = dimensions.rows().max(1).div_ceil(PAGE_ROWS);
        let column_pages = dimensions.columns().max(1).div_ceil(PAGE_COLUMNS);
        let new_page_total = page_number
            .checked_add(row_pages.saturating_mul(column_pages))
            .ok_or_else(|| Error::LimitExceeded("XLSB page count overflowed".into()))?;
        if new_page_total > options.max_pages {
            return Err(Error::LimitExceeded(format!(
                "XLSB workbook pagination would create {new_page_total} pages; maximum is {}",
                options.max_pages
            )));
        }
        cells.sort_by_key(|cell| (cell.row, cell.column));
        let columns = dimensions.columns();
        for row_offset in (0..dimensions.rows()).step_by(PAGE_ROWS) {
            let row_count = (dimensions.rows() - row_offset).min(PAGE_ROWS);
            let row_start = dimensions.first_row + row_offset as u32;
            let row_end = row_start + row_count as u32;
            let first_cell = cells.partition_point(|cell| cell.row < row_start);
            let end_cell = cells.partition_point(|cell| cell.row < row_end);
            let mut matrix = vec![String::new(); row_count.saturating_mul(columns)];
            for cell in &mut cells[first_cell..end_cell] {
                let row = (cell.row - row_start) as usize;
                let column = (cell.column - dimensions.first_column) as usize;
                let slot = row
                    .checked_mul(columns)
                    .and_then(|offset| offset.checked_add(column))
                    .ok_or_else(|| Error::LimitExceeded("XLSB cell index overflowed".into()))?;
                matrix[slot] = cell.value.take().unwrap_or_default();
            }
            for column_offset in (0..columns).step_by(PAGE_COLUMNS) {
                let visible_columns = (columns - column_offset).min(PAGE_COLUMNS);
                let mut table = TableData {
                    headers: (0..visible_columns)
                        .map(|column| {
                            excel_column_label(
                                dimensions.first_column as usize + column_offset + column,
                            )
                        })
                        .collect(),
                    rows: Vec::with_capacity(row_count),
                    alignments: vec![TableAlign::Left; visible_columns],
                    raw_source: String::new(),
                };
                for row in 0..row_count {
                    let mut values = Vec::with_capacity(visible_columns);
                    let row_start_index = row * columns + column_offset;
                    for column in 0..visible_columns {
                        values.push(std::mem::take(&mut matrix[row_start_index + column]));
                    }
                    table.rows.push(values);
                }
                page_number += 1;
                let mut page = layout_and_render_table(&table, options)?;
                page.number = page_number;
                page.title = format!(
                    "{sheet_name} — rows {}–{}, columns {}–{}",
                    row_start + 1,
                    row_end,
                    excel_column_label(dimensions.first_column as usize + column_offset),
                    excel_column_label(
                        dimensions.first_column as usize + column_offset + visible_columns - 1
                    )
                );
                page.description = format!(
                    "Excel Binary Workbook worksheet '{sheet_name}', rows {}–{}, columns {}–{}",
                    row_start + 1,
                    row_end,
                    excel_column_label(dimensions.first_column as usize + column_offset),
                    excel_column_label(
                        dimensions.first_column as usize + column_offset + visible_columns - 1
                    )
                );
                page_sink.consume(page)?;
            }
        }
    }
    Ok(warnings)
}

fn read_xlsb_sheet(
    workbook: &mut Xlsb<Cursor<Arc<[u8]>>>,
    sheet_name: &str,
) -> Result<(Dimensions, Vec<CellValue>, usize)> {
    let mut reader = workbook
        .worksheet_cells_reader(sheet_name)
        .map_err(|error| {
            Error::InvalidInput(format!(
                "cannot read XLSB worksheet '{sheet_name}': {error}"
            ))
        })?;
    let raw = reader.dimensions();
    if raw.end.0 < raw.start.0
        || raw.end.1 < raw.start.1
        || raw.end.0 >= XLSB_MAX_ROWS
        || raw.end.1 >= XLSB_MAX_COLUMNS
    {
        return Err(Error::InvalidInput(format!(
            "XLSB worksheet '{sheet_name}' has invalid used-range bounds"
        )));
    }
    let dimensions = Dimensions {
        first_row: raw.start.0,
        last_row: raw.end.0,
        first_column: raw.start.1,
        last_column: raw.end.1,
    };
    if dimensions.area() > MAX_XLSB_CELLS_PER_SHEET {
        return Err(Error::LimitExceeded(format!(
            "XLSB worksheet '{sheet_name}' spans {} cells; maximum is {MAX_XLSB_CELLS_PER_SHEET}",
            dimensions.area()
        )));
    }
    let mut cells = Vec::with_capacity(dimensions.area().min(16_384));
    let mut text_bytes = 0usize;
    while let Some(cell) = reader
        .next_cell()
        .map_err(|error| Error::InvalidInput(format!("invalid XLSB cell: {error}")))?
    {
        let (row, column) = cell.get_position();
        if !dimensions_contains(dimensions, row, column) {
            return Err(Error::InvalidInput(format!(
                "XLSB worksheet '{sheet_name}' has a cell outside its declared used range"
            )));
        }
        if cells.len() >= MAX_XLSB_CELLS_PER_SHEET {
            return Err(Error::LimitExceeded(format!(
                "XLSB worksheet '{sheet_name}' contains more than {MAX_XLSB_CELLS_PER_SHEET} cells"
            )));
        }
        let value = data_ref_to_string(cell.get_value().clone());
        if value.len() > MAX_XLSB_CELL_TEXT_CHARS * 3 {
            return Err(Error::LimitExceeded(format!(
                "XLSB worksheet '{sheet_name}' contains a cell larger than {} expanded bytes",
                MAX_XLSB_CELL_TEXT_CHARS * 3
            )));
        }
        text_bytes = text_bytes.saturating_add(value.len());
        if text_bytes > MAX_XLSB_EXPANDED_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XLSB worksheet '{sheet_name}' text exceeds {MAX_XLSB_EXPANDED_TEXT_BYTES} bytes"
            )));
        }
        cells.push(CellValue {
            row,
            column,
            value: Some(value),
        });
    }
    Ok((dimensions, cells, text_bytes))
}

fn data_ref_to_string(value: DataRef<'_>) -> String {
    let value: Data = value.into();
    match value {
        Data::Empty => String::new(),
        value => value.to_string(),
    }
}

fn dimensions_contains(dimensions: Dimensions, row: u32, column: u32) -> bool {
    row >= dimensions.first_row
        && row <= dimensions.last_row
        && column >= dimensions.first_column
        && column <= dimensions.last_column
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

fn preflight(bytes: &[u8]) -> Result<()> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("invalid XLSB ZIP package: {error}")))?;
    if archive.len() > MAX_XLSB_ARCHIVE_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "XLSB package contains {} entries; maximum is {MAX_XLSB_ARCHIVE_ENTRIES}",
            archive.len()
        )));
    }
    let mut names = HashSet::with_capacity(archive.len());
    let mut total_uncompressed = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name();
        if name.len() > 4096
            || name.starts_with('/')
            || name.contains('\\')
            || name.split('/').any(|part| part == "..")
        {
            return Err(Error::InvalidInput(
                "XLSB package has an unsafe part name".into(),
            ));
        }
        if !names.insert(name.to_owned()) {
            return Err(Error::InvalidInput(format!(
                "XLSB package repeats ZIP entry '{name}'"
            )));
        }
        if entry.size() > MAX_XLSB_ENTRY_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XLSB package entry '{name}' is {} bytes; maximum is {MAX_XLSB_ENTRY_BYTES}",
                entry.size()
            )));
        }
        total_uncompressed = total_uncompressed.saturating_add(entry.size());
        if total_uncompressed > MAX_XLSB_TOTAL_UNCOMPRESSED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XLSB package expands beyond {MAX_XLSB_TOTAL_UNCOMPRESSED_BYTES} bytes"
            )));
        }
    }
    for required in ["xl/workbook.bin", "xl/_rels/workbook.bin.rels"] {
        if !names.contains(required) {
            return Err(Error::InvalidInput(format!(
                "XLSB package is missing {required}"
            )));
        }
    }
    let workbook_bin = read_entry(&mut archive, "xl/workbook.bin", MAX_XLSB_WORKBOOK_BYTES)?;
    let rels_xml = read_entry(
        &mut archive,
        "xl/_rels/workbook.bin.rels",
        MAX_RELATIONSHIP_XML_BYTES,
    )?;
    let relationships = parse_relationships(&rels_xml)?;
    let (worksheet_paths, mut record_count) = parse_workbook_bin(&workbook_bin, &relationships)?;
    if worksheet_paths.is_empty() || worksheet_paths.len() > MAX_XLSB_SHEETS {
        return Err(Error::InvalidInput(
            "XLSB workbook has no bounded worksheet list".into(),
        ));
    }
    let shared_string_lengths = if names.contains("xl/sharedStrings.bin") {
        let bytes = read_entry(
            &mut archive,
            "xl/sharedStrings.bin",
            MAX_XLSB_STRING_TABLE_BYTES,
        )?;
        let (strings, records) = scan_shared_strings(&bytes)?;
        record_count = record_count.saturating_add(records);
        if record_count > MAX_XLSB_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "XLSB workbook exceeds {MAX_XLSB_RECORDS} binary records"
            )));
        }
        strings
    } else {
        Vec::new()
    };
    let shared_string_bytes = shared_string_lengths
        .iter()
        .try_fold(0usize, |sum, length| {
            sum.checked_add(*length)
                .ok_or_else(|| Error::LimitExceeded("XLSB shared-string size overflowed".into()))
        })?;
    let mut total_cells = 0usize;
    let mut total_dense_cells = 0usize;
    let mut total_text = 0usize;
    let mut seen_paths = HashSet::new();
    for path in &worksheet_paths {
        if !seen_paths.insert(path.clone()) {
            return Err(Error::InvalidInput(
                "XLSB workbook repeats a worksheet path".into(),
            ));
        }
        if !names.contains(path) {
            return Err(Error::InvalidInput(format!(
                "XLSB worksheet part '{path}' is missing"
            )));
        }
        let sheet_bytes = read_entry(&mut archive, path, MAX_XLSB_ENTRY_BYTES)?;
        let (cells, dense_cells, text_bytes) =
            scan_worksheet(&sheet_bytes, &mut record_count, &shared_string_lengths)?;
        total_cells = total_cells.saturating_add(cells);
        total_dense_cells = total_dense_cells.saturating_add(dense_cells);
        total_text = total_text.saturating_add(text_bytes);
        if total_cells > MAX_XLSB_CELLS_TOTAL
            || total_dense_cells > MAX_XLSB_DENSE_CELLS_TOTAL
            || total_text > MAX_XLSB_EXPANDED_TEXT_BYTES
        {
            return Err(Error::LimitExceeded(
                "XLSB workbook exceeds the aggregate worksheet budgets".into(),
            ));
        }
    }
    let style_text_bytes = if names.contains("xl/styles.bin") {
        let styles = read_entry(&mut archive, "xl/styles.bin", MAX_XLSB_STYLES_BYTES)?;
        scan_styles(&styles, &mut record_count)?
    } else {
        0
    };
    if record_count > MAX_XLSB_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "XLSB workbook exceeds {MAX_XLSB_RECORDS} binary records"
        )));
    }
    if total_text
        .saturating_add(shared_string_bytes)
        .saturating_add(style_text_bytes)
        > MAX_XLSB_EXPANDED_TEXT_BYTES
    {
        return Err(Error::LimitExceeded(format!(
            "XLSB expanded text exceeds {MAX_XLSB_EXPANDED_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn read_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>> {
    let entry = archive.by_name(name)?;
    if entry.size() > limit {
        return Err(Error::LimitExceeded(format!(
            "XLSB part '{name}' exceeds {limit} bytes"
        )));
    }
    let expected = entry.size();
    let mut bytes = Vec::with_capacity(usize::try_from(expected).unwrap_or(0).min(8 * 1024 * 1024));
    entry
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != expected {
        return Err(Error::InvalidInput(format!(
            "XLSB part '{name}' has an invalid expanded size"
        )));
    }
    Ok(bytes)
}

fn parse_relationships(xml: &[u8]) -> Result<HashMap<String, Option<String>>> {
    let mut reader = XmlReader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut relationships = HashMap::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > MAX_RELATIONSHIP_EVENTS {
            return Err(Error::LimitExceeded(
                "XLSB relationship XML exceeds its event budget".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) | Event::Empty(element)
                if crate::ooxml::local_name(element.name().as_ref()) == b"Relationship" =>
            {
                let id = crate::ooxml::attribute(&element, b"Id").unwrap_or_default();
                let target = crate::ooxml::attribute(&element, b"Target").unwrap_or_default();
                let external = crate::ooxml::attribute(&element, b"TargetMode")
                    .is_some_and(|mode| mode.eq_ignore_ascii_case("External"));
                if id.is_empty() || target.is_empty() || relationships.contains_key(&id) {
                    return Err(Error::InvalidInput(
                        "XLSB relationship entry is incomplete or duplicated".into(),
                    ));
                }
                relationships.insert(id, (!external).then_some(target));
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "XLSB relationship XML may not declare a document type".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(relationships)
}

fn parse_workbook_bin(
    bytes: &[u8],
    relationships: &HashMap<String, Option<String>>,
) -> Result<(Vec<String>, usize)> {
    let mut offset = 0usize;
    let mut count = 0usize;
    let mut sheet_paths = Vec::new();
    let mut sheet_count = 0usize;
    let mut defined_name_count = 0usize;
    let mut defined_name_formula_bytes = 0usize;
    let mut ended_sheets = false;
    let mut saw_post_sheet_terminator = false;
    while let Some((kind, payload)) = next_record(bytes, &mut offset, &mut count)? {
        match kind {
            0x0099 if payload.is_empty() => {
                return Err(Error::InvalidInput(
                    "XLSB workbook properties record is empty".into(),
                ));
            }
            0x0099 => {}
            0x009c => {
                if payload.len() < 12 {
                    return Err(Error::InvalidInput(
                        "XLSB sheet-directory record is truncated".into(),
                    ));
                }
                let state = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                if state > 2 {
                    return Err(Error::InvalidInput(
                        "XLSB sheet has an invalid visibility state".into(),
                    ));
                }
                let id_length = u32::from_le_bytes(payload[8..12].try_into().unwrap());
                if id_length == u32::MAX {
                    continue;
                }
                let id_bytes = usize::try_from(id_length)
                    .ok()
                    .and_then(|length| length.checked_mul(2))
                    .ok_or_else(|| {
                        Error::LimitExceeded("XLSB relationship id length overflowed".into())
                    })?;
                let id_end = 12usize.checked_add(id_bytes).ok_or_else(|| {
                    Error::LimitExceeded("XLSB relationship id offset overflowed".into())
                })?;
                if id_bytes > 4096 || payload.len() < id_end + 4 {
                    return Err(Error::InvalidInput(
                        "XLSB sheet relationship id is malformed".into(),
                    ));
                }
                let id_utf16 = payload[12..id_end]
                    .chunks_exact(2)
                    .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                    .collect::<Vec<_>>();
                let id = String::from_utf16(&id_utf16).map_err(|_| {
                    Error::InvalidInput("XLSB sheet relationship id is not UTF-16".into())
                })?;
                let target = relationships
                    .get(&id)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "XLSB sheet relationship '{id}' is missing or external"
                        ))
                    })?;
                if target.starts_with('/')
                    || target.contains('\\')
                    || target.split('/').any(|part| part == "..")
                {
                    return Err(Error::InvalidInput(
                        "XLSB worksheet relationship escapes the package".into(),
                    ));
                }
                let package_path = format!("xl/{target}");
                let name_chars =
                    u32::from_le_bytes(payload[id_end..id_end + 4].try_into().unwrap()) as usize;
                let name_len = 4usize
                    .checked_add(name_chars.checked_mul(2).ok_or_else(|| {
                        Error::LimitExceeded("XLSB sheet name length overflowed".into())
                    })?)
                    .ok_or_else(|| {
                        Error::LimitExceeded("XLSB sheet name length overflowed".into())
                    })?;
                if name_chars > 255 || payload.len() < id_end + name_len {
                    return Err(Error::InvalidInput(
                        "XLSB worksheet name is malformed".into(),
                    ));
                }
                let is_worksheet =
                    package_path.starts_with("xl/worksheets/") && package_path.ends_with(".bin");
                let is_supported_sheet = is_worksheet
                    || package_path.starts_with("xl/chartsheets/")
                    || package_path.starts_with("xl/dialogsheets/");
                if !is_supported_sheet {
                    return Err(Error::Unsupported(format!(
                        "unsupported XLSB sheet part '{package_path}'"
                    )));
                }
                if is_worksheet {
                    sheet_paths.push(package_path);
                    if sheet_paths.len() > MAX_XLSB_SHEETS {
                        return Err(Error::LimitExceeded(format!(
                            "XLSB has more than {MAX_XLSB_SHEETS} worksheets"
                        )));
                    }
                }
                let name_bytes = &payload[id_end + 4..id_end + name_len];
                let name_utf16 = name_bytes
                    .chunks_exact(2)
                    .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                    .collect::<Vec<_>>();
                let _sheet_name = String::from_utf16(&name_utf16)
                    .map_err(|_| Error::InvalidInput("XLSB worksheet name is not UTF-16".into()))?;
                sheet_count += 1;
                if sheet_count > MAX_XLSB_SHEETS {
                    return Err(Error::LimitExceeded(format!(
                        "XLSB has more than {MAX_XLSB_SHEETS} sheets"
                    )));
                }
            }
            0x0090 => {
                ended_sheets = true;
                break;
            }
            _ => {}
        }
    }
    if !ended_sheets || sheet_count == 0 {
        return Err(Error::InvalidInput(
            "XLSB workbook is missing its sheet-directory terminator".into(),
        ));
    }
    while let Some((kind, payload)) = next_record(bytes, &mut offset, &mut count)? {
        match kind {
            0x016a => {
                if payload.len() < 4 {
                    return Err(Error::InvalidInput(
                        "XLSB external-sheet record is truncated".into(),
                    ));
                }
                let references = u32::from_le_bytes(payload[0..4].try_into().unwrap()) as usize;
                if references > MAX_XLSB_EXTERN_SHEET_REFS
                    || payload.len() < 4 + references.saturating_mul(12)
                {
                    return Err(Error::InvalidInput(
                        "XLSB external-sheet table is malformed".into(),
                    ));
                }
            }
            0x0027 => {
                defined_name_count = defined_name_count.saturating_add(1);
                if defined_name_count > MAX_XLSB_DEFINED_NAMES {
                    return Err(Error::LimitExceeded(format!(
                        "XLSB contains more than {MAX_XLSB_DEFINED_NAMES} defined names"
                    )));
                }
                let formula_bytes = validate_defined_name(payload)?;
                defined_name_formula_bytes =
                    defined_name_formula_bytes.saturating_add(formula_bytes);
                if defined_name_formula_bytes > MAX_XLSB_NAME_FORMULA_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "XLSB defined-name formulas exceed {MAX_XLSB_NAME_FORMULA_BYTES} bytes"
                    )));
                }
            }
            0x009d | 0x0225 | 0x018d | 0x0180 | 0x009a | 0x0252 | 0x0229 | 0x009b | 0x0084 => {
                saw_post_sheet_terminator = true;
                break;
            }
            _ => {}
        }
    }
    if !saw_post_sheet_terminator {
        return Err(Error::InvalidInput(
            "XLSB workbook is missing its names terminator".into(),
        ));
    }
    Ok((sheet_paths, count))
}

fn validate_defined_name(payload: &[u8]) -> Result<usize> {
    if payload.len() < 13 {
        return Err(Error::InvalidInput(
            "XLSB defined-name record is truncated".into(),
        ));
    }
    let name_chars = u32::from_le_bytes(payload[9..13].try_into().unwrap()) as usize;
    let name_end =
        13usize
            .checked_add(name_chars.checked_mul(2).ok_or_else(|| {
                Error::LimitExceeded("XLSB defined-name length overflowed".into())
            })?)
            .ok_or_else(|| Error::LimitExceeded("XLSB defined-name length overflowed".into()))?;
    if name_chars > 255 || name_end + 4 > payload.len() {
        return Err(Error::InvalidInput(
            "XLSB defined-name string is malformed".into(),
        ));
    }
    let formula_len =
        u32::from_le_bytes(payload[name_end..name_end + 4].try_into().unwrap()) as usize;
    if formula_len > MAX_XLSB_NAME_FORMULA_BYTES || name_end + 4 + formula_len > payload.len() {
        return Err(Error::InvalidInput(
            "XLSB defined-name formula extends beyond its record".into(),
        ));
    }
    Ok(formula_len)
}

fn scan_worksheet(
    bytes: &[u8],
    records: &mut usize,
    shared_string_lengths: &[usize],
) -> Result<(usize, usize, usize)> {
    let mut offset = 0usize;
    let mut row = 0u32;
    let mut dimensions = None;
    let mut count = 0usize;
    let mut text_bytes = 0usize;
    let mut min_row = u32::MAX;
    let mut max_row = 0u32;
    let mut min_column = u32::MAX;
    let mut max_column = 0u32;
    while let Some((kind, payload)) = next_record(bytes, &mut offset, records)? {
        match kind {
            0x0094 => {
                if dimensions.is_some() {
                    return Err(Error::InvalidInput(
                        "XLSB worksheet contains duplicate BrtWsDim records".into(),
                    ));
                }
                if payload.len() != 16 {
                    return Err(Error::InvalidInput(
                        "XLSB BrtWsDim has an invalid size".into(),
                    ));
                }
                let bounds = Dimensions {
                    first_row: u32::from_le_bytes(payload[0..4].try_into().unwrap()),
                    last_row: u32::from_le_bytes(payload[4..8].try_into().unwrap()),
                    first_column: u32::from_le_bytes(payload[8..12].try_into().unwrap()),
                    last_column: u32::from_le_bytes(payload[12..16].try_into().unwrap()),
                };
                validate_dimensions(bounds)?;
                if bounds.area() > MAX_XLSB_CELLS_PER_SHEET {
                    return Err(Error::LimitExceeded(format!(
                        "XLSB worksheet range spans {} cells",
                        bounds.area()
                    )));
                }
                dimensions = Some(bounds);
            }
            0x0000 => {
                if payload.len() < 4 {
                    return Err(Error::InvalidInput("XLSB row header is truncated".into()));
                }
                row = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                if row >= XLSB_MAX_ROWS {
                    return Err(Error::InvalidInput(
                        "XLSB row index exceeds Excel's worksheet limit".into(),
                    ));
                }
            }
            0x0002..=0x000a if kind != 0x0001 => {
                if payload.len() < 9 {
                    return Err(Error::InvalidInput("XLSB cell record is truncated".into()));
                }
                let column = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                if column >= XLSB_MAX_COLUMNS {
                    return Err(Error::InvalidInput(
                        "XLSB column index exceeds Excel's worksheet limit".into(),
                    ));
                }
                let minimum = match kind {
                    2 | 7 => 12,
                    5 | 9 => 16,
                    6 | 8 => 12,
                    _ => 9,
                };
                if payload.len() < minimum {
                    return Err(Error::InvalidInput(
                        "XLSB cell payload is shorter than its value".into(),
                    ));
                }
                if kind == 6 || kind == 8 {
                    let chars = u32::from_le_bytes(payload[8..12].try_into().unwrap()) as usize;
                    let text_len = 12usize
                        .checked_add(chars.checked_mul(2).ok_or_else(|| {
                            Error::LimitExceeded("XLSB inline string size overflowed".into())
                        })?)
                        .ok_or_else(|| {
                            Error::LimitExceeded("XLSB inline string size overflowed".into())
                        })?;
                    if chars > MAX_XLSB_CELL_TEXT_CHARS || payload.len() < text_len {
                        return Err(Error::InvalidInput(
                            "XLSB inline string length is invalid".into(),
                        ));
                    }
                    text_bytes = text_bytes.saturating_add(chars.saturating_mul(3));
                } else if kind == 7 {
                    let string_index =
                        u32::from_le_bytes(payload[8..12].try_into().unwrap()) as usize;
                    let string_bytes =
                        shared_string_lengths.get(string_index).ok_or_else(|| {
                            Error::InvalidInput(
                                "XLSB shared-string cell index is out of range".into(),
                            )
                        })?;
                    text_bytes = text_bytes.saturating_add(*string_bytes);
                }
                if row < min_row {
                    min_row = row;
                }
                if row > max_row {
                    max_row = row;
                }
                if column < min_column {
                    min_column = column;
                }
                if column > max_column {
                    max_column = column;
                }
                count = count.saturating_add(1);
                if count > MAX_XLSB_CELLS_PER_SHEET {
                    return Err(Error::LimitExceeded(format!(
                        "XLSB worksheet contains more than {MAX_XLSB_CELLS_PER_SHEET} cells"
                    )));
                }
            }
            0x0092 => break,
            _ => {}
        }
    }
    let dimensions = dimensions
        .ok_or_else(|| Error::InvalidInput("XLSB worksheet has no BrtWsDim record".into()))?;
    if count > 0
        && (min_row < dimensions.first_row
            || max_row > dimensions.last_row
            || min_column < dimensions.first_column
            || max_column > dimensions.last_column)
    {
        return Err(Error::InvalidInput(
            "XLSB cell record lies outside the declared sheet range".into(),
        ));
    }
    if count > 0 {
        let dense = (max_row - min_row + 1) as usize * (max_column - min_column + 1) as usize;
        if dense > MAX_XLSB_CELLS_PER_SHEET {
            return Err(Error::LimitExceeded(format!(
                "XLSB worksheet cells span {dense} positions"
            )));
        }
    }
    Ok((count, dimensions.area(), text_bytes))
}

fn validate_dimensions(dimensions: Dimensions) -> Result<()> {
    if dimensions.last_row < dimensions.first_row
        || dimensions.last_column < dimensions.first_column
        || dimensions.last_row >= XLSB_MAX_ROWS
        || dimensions.last_column >= XLSB_MAX_COLUMNS
    {
        return Err(Error::InvalidInput(
            "XLSB BrtWsDim bounds are invalid".into(),
        ));
    }
    Ok(())
}

fn scan_shared_strings(bytes: &[u8]) -> Result<(Vec<usize>, usize)> {
    let mut offset = 0usize;
    let mut record_count = 0usize;
    let mut expected_count = None;
    let mut saw_end = false;
    let mut strings = Vec::new();
    let mut text_bytes = 0usize;
    while let Some((kind, payload)) = next_record(bytes, &mut offset, &mut record_count)? {
        if kind == 0x009f {
            if payload.len() < 8 {
                return Err(Error::InvalidInput(
                    "XLSB shared-string header is truncated".into(),
                ));
            }
            if expected_count.is_some() {
                return Err(Error::InvalidInput(
                    "XLSB shared-string part repeats its header".into(),
                ));
            }
            let total = u32::from_le_bytes(payload[0..4].try_into().unwrap());
            let count = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
            if count as u64 > u64::from(total) {
                return Err(Error::InvalidInput(
                    "XLSB unique shared-string count exceeds total references".into(),
                ));
            }
            if count > MAX_XLSB_CELLS_TOTAL {
                return Err(Error::LimitExceeded(
                    "XLSB shared-string count exceeds its limit".into(),
                ));
            }
            if count > MAX_XLSB_CELLS_TOTAL {
                return Err(Error::LimitExceeded(
                    "XLSB shared-string count exceeds its limit".into(),
                ));
            }
            expected_count = Some(count);
        } else if kind == 0x0013 {
            if payload.len() < 5 {
                return Err(Error::InvalidInput(
                    "XLSB shared-string item is truncated".into(),
                ));
            }
            let chars = u32::from_le_bytes(payload[1..5].try_into().unwrap()) as usize;
            let content_len = 5usize
                .checked_add(chars.checked_mul(2).ok_or_else(|| {
                    Error::LimitExceeded("XLSB shared-string size overflowed".into())
                })?)
                .ok_or_else(|| Error::LimitExceeded("XLSB shared-string size overflowed".into()))?;
            if chars > MAX_XLSB_CELL_TEXT_CHARS || payload.len() < content_len {
                return Err(Error::InvalidInput(
                    "XLSB shared-string length is invalid".into(),
                ));
            }
            let size = chars.saturating_mul(3);
            text_bytes = text_bytes.saturating_add(size);
            if text_bytes > MAX_XLSB_EXPANDED_TEXT_BYTES {
                return Err(Error::LimitExceeded(
                    "XLSB shared-string table expands beyond its limit".into(),
                ));
            }
            strings.push(size);
        } else if kind == 0x00a0 {
            saw_end = true;
        }
    }
    if expected_count.is_none_or(|expected| expected != strings.len()) || !saw_end {
        return Err(Error::InvalidInput(
            "XLSB shared-string table is missing its header, end, or declared items".into(),
        ));
    }
    Ok((strings, record_count))
}

fn scan_styles(bytes: &[u8], records: &mut usize) -> Result<usize> {
    let mut offset = 0;
    let mut format_count = 0usize;
    let mut xf_count = 0usize;
    let mut expanded_text = 0usize;
    while let Some((kind, payload)) = next_record(bytes, &mut offset, records)? {
        match kind {
            0x0267 => {
                if payload.len() < 4 {
                    return Err(Error::InvalidInput(
                        "XLSB format table header is truncated".into(),
                    ));
                }
                format_count = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
                if format_count > 100_000 {
                    return Err(Error::LimitExceeded(
                        "XLSB custom format count exceeds 100000".into(),
                    ));
                }
            }
            0x0269 => {
                if payload.len() < 4 {
                    return Err(Error::InvalidInput(
                        "XLSB cell format header is truncated".into(),
                    ));
                }
                xf_count = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
                if xf_count > 500_000 {
                    return Err(Error::LimitExceeded(
                        "XLSB cell format count exceeds 500000".into(),
                    ));
                }
            }
            0x002c => {
                if payload.len() < 6 {
                    return Err(Error::InvalidInput(
                        "XLSB format string is truncated".into(),
                    ));
                }
                let chars = u32::from_le_bytes(payload[2..6].try_into().unwrap()) as usize;
                if chars > MAX_XLSB_CELL_TEXT_CHARS
                    || 6usize.saturating_add(chars.saturating_mul(2)) > payload.len()
                {
                    return Err(Error::InvalidInput(
                        "XLSB format string length is invalid".into(),
                    ));
                }
                expanded_text = expanded_text.saturating_add(chars.saturating_mul(3));
                if expanded_text > MAX_XLSB_EXPANDED_TEXT_BYTES {
                    return Err(Error::LimitExceeded(
                        "XLSB number-format strings exceed the text budget".into(),
                    ));
                }
            }
            0x002f if payload.len() < 4 => {
                return Err(Error::InvalidInput("XLSB XF record is truncated".into()));
            }
            _ => {}
        }
    }
    let _ = (format_count, xf_count);
    Ok(expanded_text)
}

fn next_record<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
    count: &mut usize,
) -> Result<Option<(u16, &'a [u8])>> {
    if *offset == bytes.len() {
        return Ok(None);
    }
    if *offset > bytes.len() {
        return Err(Error::InvalidInput(
            "XLSB record offset is outside the stream".into(),
        ));
    }
    let kind_first = *bytes
        .get(*offset)
        .ok_or_else(|| Error::InvalidInput("XLSB record type is truncated".into()))?;
    *offset += 1;
    let kind = if kind_first & 0x80 != 0 {
        let second = *bytes
            .get(*offset)
            .ok_or_else(|| Error::InvalidInput("XLSB record type is truncated".into()))?;
        *offset += 1;
        if second & 0x80 != 0 {
            return Err(Error::InvalidInput(
                "XLSB record type uses an unsupported varint".into(),
            ));
        }
        (kind_first & 0x7f) as u16 | (((second & 0x7f) as u16) << 7)
    } else {
        kind_first as u16
    };
    let mut length = 0usize;
    let mut shift = 0u32;
    let mut ended = false;
    for _ in 0..5 {
        let byte = *bytes
            .get(*offset)
            .ok_or_else(|| Error::InvalidInput("XLSB record length is truncated".into()))?;
        *offset += 1;
        length = length
            .checked_add(
                ((byte & 0x7f) as usize)
                    .checked_shl(shift)
                    .unwrap_or(usize::MAX),
            )
            .ok_or_else(|| Error::LimitExceeded("XLSB record length overflowed".into()))?;
        if byte & 0x80 == 0 {
            ended = true;
            break;
        }
        shift += 7;
    }
    if !ended {
        return Err(Error::InvalidInput(
            "XLSB record length varint is too long".into(),
        ));
    }
    if length > MAX_XLSB_RECORD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XLSB record is {length} bytes; maximum is {MAX_XLSB_RECORD_BYTES}"
        )));
    }
    let end = offset
        .checked_add(length)
        .ok_or_else(|| Error::LimitExceeded("XLSB record end overflowed".into()))?;
    if end > bytes.len() {
        return Err(Error::InvalidInput("XLSB record exceeds its part".into()));
    }
    let payload = &bytes[*offset..end];
    *offset = end;
    *count = count.saturating_add(1);
    if *count > MAX_XLSB_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "XLSB package exceeds {MAX_XLSB_RECORDS} records"
        )));
    }
    Ok(Some((kind, payload)))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    fn record(kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        if kind < 0x80 {
            bytes.push(kind as u8);
        } else {
            bytes.push(((kind & 0x7f) as u8) | 0x80);
            bytes.push((kind >> 7) as u8);
        }
        let mut length = payload.len();
        loop {
            let mut byte = (length & 0x7f) as u8;
            length >>= 7;
            if length > 0 {
                byte |= 0x80;
            }
            bytes.push(byte);
            if length == 0 {
                break;
            }
        }
        bytes.extend_from_slice(payload);
        bytes
    }

    fn workbook_parts() -> (Vec<u8>, Vec<u8>) {
        let mut bundle = Vec::new();
        bundle.extend_from_slice(&0u32.to_le_bytes());
        bundle.extend_from_slice(&1u32.to_le_bytes());
        let rel_id = "rId1".encode_utf16().collect::<Vec<_>>();
        bundle.extend_from_slice(&(rel_id.len() as u32).to_le_bytes());
        for unit in rel_id {
            bundle.extend_from_slice(&unit.to_le_bytes());
        }
        let name = "Sheet1".encode_utf16().collect::<Vec<_>>();
        bundle.extend_from_slice(&(name.len() as u32).to_le_bytes());
        for unit in name {
            bundle.extend_from_slice(&unit.to_le_bytes());
        }
        let mut workbook = record(0x009c, &bundle);
        workbook.extend(record(0x0090, &[]));
        workbook.extend(record(0x009d, &[]));
        let relationships = br#"<Relationships><Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2006/relationships/worksheet" Target="worksheets/sheet1.bin"/></Relationships>"#.to_vec();
        (workbook, relationships)
    }

    fn package(sheet: &[u8], shared_strings: Option<&[u8]>) -> Vec<u8> {
        let (workbook, relationships) = workbook_parts();
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("xl/workbook.bin", options).unwrap();
        zip.write_all(&workbook).unwrap();
        zip.start_file("xl/_rels/workbook.bin.rels", options)
            .unwrap();
        zip.write_all(&relationships).unwrap();
        zip.start_file("xl/worksheets/sheet1.bin", options).unwrap();
        zip.write_all(sheet).unwrap();
        if let Some(strings) = shared_strings {
            zip.start_file("xl/sharedStrings.bin", options).unwrap();
            zip.write_all(strings).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn worksheet_dimensions(rows: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(rows - 1).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes
    }

    #[test]
    fn rejects_xlsb_used_ranges_before_allocating_a_sheet_matrix() {
        let mut sheet = record(0x0094, &worksheet_dimensions(1_048_576));
        sheet.extend(record(0x0091, &[]));
        sheet.extend(record(0x0092, &[]));
        let error = preflight(&package(&sheet, None)).unwrap_err();
        assert!(format!("{error}").contains("range spans"));
    }

    #[test]
    fn caps_repeated_xlsb_shared_string_expansion_before_rendering() {
        let cell_count = 50_000u32;
        let mut sheet = record(0x0094, &worksheet_dimensions(cell_count));
        sheet.extend(record(0x0091, &[]));
        for row in 0..cell_count {
            sheet.extend(record(0x0000, &row.to_le_bytes()));
            let mut cell = Vec::new();
            cell.extend_from_slice(&0u32.to_le_bytes());
            cell.extend_from_slice(&0u32.to_le_bytes());
            cell.extend_from_slice(&0u32.to_le_bytes());
            sheet.extend(record(0x0007, &cell));
        }
        sheet.extend(record(0x0092, &[]));

        let mut header = Vec::new();
        header.extend_from_slice(&cell_count.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        let mut shared_strings = record(0x009f, &header);
        let mut item = vec![0u8];
        item.extend_from_slice(&1_000u32.to_le_bytes());
        item.extend(std::iter::repeat_n(0x41, 2_000));
        shared_strings.extend(record(0x0013, &item));
        shared_strings.extend(record(0x00a0, &[]));

        let error = preflight(&package(&sheet, Some(&shared_strings))).unwrap_err();
        assert!(format!("{error}").contains("aggregate worksheet budgets"));
    }
}
