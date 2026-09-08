use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};
use crate::ooxml::chart::{ChartData, parse_chart, render_chart};
use crate::ooxml::{
    Relationships, ZipPackage, attribute, color_from_hex, local_name, parse_f64, parse_i64,
    text_advance_factor,
};

const DEFAULT_COLUMN_POINTS: f64 = 48.0;
const DEFAULT_ROW_POINTS: f64 = 15.0;
const MAX_RENDERED_CELLS: usize = 200_000;
const MAX_CANVAS_POINTS: f64 = 200_000.0;
const MAX_POPULATED_CELLS: usize = 1_000_000;
const AUTO_TILE_POINTS: f64 = 16_384.0;
const AUTO_TILE_CELLS: usize = 2_000;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut package = ZipPackage::open(path, options.max_zip_entry_bytes)?;
    let workbook_part = "xl/workbook.xml";
    if !package.contains(workbook_part) {
        return Err(Error::InvalidInput(
            "XLSX is missing xl/workbook.xml".into(),
        ));
    }
    let workbook_xml = package.read(workbook_part)?;
    let workbook_relationships = package.relationships(workbook_part, options.max_xml_events)?;
    let workbook = parse_workbook(&workbook_xml, options.max_xml_events)?;
    if workbook.sheets.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "XLSX contains {} worksheets; maximum is {}",
            workbook.sheets.len(),
            options.max_pages
        )));
    }
    let shared_strings = package
        .read_optional("xl/sharedStrings.xml")?
        .map(|xml| parse_shared_strings(&xml, options.max_xml_events))
        .transpose()?
        .unwrap_or_default();
    let mut styles = package
        .read_optional("xl/styles.xml")?
        .map(|xml| Styles::parse(&xml, options.max_xml_events))
        .transpose()?
        .unwrap_or_default();
    styles.date_1904 = workbook.date_1904;
    let mut warnings = Vec::new();
    let mut output_page_number = 0usize;
    for (name, relationship_id) in &workbook.sheets {
        let Some(sheet_part) = workbook_relationships.target(relationship_id, workbook_part) else {
            return Err(Error::InvalidInput(format!(
                "worksheet relationship {relationship_id} for {name} is missing"
            )));
        };
        let xml = package.read(&sheet_part)?;
        let relationships = package.relationships(&sheet_part, options.max_xml_events)?;
        let mut sheet = parse_sheet(
            &xml,
            name,
            &shared_strings,
            &styles,
            &relationships,
            &sheet_part,
            workbook.print_areas.get(name).cloned().unwrap_or_default(),
            workbook.print_titles.get(name).copied().unwrap_or_default(),
            options.max_xml_events,
        )?;
        let drawing_parts = relationships
            .ids_of_type("/drawing")
            .filter_map(|(id, _)| relationships.target(id, &sheet_part))
            .collect::<Vec<_>>();
        let mut drawing_objects = Vec::new();
        for drawing_part in drawing_parts {
            let drawing_xml = package.read(&drawing_part)?;
            let drawing_relationships =
                package.relationships(&drawing_part, options.max_xml_events)?;
            let (mut objects, drawing_warnings) = parse_drawing_objects(
                &drawing_xml,
                &drawing_relationships,
                &drawing_part,
                &mut package,
                options.max_xml_events,
            )?;
            drawing_objects.append(&mut objects);
            sheet.warnings.extend(drawing_warnings);
        }
        let formula_context = if sheet_needs_formula_resolver(&sheet, &workbook.defined_names) {
            build_formula_context(
                name,
                &sheet,
                &workbook,
                &workbook_relationships,
                workbook_part,
                &mut package,
                &shared_strings,
                &styles,
                options.max_xml_events,
            )?
        } else {
            FormulaContext {
                sheet_name: name.clone(),
                ..FormulaContext::default()
            }
        };
        evaluate_missing_formulas(&mut sheet, &styles, &formula_context);
        let conditional_expression_results = sheet
            .conditional_rules
            .iter()
            .map(|rule| {
                if let ConditionalRuleKind::Expression { formula, .. } = &rule.kind {
                    evaluate_formula_expression(formula, &sheet.cells, &formula_context)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let grid = sheet.build_grid(&drawing_objects);
        let regions = sheet.render_regions(&drawing_objects);
        for (region_index, region) in regions.iter().enumerate() {
            output_page_number += 1;
            if output_page_number > options.max_pages {
                return Err(Error::LimitExceeded(format!(
                    "XLSX pagination exceeds {} pages",
                    options.max_pages
                )));
            }
            let page = sheet.render_region(
                output_page_number,
                &styles,
                &drawing_objects,
                &grid,
                &conditional_expression_results,
                *region,
                region_index + 1,
                regions.len(),
            )?;
            warnings.extend(page.warnings.iter().cloned());
            sink.consume(page)?;
        }
    }
    Ok(deduplicate(warnings))
}

fn sheet_needs_formula_resolver(
    sheet: &Sheet,
    defined_names: &HashMap<String, CellTarget>,
) -> bool {
    let formulas = sheet
        .cells
        .values()
        .filter(|cell| !cell.formula.is_empty())
        .map(|cell| cell.formula.as_str())
        .chain(sheet.conditional_rules.iter().filter_map(|rule| {
            if let ConditionalRuleKind::Expression { formula, .. } = &rule.kind {
                Some(formula.as_str())
            } else {
                None
            }
        }));
    formulas.into_iter().any(|formula| {
        formula.contains('!')
            || defined_names
                .keys()
                .any(|name| formula.contains(name.as_str()))
    })
}

struct WorkbookInfo {
    sheets: Vec<(String, String)>,
    date_1904: bool,
    print_areas: HashMap<String, Vec<MergeRange>>,
    print_titles: HashMap<String, PrintTitles>,
    defined_names: HashMap<String, CellTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CellTarget {
    sheet: String,
    row: usize,
    column: usize,
}

#[derive(Clone, Copy, Debug, Default)]
struct PrintTitles {
    rows: Option<(usize, usize)>,
    columns: Option<(usize, usize)>,
}

fn parse_workbook(xml: &[u8], max_events: usize) -> Result<WorkbookInfo> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut result = Vec::new();
    let mut date_1904 = false;
    let mut print_area_capture = None::<usize>;
    let mut print_area_text = String::new();
    let mut raw_print_areas = Vec::<(usize, String)>::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "workbook.xml event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) | Event::Empty(start) => match local_name(start.name().as_ref()) {
                b"sheet" => {
                    let name = attribute(&start, b"name")
                        .unwrap_or_else(|| format!("Sheet{}", result.len() + 1));
                    let relationship_id = attribute(&start, b"id").unwrap_or_default();
                    if !relationship_id.is_empty() {
                        result.push((name, relationship_id));
                    }
                }
                b"workbookPr" => {
                    date_1904 = attribute(&start, b"date1904")
                        .is_some_and(|value| value == "1" || value == "true");
                }
                b"definedName"
                    if attribute(&start, b"name").as_deref() == Some("_xlnm.Print_Area") =>
                {
                    print_area_capture =
                        attribute(&start, b"localSheetId").and_then(|value| value.parse().ok());
                    print_area_text.clear();
                }
                _ => {}
            },
            Event::Text(text) if print_area_capture.is_some() => {
                print_area_text.push_str(&decode_xlsx_text(&text, "print area")?);
            }
            Event::GeneralRef(reference) if print_area_capture.is_some() => {
                print_area_text.push_str(&decode_xlsx_reference(&reference, "print area")?);
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"definedName" => {
                if let Some(sheet_index) = print_area_capture.take() {
                    raw_print_areas.push((sheet_index, std::mem::take(&mut print_area_text)));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    let mut print_areas = HashMap::new();
    for (sheet_index, value) in raw_print_areas {
        if let Some((sheet_name, _)) = result.get(sheet_index) {
            let ranges = parse_print_area_ranges(&value);
            if !ranges.is_empty() {
                print_areas.insert(sheet_name.clone(), ranges);
            }
        }
    }
    let print_titles = parse_workbook_print_titles(xml, &result, max_events)?;
    let defined_names = parse_workbook_defined_names(xml, max_events)?;
    Ok(WorkbookInfo {
        sheets: result,
        date_1904,
        print_areas,
        print_titles,
        defined_names,
    })
}

fn parse_workbook_defined_names(
    xml: &[u8],
    max_events: usize,
) -> Result<HashMap<String, CellTarget>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut name = None::<String>;
    let mut value = String::new();
    let mut output = HashMap::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "XLSX defined-name event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) if local_name(start.name().as_ref()) == b"definedName" => {
                let candidate = attribute(&start, b"name").unwrap_or_default();
                name = (!candidate.starts_with("_xlnm.")).then_some(candidate);
                value.clear();
            }
            Event::Text(text) if name.is_some() => {
                value.push_str(&decode_xlsx_text(&text, "defined name")?);
            }
            Event::GeneralRef(reference) if name.is_some() => {
                value.push_str(&decode_xlsx_reference(&reference, "defined name")?);
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"definedName" => {
                if let Some(name) = name.take()
                    && let Some(target) = parse_named_cell_target(&value)
                {
                    output.insert(name, target);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(output)
}

fn parse_named_cell_target(value: &str) -> Option<CellTarget> {
    let (sheet, reference) = value.trim().rsplit_once('!')?;
    if reference.contains(':') || reference.contains("#REF!") {
        return None;
    }
    let (row, column) = parse_cell_reference(reference)?;
    let sheet = sheet.trim().trim_matches('\'').replace("''", "'");
    (!sheet.is_empty()).then_some(CellTarget { sheet, row, column })
}

fn parse_print_area_ranges(value: &str) -> Vec<MergeRange> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    for (index, character) in value.char_indices() {
        if character == '\'' {
            quoted = !quoted;
        } else if character == ',' && !quoted {
            parts.push(&value[start..index]);
            start = index + 1;
        }
    }
    parts.push(&value[start..]);
    parts
        .into_iter()
        .filter_map(|part| {
            let range = part
                .rsplit_once('!')
                .map_or(part, |(_, range)| range)
                .trim();
            parse_merge_range(range)
        })
        .collect()
}

fn parse_workbook_print_titles(
    xml: &[u8],
    sheets: &[(String, String)],
    max_events: usize,
) -> Result<HashMap<String, PrintTitles>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut capture = None::<usize>;
    let mut text = String::new();
    let mut raw = Vec::<(usize, String)>::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "XLSX print-title event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start)
                if local_name(start.name().as_ref()) == b"definedName"
                    && attribute(&start, b"name").as_deref() == Some("_xlnm.Print_Titles") =>
            {
                capture = attribute(&start, b"localSheetId").and_then(|value| value.parse().ok());
                text.clear();
            }
            Event::Text(value) if capture.is_some() => {
                text.push_str(&decode_xlsx_text(&value, "print titles")?);
            }
            Event::GeneralRef(reference) if capture.is_some() => {
                text.push_str(&decode_xlsx_reference(&reference, "print titles")?);
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"definedName" => {
                if let Some(sheet_index) = capture.take() {
                    raw.push((sheet_index, std::mem::take(&mut text)));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    let mut output = HashMap::new();
    for (sheet_index, value) in raw {
        if let Some((sheet_name, _)) = sheets.get(sheet_index) {
            let titles = parse_print_titles(&value);
            if titles.rows.is_some() || titles.columns.is_some() {
                output.insert(sheet_name.clone(), titles);
            }
        }
    }
    Ok(output)
}

fn parse_print_titles(value: &str) -> PrintTitles {
    let mut titles = PrintTitles::default();
    for part in split_defined_name_parts(value) {
        let range = part
            .rsplit_once('!')
            .map_or(part, |(_, range)| range)
            .trim();
        let range = range.replace('$', "");
        let Some((first, second)) = range.split_once(':') else {
            continue;
        };
        if let (Ok(first), Ok(second)) = (first.parse::<usize>(), second.parse::<usize>()) {
            titles.rows = Some((first.min(second).max(1), first.max(second).max(1)));
        } else if let (Some(first), Some(second)) =
            (parse_column_letters(first), parse_column_letters(second))
        {
            titles.columns = Some((first.min(second), first.max(second)));
        }
    }
    titles
}

fn split_defined_name_parts(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quoted = false;
    for (index, character) in value.char_indices() {
        if character == '\'' {
            quoted = !quoted;
        } else if character == ',' && !quoted {
            parts.push(&value[start..index]);
            start = index + 1;
        }
    }
    parts.push(&value[start..]);
    parts
}

fn parse_column_letters(value: &str) -> Option<usize> {
    let mut column = 0usize;
    for byte in value.bytes() {
        let byte = byte.to_ascii_uppercase();
        if !byte.is_ascii_uppercase() {
            return None;
        }
        column = column
            .checked_mul(26)?
            .checked_add(usize::from(byte - b'A' + 1))?;
    }
    (column > 0).then_some(column)
}

fn parse_shared_strings(xml: &[u8], max_events: usize) -> Result<Vec<String>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_item = false;
    let mut in_text = false;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "sharedStrings.xml event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => match local_name(start.name().as_ref()) {
                b"si" => {
                    current.clear();
                    in_item = true;
                }
                b"t" if in_item => in_text = true,
                _ => {}
            },
            Event::Text(text) if in_text => {
                current.push_str(&decode_xlsx_text(&text, "shared string")?)
            }
            Event::GeneralRef(reference) if in_text => {
                current.push_str(&decode_xlsx_reference(&reference, "shared string")?)
            }
            Event::End(end) => match local_name(end.name().as_ref()) {
                b"t" => in_text = false,
                b"si" => {
                    result.push(std::mem::take(&mut current));
                    in_item = false;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(result)
}

#[derive(Clone, Debug)]
struct FontStyle {
    family: String,
    size: f64,
    color: String,
    bold: bool,
    italic: bool,
}

impl Default for FontStyle {
    fn default() -> Self {
        Self {
            family: "Calibri, Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into(),
            size: 11.0,
            color: "#000000".into(),
            bold: false,
            italic: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct BorderStyle {
    left: Option<(String, f64)>,
    right: Option<(String, f64)>,
    top: Option<(String, f64)>,
    bottom: Option<(String, f64)>,
}

#[derive(Clone, Debug, Default)]
struct DifferentialStyle {
    font_color: Option<String>,
    fill: Option<String>,
}

#[derive(Clone, Debug)]
struct CellStyle {
    font_id: usize,
    fill_id: usize,
    border_id: usize,
    number_format_id: u32,
    horizontal: TextAnchor,
    wrap_text: bool,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            font_id: 0,
            fill_id: 0,
            border_id: 0,
            number_format_id: 0,
            horizontal: TextAnchor::Start,
            wrap_text: false,
        }
    }
}

#[derive(Clone, Debug)]
struct Styles {
    fonts: Vec<FontStyle>,
    fills: Vec<Option<String>>,
    borders: Vec<BorderStyle>,
    cell_styles: Vec<CellStyle>,
    custom_number_formats: HashMap<u32, String>,
    date_1904: bool,
    differential_styles: Vec<DifferentialStyle>,
}

impl Default for Styles {
    fn default() -> Self {
        Self {
            fonts: vec![FontStyle::default()],
            fills: vec![None],
            borders: vec![BorderStyle::default()],
            cell_styles: vec![CellStyle::default()],
            custom_number_formats: HashMap::new(),
            date_1904: false,
            differential_styles: Vec::new(),
        }
    }
}

impl Styles {
    fn parse(xml: &[u8], max_events: usize) -> Result<Self> {
        let mut styles = Self::default();
        styles.fonts.clear();
        styles.fills.clear();
        styles.borders.clear();
        styles.cell_styles.clear();
        let mut reader = Reader::from_reader(xml);
        reader.config_mut().trim_text(true);
        let mut buffer = Vec::new();
        let mut stack = Vec::<String>::new();
        let mut font = None::<FontStyle>;
        let mut fill = None::<Option<String>>;
        let mut border = None::<BorderStyle>;
        let mut border_side = None::<String>;
        let mut cell_style = None::<CellStyle>;
        let mut events = 0usize;
        loop {
            events += 1;
            if events > max_events {
                return Err(Error::LimitExceeded(
                    "styles.xml event limit exceeded".into(),
                ));
            }
            match reader.read_event_into(&mut buffer)? {
                Event::Start(start) => {
                    let name =
                        String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                    style_event(
                        &start,
                        &name,
                        &stack,
                        &mut font,
                        &mut fill,
                        &mut border,
                        &mut border_side,
                        &mut cell_style,
                        &mut styles,
                    );
                    stack.push(name);
                }
                Event::Empty(start) => {
                    let name =
                        String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                    style_event(
                        &start,
                        &name,
                        &stack,
                        &mut font,
                        &mut fill,
                        &mut border,
                        &mut border_side,
                        &mut cell_style,
                        &mut styles,
                    );
                    match name.as_str() {
                        "font" if stack.iter().any(|item| item == "fonts") => {
                            styles.fonts.push(font.take().unwrap_or_default());
                        }
                        "fill" if stack.iter().any(|item| item == "fills") => {
                            styles.fills.push(fill.take().unwrap_or(None));
                        }
                        "border" if stack.iter().any(|item| item == "borders") => {
                            styles.borders.push(border.take().unwrap_or_default());
                        }
                        "xf" if stack.iter().any(|item| item == "cellXfs") => {
                            styles
                                .cell_styles
                                .push(cell_style.take().unwrap_or_default());
                        }
                        _ => {}
                    }
                }
                Event::End(end) => {
                    match local_name(end.name().as_ref()) {
                        b"font" => styles.fonts.push(font.take().unwrap_or_default()),
                        b"fill" => styles.fills.push(fill.take().unwrap_or(None)),
                        b"border" => styles.borders.push(border.take().unwrap_or_default()),
                        b"left" | b"right" | b"top" | b"bottom" => border_side = None,
                        b"xf" if stack.iter().any(|item| item == "cellXfs") => {
                            styles
                                .cell_styles
                                .push(cell_style.take().unwrap_or_default());
                        }
                        _ => {}
                    }
                    stack.pop();
                }
                Event::Eof => break,
                _ => {}
            }
            buffer.clear();
        }
        if styles.fonts.is_empty() {
            styles.fonts.push(FontStyle::default());
        }
        if styles.fills.is_empty() {
            styles.fills.push(None);
        }
        if styles.borders.is_empty() {
            styles.borders.push(BorderStyle::default());
        }
        if styles.cell_styles.is_empty() {
            styles.cell_styles.push(CellStyle::default());
        }
        styles.differential_styles = parse_differential_styles(xml, max_events)?;
        Ok(styles)
    }

    fn cell_style(&self, id: usize) -> &CellStyle {
        self.cell_styles
            .get(id)
            .unwrap_or_else(|| &self.cell_styles[0])
    }

    fn font(&self, id: usize) -> &FontStyle {
        self.fonts.get(id).unwrap_or_else(|| &self.fonts[0])
    }

    fn fill(&self, id: usize) -> Option<&str> {
        self.fills.get(id).and_then(Option::as_deref)
    }

    fn border(&self, id: usize) -> &BorderStyle {
        self.borders.get(id).unwrap_or_else(|| &self.borders[0])
    }

    fn format_value(&self, raw: &str, style: &CellStyle) -> String {
        let Ok(value) = raw.parse::<f64>() else {
            return raw.to_owned();
        };
        let format_code = self
            .custom_number_formats
            .get(&style.number_format_id)
            .map(String::as_str)
            .or_else(|| builtin_number_format(style.number_format_id));
        let Some(format_code) = format_code else {
            return raw.to_owned();
        };
        if is_excel_datetime_format(format_code) {
            return format_excel_datetime(value, format_code, self.date_1904);
        }
        let section = format_code.split(';').next().unwrap_or(format_code);
        let decimals = number_format_decimals(section);
        if section.contains('%') {
            return format!("{:.*}%", decimals, value * 100.0);
        }
        if section.to_ascii_uppercase().contains("E+")
            || section.to_ascii_uppercase().contains("E-")
        {
            return format!("{value:.*E}", decimals.max(1));
        }
        let grouping = section.contains(',');
        let mut formatted = format_decimal_number(value, decimals, grouping);
        if let Some(currency) = section
            .chars()
            .find(|character| "$¥€£￥".contains(*character))
        {
            formatted.insert(0, currency);
        }
        let negative_section = format_code.split(';').nth(1).unwrap_or_default();
        if value < 0.0 && negative_section.contains('(') {
            formatted = format!("({})", formatted.trim_start_matches('-'));
        }
        formatted
    }
}

fn builtin_number_format(id: u32) -> Option<&'static str> {
    match id {
        1 => Some("0"),
        2 => Some("0.00"),
        3 => Some("#,##0"),
        4 => Some("#,##0.00"),
        5 | 6 => Some("$#,##0"),
        7 | 8 => Some("$#,##0.00"),
        9 => Some("0%"),
        10 => Some("0.00%"),
        11 => Some("0.00E+00"),
        12 | 13 => None,
        14 => Some("m/d/yy"),
        15 => Some("d-mmm-yy"),
        16 => Some("d-mmm"),
        17 => Some("mmm-yy"),
        18 => Some("h:mm AM/PM"),
        19 => Some("h:mm:ss AM/PM"),
        20 => Some("h:mm"),
        21 => Some("h:mm:ss"),
        22 => Some("m/d/yy h:mm"),
        37 | 38 => Some("#,##0;(#,##0)"),
        39 | 40 => Some("#,##0.00;(#,##0.00)"),
        45 => Some("mm:ss"),
        46 => Some("[h]:mm:ss"),
        47 => Some("mm:ss.0"),
        _ => None,
    }
}

fn parse_differential_styles(xml: &[u8], max_events: usize) -> Result<Vec<DifferentialStyle>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut current = None::<DifferentialStyle>;
    let mut output = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "XLSX differential-style event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_differential_style_event(&start, &name, &stack, &mut current);
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_differential_style_event(&start, &name, &stack, &mut current);
                if name == "dxf" && stack.iter().any(|item| item == "dxfs") {
                    output.push(current.take().unwrap_or_default());
                }
            }
            Event::End(end) => {
                if local_name(end.name().as_ref()) == b"dxf" {
                    output.push(current.take().unwrap_or_default());
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(output)
}

fn apply_differential_style_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    current: &mut Option<DifferentialStyle>,
) {
    if name == "dxf" && stack.iter().any(|item| item == "dxfs") {
        *current = Some(DifferentialStyle::default());
        return;
    }
    let Some(style) = current.as_mut() else {
        return;
    };
    if name == "color" && stack.iter().any(|item| item == "font") {
        if let Some(rgb) = attribute(start, b"rgb") {
            style.font_color = Some(argb_to_rgb(&rgb));
        }
    } else if name == "fgColor"
        && stack.iter().any(|item| item == "fill")
        && let Some(rgb) = attribute(start, b"rgb")
    {
        style.fill = Some(argb_to_rgb(&rgb));
    }
}

fn number_format_decimals(format_code: &str) -> usize {
    format_code
        .split('.')
        .nth(1)
        .map_or(0, |fraction| {
            fraction
                .chars()
                .take_while(|character| matches!(character, '0' | '#'))
                .count()
        })
        .min(12)
}

fn format_decimal_number(value: f64, decimals: usize, grouping: bool) -> String {
    let mut formatted = format!("{:.*}", decimals, value.abs());
    if grouping {
        let split = formatted.find('.').unwrap_or(formatted.len());
        let mut grouped = String::with_capacity(formatted.len() + split / 3);
        for (index, character) in formatted[..split].chars().enumerate() {
            if index > 0 && (split - index).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(character);
        }
        grouped.push_str(&formatted[split..]);
        formatted = grouped;
    }
    if value.is_sign_negative() {
        formatted.insert(0, '-');
    }
    formatted
}

fn is_excel_datetime_format(format_code: &str) -> bool {
    let mut probe = String::new();
    let mut quoted = false;
    let mut bracketed = false;
    let mut escaped = false;
    for character in format_code.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => quoted = !quoted,
            '[' if !quoted => bracketed = true,
            ']' if !quoted => bracketed = false,
            _ if !quoted && !bracketed => probe.push(character.to_ascii_lowercase()),
            _ => {}
        }
    }
    probe
        .chars()
        .any(|character| matches!(character, 'y' | 'd' | 'h' | 's'))
        || format_code.to_ascii_lowercase().contains("[h]")
}

fn format_excel_datetime(value: f64, format_code: &str, date_1904: bool) -> String {
    let serial_day = value.floor() as i64;
    let fraction = value - value.floor();
    let seconds_value = fraction * 86_400.0;
    let total_seconds = seconds_value.floor() as i64;
    let fractional_second_digit = (seconds_value.fract() * 10.0).round() as i64 % 10;
    let hour = total_seconds.div_euclid(3_600).rem_euclid(24) as u32;
    let minute = total_seconds.div_euclid(60).rem_euclid(60) as u32;
    let second = total_seconds.rem_euclid(60) as u32;
    let (year, month, day, unix_day) = if !date_1904 && serial_day == 60 {
        (1900, 2, 29, -25_508)
    } else {
        let unix_day = if date_1904 {
            serial_day - 24_107
        } else {
            serial_day - 25_569
        };
        let (year, month, day) = civil_from_days(unix_day);
        (year, month, day, unix_day)
    };
    let weekdays = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let has_ampm = format_code.to_ascii_lowercase().contains("am/pm");
    let mut output = String::new();
    let characters = format_code.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut previous_token = '\0';
    while index < characters.len() {
        let character = characters[index];
        if character == ';' {
            break;
        }
        if character == '"' {
            index += 1;
            while index < characters.len() && characters[index] != '"' {
                output.push(characters[index]);
                index += 1;
            }
            index += usize::from(index < characters.len());
            continue;
        }
        if character == '\\' {
            if let Some(next) = characters.get(index + 1) {
                output.push(*next);
            }
            index += 2;
            continue;
        }
        if matches!(character, '_' | '*') {
            index += 2.min(characters.len() - index);
            continue;
        }
        if character == '[' {
            let end = characters[index..]
                .iter()
                .position(|character| *character == ']')
                .map(|offset| index + offset);
            if let Some(end) = end {
                let content = characters[index + 1..end].iter().collect::<String>();
                if content.eq_ignore_ascii_case("h") {
                    output.push_str(&format!("{}", (value * 24.0).floor() as i64));
                    previous_token = 'h';
                }
                index = end + 1;
                continue;
            }
        }
        let remaining = characters[index..].iter().collect::<String>();
        if remaining.to_ascii_lowercase().starts_with("am/pm") {
            output.push_str(if hour < 12 { "AM" } else { "PM" });
            index += 5;
            continue;
        }
        let lower = character.to_ascii_lowercase();
        if matches!(lower, 'y' | 'm' | 'd' | 'h' | 's') {
            let mut end = index + 1;
            while end < characters.len() && characters[end].to_ascii_lowercase() == lower {
                end += 1;
            }
            let count = end - index;
            let previous_character = index.checked_sub(1).and_then(|value| characters.get(value));
            let next_character = characters.get(end);
            let minute_token = lower == 'm'
                && (previous_token == 'h'
                    || previous_character == Some(&':')
                    || next_character == Some(&':'));
            match lower {
                'y' if count <= 2 => output.push_str(&format!("{:02}", year.rem_euclid(100))),
                'y' => output.push_str(&format!("{year:04}")),
                'm' if minute_token && count == 1 => output.push_str(&minute.to_string()),
                'm' if minute_token => output.push_str(&format!("{minute:02}")),
                'm' if count == 1 => output.push_str(&month.to_string()),
                'm' if count == 2 => output.push_str(&format!("{month:02}")),
                'm' if count == 3 => output.push_str(months[(month - 1) as usize]),
                'm' => {
                    const FULL: [&str; 12] = [
                        "January",
                        "February",
                        "March",
                        "April",
                        "May",
                        "June",
                        "July",
                        "August",
                        "September",
                        "October",
                        "November",
                        "December",
                    ];
                    output.push_str(FULL[(month - 1) as usize]);
                }
                'd' if count == 1 => output.push_str(&day.to_string()),
                'd' if count == 2 => output.push_str(&format!("{day:02}")),
                'd' if count == 3 => {
                    output.push_str(weekdays[(unix_day + 4).rem_euclid(7) as usize]);
                }
                'd' => {
                    const FULL: [&str; 7] = [
                        "Sunday",
                        "Monday",
                        "Tuesday",
                        "Wednesday",
                        "Thursday",
                        "Friday",
                        "Saturday",
                    ];
                    output.push_str(FULL[(unix_day + 4).rem_euclid(7) as usize]);
                }
                'h' => {
                    let display_hour = if has_ampm { (hour + 11) % 12 + 1 } else { hour };
                    if count == 1 {
                        output.push_str(&display_hour.to_string());
                    } else {
                        output.push_str(&format!("{display_hour:02}"));
                    }
                }
                's' if count == 1 => output.push_str(&second.to_string()),
                's' => output.push_str(&format!("{second:02}")),
                _ => {}
            }
            previous_token = if minute_token { 'm' } else { lower };
            index = end;
            continue;
        }
        if character == '.'
            && characters
                .get(index + 1)
                .is_some_and(|character| *character == '0')
        {
            output.push_str(&format!(".{fractional_second_digit}"));
            index += 2;
            continue;
        }
        if character != '@' {
            output.push(character);
        }
        index += 1;
    }
    output
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i32, u32, u32) {
    let days = days_since_unix_epoch + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}

#[allow(clippy::too_many_arguments)]
fn style_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    font: &mut Option<FontStyle>,
    fill: &mut Option<Option<String>>,
    border: &mut Option<BorderStyle>,
    border_side: &mut Option<String>,
    cell_style: &mut Option<CellStyle>,
    styles: &mut Styles,
) {
    let in_fonts = stack.iter().any(|item| item == "fonts");
    let in_fills = stack.iter().any(|item| item == "fills");
    let in_borders = stack.iter().any(|item| item == "borders");
    let in_cell_xfs = stack.iter().any(|item| item == "cellXfs");
    match name {
        "font" if in_fonts => *font = Some(FontStyle::default()),
        "name" if in_fonts => {
            if let (Some(font), Some(value)) = (font.as_mut(), attribute(start, b"val")) {
                font.family = value;
            }
        }
        "sz" if in_fonts => {
            if let Some(font) = font.as_mut() {
                font.size = parse_f64(attribute(start, b"val"), 11.0);
            }
        }
        "b" if in_fonts => {
            if let Some(font) = font.as_mut() {
                font.bold = true;
            }
        }
        "i" if in_fonts => {
            if let Some(font) = font.as_mut() {
                font.italic = true;
            }
        }
        "color" if in_fonts => {
            if let Some(font) = font.as_mut()
                && let Some(rgb) = attribute(start, b"rgb")
            {
                font.color = argb_to_rgb(&rgb);
            }
        }
        "fill" if in_fills => *fill = Some(None),
        "fgColor" if in_fills => {
            if let Some(fill) = fill.as_mut()
                && let Some(rgb) = attribute(start, b"rgb")
            {
                *fill = Some(argb_to_rgb(&rgb));
            }
        }
        "border" if in_borders => *border = Some(BorderStyle::default()),
        "left" | "right" | "top" | "bottom" if in_borders => {
            *border_side = Some(name.to_owned());
            let width = border_width(attribute(start, b"style").as_deref());
            if let Some(width) = width {
                set_border_side(border, name, "#000000".into(), width);
            }
        }
        "color" if in_borders => {
            if let (Some(side), Some(rgb)) = (border_side.as_deref(), attribute(start, b"rgb")) {
                let width = border
                    .as_ref()
                    .and_then(|border| get_border_side(border, side))
                    .map_or(0.75, |(_, width)| *width);
                set_border_side(border, side, argb_to_rgb(&rgb), width);
            }
        }
        "numFmt" => {
            let id = parse_i64(attribute(start, b"numFmtId"), -1);
            if id >= 0
                && let Some(code) = attribute(start, b"formatCode")
            {
                styles.custom_number_formats.insert(id as u32, code);
            }
        }
        "xf" if in_cell_xfs => {
            *cell_style = Some(CellStyle {
                font_id: parse_i64(attribute(start, b"fontId"), 0).max(0) as usize,
                fill_id: parse_i64(attribute(start, b"fillId"), 0).max(0) as usize,
                border_id: parse_i64(attribute(start, b"borderId"), 0).max(0) as usize,
                number_format_id: parse_i64(attribute(start, b"numFmtId"), 0).max(0) as u32,
                ..CellStyle::default()
            });
        }
        "alignment" if in_cell_xfs => {
            if let Some(style) = cell_style.as_mut() {
                style.horizontal = match attribute(start, b"horizontal").as_deref() {
                    Some("center" | "centerContinuous") => TextAnchor::Middle,
                    Some("right") => TextAnchor::End,
                    _ => TextAnchor::Start,
                };
                style.wrap_text = attribute(start, b"wrapText")
                    .is_some_and(|value| value == "1" || value == "true");
            }
        }
        _ => {}
    }
}

fn set_border_side(border: &mut Option<BorderStyle>, side: &str, color: String, width: f64) {
    let border = border.get_or_insert_with(BorderStyle::default);
    let value = Some((color, width));
    match side {
        "left" => border.left = value,
        "right" => border.right = value,
        "top" => border.top = value,
        "bottom" => border.bottom = value,
        _ => {}
    }
}

fn get_border_side<'a>(border: &'a BorderStyle, side: &str) -> Option<&'a (String, f64)> {
    match side {
        "left" => border.left.as_ref(),
        "right" => border.right.as_ref(),
        "top" => border.top.as_ref(),
        "bottom" => border.bottom.as_ref(),
        _ => None,
    }
}

fn border_width(style: Option<&str>) -> Option<f64> {
    match style {
        None | Some("none") => None,
        Some("thin" | "hair") => Some(0.5),
        Some("medium" | "mediumDashed" | "mediumDashDot" | "mediumDashDotDot") => Some(1.5),
        Some("thick" | "double") => Some(2.25),
        Some(_) => Some(0.75),
    }
}

#[derive(Clone, Debug, Default)]
struct Cell {
    row: usize,
    column: usize,
    style_id: usize,
    value: String,
    raw_value: String,
    data_type: String,
    formula: String,
}

#[derive(Clone, Debug)]
struct ConditionalRule {
    ranges: Vec<MergeRange>,
    priority: usize,
    stop_if_true: bool,
    kind: ConditionalRuleKind,
}

#[derive(Clone, Debug)]
enum ConditionalRuleKind {
    CellIs {
        operator: String,
        formulas: Vec<String>,
        differential_style_id: Option<usize>,
    },
    ColorScale {
        thresholds: Vec<ConditionalThreshold>,
        colors: Vec<String>,
    },
    DataBar {
        thresholds: Vec<ConditionalThreshold>,
        color: String,
        minimum_length: f64,
        maximum_length: f64,
    },
    ContainsBlanks {
        invert: bool,
        differential_style_id: Option<usize>,
    },
    ContainsText {
        text: String,
        invert: bool,
        differential_style_id: Option<usize>,
    },
    Expression {
        formula: String,
        differential_style_id: Option<usize>,
    },
    Unsupported(String),
}

#[derive(Clone, Debug)]
struct ConditionalThreshold {
    kind: String,
    value: Option<f64>,
}

#[derive(Clone, Debug, Default)]
struct AppliedConditionalStyle {
    fill: Option<String>,
    font_color: Option<String>,
    data_bar: Option<(f64, f64, String)>,
}

#[derive(Clone, Copy, Debug)]
struct MergeRange {
    start_row: usize,
    start_column: usize,
    end_row: usize,
    end_column: usize,
}

#[derive(Clone, Debug, Default)]
struct DrawingMarker {
    column: usize,
    row: usize,
    column_offset: f64,
    row_offset: f64,
}

#[derive(Clone, Debug, Default)]
struct DrawingImage {
    from: DrawingMarker,
    to: Option<DrawingMarker>,
    position: Option<(f64, f64)>,
    width: f64,
    height: f64,
    href: String,
    name: String,
    alt_text: String,
}

#[derive(Clone, Debug, Default)]
struct DrawingShape {
    from: DrawingMarker,
    to: Option<DrawingMarker>,
    position: Option<(f64, f64)>,
    width: f64,
    height: f64,
    name: String,
    alt_text: String,
    preset: String,
    fill: Paint,
    stroke: Stroke,
    text: String,
    font_family: String,
    font_size: f64,
    text_fill: Paint,
}

#[derive(Clone, Debug, Default)]
struct DrawingChart {
    from: DrawingMarker,
    to: Option<DrawingMarker>,
    position: Option<(f64, f64)>,
    width: f64,
    height: f64,
    name: String,
    data: ChartData,
}

#[derive(Clone, Debug)]
enum DrawingObject {
    Image(DrawingImage),
    Shape(Box<DrawingShape>),
    Chart(Box<DrawingChart>),
}

#[derive(Clone, Debug)]
struct DrawingAnchorBuilder {
    from: DrawingMarker,
    to: DrawingMarker,
    has_to: bool,
    width: f64,
    height: f64,
    explicit_x: Option<f64>,
    explicit_y: Option<f64>,
    relationship_id: String,
    name: String,
    alt_text: String,
    hidden: bool,
    has_shape: bool,
    preset: String,
    fill: Paint,
    stroke: Stroke,
    text: String,
    font_family: String,
    font_size: f64,
    text_fill: Paint,
    chart_relationship_id: String,
}

impl Default for DrawingAnchorBuilder {
    fn default() -> Self {
        Self {
            from: DrawingMarker::default(),
            to: DrawingMarker::default(),
            has_to: false,
            width: 0.0,
            height: 0.0,
            explicit_x: None,
            explicit_y: None,
            relationship_id: String::new(),
            name: String::new(),
            alt_text: String::new(),
            hidden: false,
            has_shape: false,
            preset: String::new(),
            fill: Paint::None,
            stroke: Stroke {
                paint: Paint::None,
                width: 0.75,
                miter_limit: 10.0,
                ..Stroke::default()
            },
            text: String::new(),
            font_family: "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into(),
            font_size: 11.0,
            text_fill: Paint::solid("#000000"),
            chart_relationship_id: String::new(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_drawing_objects(
    xml: &[u8],
    relationships: &Relationships,
    drawing_part: &str,
    package: &mut ZipPackage<std::fs::File>,
    max_events: usize,
) -> Result<(Vec<DrawingObject>, Vec<String>)> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut anchor = None::<DrawingAnchorBuilder>;
    let mut captured_field = None::<(bool, String)>;
    let mut captured_text = String::new();
    let mut shape_text = String::new();
    let mut objects = Vec::new();
    let mut warnings = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "drawing {drawing_part} exceeds {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                match name.as_str() {
                    "oneCellAnchor" | "twoCellAnchor" | "absoluteAnchor" => {
                        anchor = Some(DrawingAnchorBuilder::default());
                    }
                    "col" | "row" | "colOff" | "rowOff" if anchor.is_some() => {
                        captured_field =
                            Some((stack.iter().any(|item| item == "to"), name.clone()));
                        captured_text.clear();
                    }
                    "t" if anchor.as_ref().is_some_and(|anchor| anchor.has_shape) => {
                        shape_text.clear();
                    }
                    _ => apply_drawing_event(&start, &name, &stack, anchor.as_mut()),
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                apply_drawing_event(&start, &name, &stack, anchor.as_mut());
            }
            Event::Text(text) if captured_field.is_some() => {
                captured_text.push_str(&decode_xlsx_text(&text, "drawing marker")?);
            }
            Event::GeneralRef(reference) if captured_field.is_some() => {
                captured_text.push_str(&decode_xlsx_reference(&reference, "drawing marker")?);
            }
            Event::Text(text)
                if stack.last().is_some_and(|name| name == "t")
                    && anchor.as_ref().is_some_and(|anchor| anchor.has_shape) =>
            {
                shape_text.push_str(&decode_xlsx_text(&text, "drawing text")?);
            }
            Event::GeneralRef(reference)
                if stack.last().is_some_and(|name| name == "t")
                    && anchor.as_ref().is_some_and(|anchor| anchor.has_shape) =>
            {
                shape_text.push_str(&decode_xlsx_reference(&reference, "drawing text")?);
            }
            Event::End(end) => {
                let name = String::from_utf8_lossy(local_name(end.name().as_ref())).into_owned();
                if matches!(name.as_str(), "col" | "row" | "colOff" | "rowOff")
                    && let Some((is_to, field)) = captured_field.take()
                    && let Some(anchor) = anchor.as_mut()
                {
                    apply_drawing_marker_value(anchor, is_to, &field, &captured_text);
                } else if name == "t"
                    && let Some(anchor) = anchor.as_mut()
                    && anchor.has_shape
                {
                    anchor.text.push_str(&shape_text);
                } else if name == "p"
                    && let Some(anchor) = anchor.as_mut()
                    && anchor.has_shape
                    && !anchor.text.ends_with('\n')
                {
                    anchor.text.push('\n');
                } else if matches!(
                    name.as_str(),
                    "oneCellAnchor" | "twoCellAnchor" | "absoluteAnchor"
                ) && let Some(mut finished) = anchor.take()
                {
                    if finished.hidden {
                        stack.pop();
                        buffer.clear();
                        continue;
                    }
                    if finished.has_shape && !finished.text.trim().is_empty() {
                        if finished.width <= 0.0 {
                            finished.width = finished
                                .text
                                .lines()
                                .map(|line| {
                                    line.chars()
                                        .map(|character| {
                                            finished.font_size * text_advance_factor(character)
                                        })
                                        .sum::<f64>()
                                })
                                .fold(0.0, f64::max)
                                + 6.0;
                        }
                        if finished.height <= 0.0 {
                            finished.height = finished.text.lines().count().max(1) as f64
                                * finished.font_size
                                * 1.2
                                + 6.0;
                        }
                    }
                    if !finished.relationship_id.is_empty() {
                        if let Some(media_part) =
                            relationships.target(&finished.relationship_id, drawing_part)
                        {
                            if let Some(bytes) = package.read_optional(&media_part)? {
                                objects.push(DrawingObject::Image(DrawingImage {
                                    from: finished.from.clone(),
                                    to: finished.has_to.then_some(finished.to.clone()),
                                    position: finished.explicit_x.zip(finished.explicit_y),
                                    width: finished.width,
                                    height: finished.height,
                                    href: format!(
                                        "data:{};base64,{}",
                                        drawing_mime_type(&media_part),
                                        base64::engine::general_purpose::STANDARD.encode(bytes)
                                    ),
                                    name: finished.name.clone(),
                                    alt_text: finished.alt_text.clone(),
                                }));
                            } else {
                                warnings
                                    .push(format!("drawing image part {media_part} is missing"));
                            }
                        } else if let Some(target) =
                            relationships.external_target(&finished.relationship_id)
                        {
                            warnings
                                .push(format!("external drawing image {target} was not fetched"));
                        }
                    }
                    if !finished.chart_relationship_id.is_empty() {
                        if let Some(chart_part) =
                            relationships.target(&finished.chart_relationship_id, drawing_part)
                        {
                            if let Some(chart_xml) = package.read_optional(&chart_part)? {
                                objects.push(DrawingObject::Chart(Box::new(DrawingChart {
                                    from: finished.from.clone(),
                                    to: finished.has_to.then_some(finished.to.clone()),
                                    position: finished.explicit_x.zip(finished.explicit_y),
                                    width: finished.width,
                                    height: finished.height,
                                    name: finished.name.clone(),
                                    data: parse_chart(&chart_xml, max_events)?,
                                })));
                            } else {
                                warnings.push(format!("chart part {chart_part} is missing"));
                            }
                        } else {
                            warnings.push(format!(
                                "chart relationship {} is missing",
                                finished.chart_relationship_id
                            ));
                        }
                    }
                    if finished.has_shape {
                        objects.push(DrawingObject::Shape(Box::new(DrawingShape {
                            from: finished.from,
                            to: finished.has_to.then_some(finished.to),
                            position: finished.explicit_x.zip(finished.explicit_y),
                            width: finished.width,
                            height: finished.height,
                            name: finished.name,
                            alt_text: finished.alt_text,
                            preset: finished.preset,
                            fill: finished.fill,
                            stroke: finished.stroke,
                            text: finished.text.trim_end_matches('\n').into(),
                            font_family: finished.font_family,
                            font_size: finished.font_size,
                            text_fill: finished.text_fill,
                        })));
                    }
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((objects, deduplicate(warnings)))
}

fn apply_drawing_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    stack: &[String],
    anchor: Option<&mut DrawingAnchorBuilder>,
) {
    let Some(anchor) = anchor else {
        return;
    };
    match name {
        "sp" => anchor.has_shape = true,
        "pos" if stack.last().is_some_and(|item| item == "absoluteAnchor") => {
            anchor.from.column = 0;
            anchor.from.row = 0;
            anchor.from.column_offset = parse_i64(attribute(start, b"x"), 0) as f64 / 12_700.0;
            anchor.from.row_offset = parse_i64(attribute(start, b"y"), 0) as f64 / 12_700.0;
        }
        "ext"
            if stack.last().is_some_and(|item| {
                matches!(item.as_str(), "oneCellAnchor" | "absoluteAnchor")
            }) =>
        {
            anchor.width = parse_i64(attribute(start, b"cx"), 0).max(0) as f64 / 12_700.0;
            anchor.height = parse_i64(attribute(start, b"cy"), 0).max(0) as f64 / 12_700.0;
        }
        "off"
            if stack.last().is_some_and(|item| item == "xfrm")
                && stack.iter().any(|item| item == "spPr") =>
        {
            anchor.explicit_x = Some(parse_i64(attribute(start, b"x"), 0) as f64 / 12_700.0);
            anchor.explicit_y = Some(parse_i64(attribute(start, b"y"), 0) as f64 / 12_700.0);
        }
        "ext"
            if stack.last().is_some_and(|item| item == "xfrm")
                && stack.iter().any(|item| item == "spPr") =>
        {
            anchor.width = parse_i64(attribute(start, b"cx"), 0).max(0) as f64 / 12_700.0;
            anchor.height = parse_i64(attribute(start, b"cy"), 0).max(0) as f64 / 12_700.0;
        }
        "blip" => anchor.relationship_id = attribute(start, b"embed").unwrap_or_default(),
        "cNvPr" => {
            anchor.name = attribute(start, b"name").unwrap_or_default();
            anchor.alt_text = attribute(start, b"descr").unwrap_or_default();
            anchor.hidden =
                attribute(start, b"hidden").is_some_and(|value| value == "1" || value == "true");
        }
        "prstGeom" => {
            anchor.preset = attribute(start, b"prst").unwrap_or_else(|| "rect".into());
        }
        "ln" => {
            anchor.stroke.width = parse_i64(attribute(start, b"w"), 9_525).max(0) as f64 / 12_700.0;
        }
        "noFill" => {
            if stack.iter().any(|item| item == "ln") {
                anchor.stroke.paint = Paint::None;
            } else {
                anchor.fill = Paint::None;
            }
        }
        "srgbClr" => {
            let color = Paint::solid(color_from_hex(
                &attribute(start, b"val").unwrap_or_default(),
                "#000000",
            ));
            if stack.iter().any(|item| item == "ln") {
                anchor.stroke.paint = color;
            } else if stack.iter().any(|item| item == "rPr") {
                anchor.text_fill = color;
            } else {
                anchor.fill = color;
            }
        }
        "schemeClr" => {
            let color = Paint::solid(match attribute(start, b"val").as_deref() {
                Some("lt1") => "#FFFFFF",
                Some("dk1") | Some("tx1") => "#000000",
                Some("accent1") => "#4472C4",
                Some("accent2") => "#ED7D31",
                _ => "#000000",
            });
            if stack.iter().any(|item| item == "ln") {
                anchor.stroke.paint = color;
            } else if stack.iter().any(|item| item == "rPr") {
                anchor.text_fill = color;
            } else {
                anchor.fill = color;
            }
        }
        "rPr" => {
            if let Some(size) = attribute(start, b"sz") {
                anchor.font_size = size.parse::<f64>().unwrap_or(1_100.0) / 100.0;
            }
        }
        "latin" | "ea" => {
            if let Some(typeface) = attribute(start, b"typeface")
                && !typeface.is_empty()
            {
                anchor.font_family = typeface;
            }
        }
        "chart" => {
            anchor.chart_relationship_id = attribute(start, b"id").unwrap_or_default();
        }
        _ => {}
    }
}

fn apply_drawing_marker_value(
    anchor: &mut DrawingAnchorBuilder,
    is_to: bool,
    field: &str,
    value: &str,
) {
    let marker = if is_to {
        anchor.has_to = true;
        &mut anchor.to
    } else {
        &mut anchor.from
    };
    match field {
        "col" => marker.column = value.parse().unwrap_or(0),
        "row" => marker.row = value.parse().unwrap_or(0),
        "colOff" => marker.column_offset = value.parse::<f64>().unwrap_or(0.0) / 12_700.0,
        "rowOff" => marker.row_offset = value.parse::<f64>().unwrap_or(0.0) / 12_700.0,
        _ => {}
    }
}

fn drawing_mime_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

#[derive(Clone, Debug)]
struct Sheet {
    name: String,
    cells: BTreeMap<(usize, usize), Cell>,
    column_widths: HashMap<usize, f64>,
    hidden_columns: HashSet<usize>,
    row_heights: HashMap<usize, f64>,
    hidden_rows: HashSet<usize>,
    merges: Vec<MergeRange>,
    max_row: usize,
    max_column: usize,
    warnings: Vec<String>,
    conditional_rules: Vec<ConditionalRule>,
    print_areas: Vec<MergeRange>,
    print_titles: PrintTitles,
    row_breaks: Vec<usize>,
    column_breaks: Vec<usize>,
    print_setup: PrintSetup,
}

struct SheetGrid {
    column_positions: Vec<f64>,
    row_positions: Vec<f64>,
}

#[derive(Clone, Debug)]
struct PrintSetup {
    enabled: bool,
    paper_width: f64,
    paper_height: f64,
    margin_left: f64,
    margin_right: f64,
    margin_top: f64,
    margin_bottom: f64,
    fit_to_page: bool,
    fit_to_width: usize,
    fit_to_height: usize,
    scale: f64,
    center_horizontal: bool,
    center_vertical: bool,
}

impl Default for PrintSetup {
    fn default() -> Self {
        Self {
            enabled: false,
            paper_width: 612.0,
            paper_height: 792.0,
            margin_left: 0.7 * 72.0,
            margin_right: 0.7 * 72.0,
            margin_top: 0.75 * 72.0,
            margin_bottom: 0.75 * 72.0,
            fit_to_page: false,
            fit_to_width: 0,
            fit_to_height: 0,
            scale: 1.0,
            center_horizontal: false,
            center_vertical: false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_sheet(
    xml: &[u8],
    name: &str,
    shared_strings: &[String],
    styles: &Styles,
    relationships: &Relationships,
    sheet_part: &str,
    print_areas: Vec<MergeRange>,
    print_titles: PrintTitles,
    max_events: usize,
) -> Result<Sheet> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut sheet = Sheet {
        name: name.into(),
        cells: BTreeMap::new(),
        column_widths: HashMap::new(),
        hidden_columns: HashSet::new(),
        row_heights: HashMap::new(),
        hidden_rows: HashSet::new(),
        merges: Vec::new(),
        max_row: 1,
        max_column: 1,
        warnings: Vec::new(),
        conditional_rules: Vec::new(),
        print_areas,
        print_titles,
        row_breaks: Vec::new(),
        column_breaks: Vec::new(),
        print_setup: PrintSetup::default(),
    };
    let mut current_cell = None::<Cell>;
    let mut capture = None::<String>;
    let mut text = String::new();
    let mut inline_text = String::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "worksheet {name} exceeds {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => match local_name(start.name().as_ref()) {
                b"c" => {
                    let reference = attribute(&start, b"r").unwrap_or_default();
                    let (row, column) =
                        parse_cell_reference(&reference).unwrap_or((sheet.max_row, 1));
                    current_cell = Some(Cell {
                        row,
                        column,
                        style_id: parse_i64(attribute(&start, b"s"), 0).max(0) as usize,
                        data_type: attribute(&start, b"t").unwrap_or_default(),
                        ..Cell::default()
                    });
                    inline_text.clear();
                }
                b"v" | b"t" | b"f" if current_cell.is_some() => {
                    capture = Some(
                        String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned(),
                    );
                    text.clear();
                }
                b"row" => apply_row(&start, &mut sheet),
                b"col" => apply_column(&start, &mut sheet),
                _ => {}
            },
            Event::Empty(start) => match local_name(start.name().as_ref()) {
                b"row" => apply_row(&start, &mut sheet),
                b"col" => apply_column(&start, &mut sheet),
                b"mergeCell" => {
                    if let Some(range) =
                        attribute(&start, b"ref").and_then(|value| parse_merge_range(&value))
                    {
                        sheet.max_row = sheet.max_row.max(range.end_row);
                        sheet.max_column = sheet.max_column.max(range.end_column);
                        sheet.merges.push(range);
                    }
                }
                _ => {}
            },
            Event::Text(value) if capture.is_some() => {
                text.push_str(&decode_xlsx_text(&value, &format!("cell text in {name}"))?)
            }
            Event::GeneralRef(reference) if capture.is_some() => text.push_str(
                &decode_xlsx_reference(&reference, &format!("cell text in {name}"))?,
            ),
            Event::End(end) => match local_name(end.name().as_ref()) {
                b"v" => {
                    if let Some(cell) = current_cell.as_mut() {
                        cell.value = resolve_cell_value(
                            &text,
                            &cell.data_type,
                            cell.style_id,
                            shared_strings,
                            styles,
                        );
                        if matches!(cell.data_type.as_str(), "" | "n" | "b") {
                            cell.raw_value = std::mem::take(&mut text);
                        } else {
                            text.clear();
                        }
                    }
                    capture = None;
                }
                b"f" => {
                    if let Some(cell) = current_cell.as_mut() {
                        cell.formula = std::mem::take(&mut text);
                    }
                    capture = None;
                }
                b"t" => {
                    if inline_text.is_empty() {
                        std::mem::swap(&mut inline_text, &mut text);
                    } else {
                        inline_text.push_str(&text);
                        text.clear();
                    }
                    capture = None;
                }
                b"c" => {
                    if let Some(mut cell) = current_cell.take() {
                        if cell.value.is_empty() && !inline_text.is_empty() {
                            cell.value = std::mem::take(&mut inline_text);
                        }
                        sheet.max_row = sheet.max_row.max(cell.row);
                        sheet.max_column = sheet.max_column.max(cell.column);
                        sheet.cells.insert((cell.row, cell.column), cell);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    sheet.conditional_rules = parse_conditional_rules(xml, max_events)?;
    (sheet.row_breaks, sheet.column_breaks) = parse_manual_page_breaks(xml, max_events)?;
    sheet.print_setup = parse_print_setup(xml, max_events)?;
    let _ = (relationships, sheet_part);
    if sheet.cells.len() > MAX_POPULATED_CELLS {
        return Err(Error::LimitExceeded(format!(
            "worksheet {name} contains {} populated cells; maximum is {MAX_POPULATED_CELLS}",
            sheet.cells.len()
        )));
    }
    Ok(sheet)
}

#[allow(clippy::too_many_arguments)]
fn build_formula_context(
    current_sheet: &str,
    sheet: &Sheet,
    workbook: &WorkbookInfo,
    workbook_relationships: &Relationships,
    workbook_part: &str,
    package: &mut ZipPackage<std::fs::File>,
    shared_strings: &[String],
    styles: &Styles,
    max_events: usize,
) -> Result<FormulaContext> {
    let mut targets = workbook
        .defined_names
        .values()
        .filter(|target| target.sheet != current_sheet)
        .cloned()
        .collect::<HashSet<_>>();
    for cell in sheet.cells.values().filter(|cell| !cell.formula.is_empty()) {
        targets.extend(
            external_formula_targets(&cell.formula)
                .into_iter()
                .filter(|target| target.sheet != current_sheet),
        );
    }
    let mut by_sheet = HashMap::<String, HashSet<(usize, usize)>>::new();
    for target in &targets {
        by_sheet
            .entry(target.sheet.clone())
            .or_default()
            .insert((target.row, target.column));
    }
    let mut external_values = HashMap::new();
    for (sheet_name, positions) in by_sheet {
        let Some((_, relationship_id)) =
            workbook.sheets.iter().find(|(name, _)| name == &sheet_name)
        else {
            continue;
        };
        let Some(sheet_part) = workbook_relationships.target(relationship_id, workbook_part) else {
            continue;
        };
        let xml = package.read(&sheet_part)?;
        let values =
            read_selected_sheet_values(&xml, &positions, shared_strings, styles, max_events)?;
        for (row, column) in positions {
            external_values.insert(
                CellTarget {
                    sheet: sheet_name.clone(),
                    row,
                    column,
                },
                values
                    .get(&(row, column))
                    .cloned()
                    .unwrap_or(FormulaScalar::Blank),
            );
        }
    }
    Ok(FormulaContext {
        sheet_name: current_sheet.into(),
        defined_names: workbook.defined_names.clone(),
        external_values,
    })
}

fn external_formula_targets(formula: &str) -> Vec<CellTarget> {
    let mut output = Vec::new();
    for (bang, _) in formula.match_indices('!') {
        let before = formula[..bang].trim_end();
        let sheet = if let Some(quoted) = before.strip_suffix('\'') {
            quoted
                .rfind('\'')
                .map(|start| quoted[start + 1..].replace("''", "'"))
        } else {
            before
                .rsplit(|character: char| {
                    character.is_whitespace()
                        || matches!(character, '(' | ')' | ',' | '+' | '-' | '*' | '/' | '=')
                })
                .next()
                .map(str::to_owned)
        };
        let Some(sheet) = sheet.filter(|sheet| !sheet.is_empty()) else {
            continue;
        };
        let reference = formula[bang + 1..]
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '$')
            .collect::<String>();
        if let Some((row, column)) = parse_cell_reference(&reference) {
            output.push(CellTarget { sheet, row, column });
        }
    }
    output
}

fn read_selected_sheet_values(
    xml: &[u8],
    positions: &HashSet<(usize, usize)>,
    shared_strings: &[String],
    styles: &Styles,
    max_events: usize,
) -> Result<HashMap<(usize, usize), FormulaScalar>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut current = None::<((usize, usize), String, usize)>;
    let mut capture = false;
    let mut text = String::new();
    let mut inline_text = String::new();
    let mut output = HashMap::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "XLSX selected-cell event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) if local_name(start.name().as_ref()) == b"c" => {
                let position =
                    attribute(&start, b"r").and_then(|reference| parse_cell_reference(&reference));
                current = position
                    .filter(|position| positions.contains(position))
                    .map(|position| {
                        (
                            position,
                            attribute(&start, b"t").unwrap_or_default(),
                            parse_i64(attribute(&start, b"s"), 0).max(0) as usize,
                        )
                    });
                inline_text.clear();
            }
            Event::Start(start)
                if current.is_some()
                    && matches!(local_name(start.name().as_ref()), b"v" | b"t") =>
            {
                capture = true;
                text.clear();
            }
            Event::Text(value) if capture => {
                text.push_str(&decode_xlsx_text(&value, "selected cell")?);
            }
            Event::GeneralRef(reference) if capture => {
                text.push_str(&decode_xlsx_reference(&reference, "selected cell")?);
            }
            Event::End(end) if current.is_some() => match local_name(end.name().as_ref()) {
                b"v" => {
                    if let Some((position, data_type, style_id)) = current.as_ref() {
                        let value =
                            resolve_cell_value(&text, data_type, *style_id, shared_strings, styles);
                        output.insert(*position, scalar_from_cell_text(&text, &value, data_type));
                    }
                    capture = false;
                }
                b"t" => {
                    inline_text.push_str(&text);
                    capture = false;
                }
                b"c" => {
                    if let Some((position, _, _)) = current.take()
                        && !inline_text.is_empty()
                    {
                        output.insert(
                            position,
                            FormulaScalar::Text(std::mem::take(&mut inline_text)),
                        );
                    }
                }
                _ => {}
            },
            Event::End(end) if local_name(end.name().as_ref()) == b"c" => current = None,
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(output)
}

fn scalar_from_cell_text(raw: &str, value: &str, data_type: &str) -> FormulaScalar {
    if matches!(data_type, "" | "n" | "b")
        && let Ok(number) = raw.parse::<f64>()
    {
        FormulaScalar::Number(number)
    } else if value.is_empty() {
        FormulaScalar::Blank
    } else {
        FormulaScalar::Text(value.into())
    }
}

#[derive(Default)]
struct FormulaContext {
    sheet_name: String,
    defined_names: HashMap<String, CellTarget>,
    external_values: HashMap<CellTarget, FormulaScalar>,
}

fn evaluate_missing_formulas(sheet: &mut Sheet, styles: &Styles, context: &FormulaContext) {
    let positions = sheet
        .cells
        .iter()
        .filter(|(_, cell)| {
            !cell.formula.is_empty() && cell.raw_value.is_empty() && cell.value.is_empty()
        })
        .map(|(position, _)| *position)
        .collect::<Vec<_>>();
    let mut memo = HashMap::<(usize, usize), FormulaScalar>::new();
    let mut failed = Vec::new();
    for position in positions {
        let mut visiting = HashSet::new();
        let mut operations = 0usize;
        if let Some(value) = evaluate_formula_position(
            position,
            &sheet.cells,
            context,
            &mut memo,
            &mut visiting,
            &mut operations,
            0,
        ) {
            if let Some(cell) = sheet.cells.get_mut(&position) {
                match value {
                    FormulaScalar::Number(value) => {
                        cell.raw_value = value.to_string();
                        cell.value =
                            styles.format_value(&cell.raw_value, styles.cell_style(cell.style_id));
                    }
                    FormulaScalar::Text(value) => cell.value = value,
                    FormulaScalar::Blank => {}
                }
            }
        } else {
            failed.push(cell_reference(position.0, position.1));
        }
    }
    if !failed.is_empty() {
        let sample = failed
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        sheet.warnings.push(format!(
            "XLSX formulas without cached values could not be evaluated in {sample}{}",
            if failed.len() > 8 { ", …" } else { "" }
        ));
    }
}

fn evaluate_formula_position(
    position: (usize, usize),
    cells: &BTreeMap<(usize, usize), Cell>,
    context: &FormulaContext,
    memo: &mut HashMap<(usize, usize), FormulaScalar>,
    visiting: &mut HashSet<(usize, usize)>,
    operations: &mut usize,
    depth: usize,
) -> Option<FormulaScalar> {
    if depth > 64 || *operations > 100_000 {
        return None;
    }
    if let Some(value) = memo.get(&position) {
        return Some(value.clone());
    }
    let cell = cells.get(&position)?;
    if let Ok(value) = cell.raw_value.parse::<f64>() {
        let value = FormulaScalar::Number(value);
        memo.insert(position, value.clone());
        return Some(value);
    }
    if cell.formula.is_empty() || cell.formula.len() > 1024 * 1024 || !visiting.insert(position) {
        return None;
    }
    let (parsed, complete) = {
        let mut parser = FormulaParser {
            source: cell.formula.trim().trim_start_matches('=').as_bytes(),
            position: 0,
            cells,
            context,
            memo,
            visiting,
            operations,
            depth,
        };
        let parsed = parser.parse_comparison().and_then(|value| value.scalar());
        parser.skip_spaces();
        let complete = parsed.as_ref().is_some_and(|value| {
            parser.position == parser.source.len()
                && !matches!(value, FormulaScalar::Number(value) if !value.is_finite())
        });
        (parsed, complete)
    };
    visiting.remove(&position);
    complete.then(|| {
        let value = parsed.expect("checked above");
        memo.insert(position, value.clone());
        value
    })
}

fn evaluate_formula_expression(
    formula: &str,
    cells: &BTreeMap<(usize, usize), Cell>,
    context: &FormulaContext,
) -> Option<bool> {
    if formula.len() > 1024 * 1024 {
        return None;
    }
    let mut memo = HashMap::new();
    let mut visiting = HashSet::new();
    let mut operations = 0usize;
    let mut parser = FormulaParser {
        source: formula.trim().trim_start_matches('=').as_bytes(),
        position: 0,
        cells,
        context,
        memo: &mut memo,
        visiting: &mut visiting,
        operations: &mut operations,
        depth: 0,
    };
    let value = parser.parse_comparison()?;
    parser.skip_spaces();
    if parser.position != parser.source.len() {
        return None;
    }
    match value {
        FormulaValue::Number(value) => Some(value.is_finite() && value != 0.0),
        FormulaValue::Text(value) => Some(!value.is_empty()),
        FormulaValue::Range { values, .. } => Some(values.iter().any(|value| match value {
            FormulaScalar::Number(value) => value.is_finite() && *value != 0.0,
            FormulaScalar::Text(value) => !value.is_empty(),
            FormulaScalar::Blank => false,
        })),
    }
}

#[derive(Clone, Debug)]
enum FormulaValue {
    Number(f64),
    Range {
        values: Vec<FormulaScalar>,
        rows: usize,
        columns: usize,
    },
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
enum FormulaScalar {
    Number(f64),
    Text(String),
    Blank,
}

impl FormulaScalar {
    fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Text(value) => value.parse().ok(),
            Self::Blank => None,
        }
    }

    fn as_text(&self) -> Option<&str> {
        match self {
            Self::Number(_) => None,
            Self::Text(value) => Some(value),
            Self::Blank => Some(""),
        }
    }
}

impl FormulaValue {
    fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::Text(value) => value.parse().ok(),
            Self::Range { .. } => None,
        }
    }

    fn numbers(&self) -> Vec<f64> {
        match self {
            Self::Number(value) => vec![*value],
            Self::Range { values, .. } => {
                values.iter().filter_map(FormulaScalar::as_number).collect()
            }
            Self::Text(value) => value.parse().ok().into_iter().collect(),
        }
    }

    fn scalars(&self) -> Vec<FormulaScalar> {
        match self {
            Self::Number(value) => vec![FormulaScalar::Number(*value)],
            Self::Range { values, .. } => values.clone(),
            Self::Text(value) => vec![FormulaScalar::Text(value.clone())],
        }
    }

    fn scalar(&self) -> Option<FormulaScalar> {
        match self {
            Self::Number(value) => Some(FormulaScalar::Number(*value)),
            Self::Text(value) if value.is_empty() => Some(FormulaScalar::Blank),
            Self::Text(value) => Some(FormulaScalar::Text(value.clone())),
            Self::Range { .. } => None,
        }
    }

    fn range(&self) -> Option<(&[FormulaScalar], usize, usize)> {
        match self {
            Self::Range {
                values,
                rows,
                columns,
            } => Some((values, *rows, *columns)),
            _ => None,
        }
    }
}

struct FormulaParser<'a, 'state> {
    source: &'a [u8],
    position: usize,
    cells: &'a BTreeMap<(usize, usize), Cell>,
    context: &'a FormulaContext,
    memo: &'state mut HashMap<(usize, usize), FormulaScalar>,
    visiting: &'state mut HashSet<(usize, usize)>,
    operations: &'state mut usize,
    depth: usize,
}

impl FormulaParser<'_, '_> {
    fn parse_comparison(&mut self) -> Option<FormulaValue> {
        let left = self.parse_expression()?;
        self.skip_spaces();
        let operators = ["<=", ">=", "<>", "=", "<", ">"];
        let operator = operators
            .iter()
            .find(|operator| self.remaining().starts_with(operator.as_bytes()))
            .copied();
        let Some(operator) = operator else {
            return Some(left);
        };
        self.position += operator.len();
        let right = self.parse_expression()?;
        formula_comparison_value(&left, &right, operator)
    }

    fn parse_expression(&mut self) -> Option<FormulaValue> {
        let mut value = self.parse_term()?;
        loop {
            self.skip_spaces();
            let Some(operator) = self.source.get(self.position).copied() else {
                break;
            };
            if !matches!(operator, b'+' | b'-') {
                break;
            }
            self.position += 1;
            let right = self.parse_term()?.as_number()?;
            let left = value.as_number()?;
            value = FormulaValue::Number(if operator == b'+' {
                left + right
            } else {
                left - right
            });
            self.bump()?;
        }
        Some(value)
    }

    fn parse_term(&mut self) -> Option<FormulaValue> {
        let mut value = self.parse_power()?;
        loop {
            self.skip_spaces();
            let Some(operator) = self.source.get(self.position).copied() else {
                break;
            };
            if !matches!(operator, b'*' | b'/') {
                break;
            }
            self.position += 1;
            let right = self.parse_power()?.as_number()?;
            let left = value.as_number()?;
            let result = if operator == b'*' {
                left * right
            } else {
                left / right
            };
            if !result.is_finite() {
                return None;
            }
            value = FormulaValue::Number(result);
            self.bump()?;
        }
        Some(value)
    }

    fn parse_power(&mut self) -> Option<FormulaValue> {
        let left = self.parse_unary()?;
        self.skip_spaces();
        if self.source.get(self.position) == Some(&b'^') {
            self.position += 1;
            let right = self.parse_power()?.as_number()?;
            return Some(FormulaValue::Number(left.as_number()?.powf(right)));
        }
        Some(left)
    }

    fn parse_unary(&mut self) -> Option<FormulaValue> {
        self.skip_spaces();
        if self.source.get(self.position) == Some(&b'+') {
            self.position += 1;
            return self.parse_unary();
        }
        if self.source.get(self.position) == Some(&b'-') {
            self.position += 1;
            return Some(FormulaValue::Number(-self.parse_unary()?.as_number()?));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Option<FormulaValue> {
        self.skip_spaces();
        if self.source.get(self.position) == Some(&b'(') {
            self.position += 1;
            let value = self.parse_comparison()?;
            self.skip_spaces();
            (self.source.get(self.position) == Some(&b')')).then(|| self.position += 1)?;
            return self.apply_percent(value);
        }
        if self.source.get(self.position) == Some(&b'"') {
            self.position += 1;
            let start = self.position;
            while self
                .source
                .get(self.position)
                .is_some_and(|byte| *byte != b'"')
            {
                self.position += 1;
            }
            let value = String::from_utf8_lossy(&self.source[start..self.position]).into_owned();
            self.position += usize::from(self.position < self.source.len());
            return Some(FormulaValue::Text(value));
        }
        if self.source.get(self.position) == Some(&b'\'') {
            return self.parse_quoted_sheet_reference();
        }
        if self
            .source
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.')
        {
            let start = self.position;
            self.position += 1;
            while self.source.get(self.position).is_some_and(|byte| {
                byte.is_ascii_digit() || matches!(*byte, b'.' | b'e' | b'E' | b'+' | b'-')
            }) {
                if matches!(self.source[self.position], b'+' | b'-')
                    && !matches!(self.source[self.position - 1], b'e' | b'E')
                {
                    break;
                }
                self.position += 1;
            }
            let value = std::str::from_utf8(&self.source[start..self.position])
                .ok()?
                .parse::<f64>()
                .ok()?;
            return self.apply_percent(FormulaValue::Number(value));
        }
        let start = self.position;
        while self.source.get(self.position).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'.' | b'$') || *byte >= 0x80
        }) {
            self.position += 1;
        }
        if start == self.position {
            return None;
        }
        let identifier = std::str::from_utf8(&self.source[start..self.position]).ok()?;
        self.skip_spaces();
        if self.source.get(self.position) == Some(&b'!') {
            self.position += 1;
            return self.parse_sheet_cell_reference(identifier);
        }
        if self.source.get(self.position) == Some(&b'(') {
            return self.parse_function(identifier);
        }
        if identifier.eq_ignore_ascii_case("TRUE") {
            return Some(FormulaValue::Number(1.0));
        }
        if identifier.eq_ignore_ascii_case("FALSE") {
            return Some(FormulaValue::Number(0.0));
        }
        if let Some(target) = self.context.defined_names.get(identifier).cloned() {
            return self.formula_value_for_target(&target);
        }
        let first = parse_cell_reference(identifier)?;
        self.skip_spaces();
        if self.source.get(self.position) == Some(&b':') {
            self.position += 1;
            self.skip_spaces();
            let range_start = self.position;
            while self
                .source
                .get(self.position)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'$')
            {
                self.position += 1;
            }
            let second = parse_cell_reference(
                std::str::from_utf8(&self.source[range_start..self.position]).ok()?,
            )?;
            let rows = first.0.abs_diff(second.0) + 1;
            let columns = first.1.abs_diff(second.1) + 1;
            let count = rows.checked_mul(columns)?;
            if count > 100_000 {
                return None;
            }
            let mut values = Vec::new();
            for row in first.0.min(second.0)..=first.0.max(second.0) {
                for column in first.1.min(second.1)..=first.1.max(second.1) {
                    self.bump()?;
                    values.push(self.cell_scalar((row, column)));
                }
            }
            return Some(FormulaValue::Range {
                values,
                rows,
                columns,
            });
        }
        let value = self.cell_scalar(first);
        match value {
            FormulaScalar::Number(value) => self.apply_percent(FormulaValue::Number(value)),
            FormulaScalar::Text(value) => self.apply_percent(FormulaValue::Text(value)),
            FormulaScalar::Blank => self.apply_percent(FormulaValue::Text(String::new())),
        }
    }

    fn parse_quoted_sheet_reference(&mut self) -> Option<FormulaValue> {
        self.position += 1;
        let mut sheet = String::new();
        while self.position < self.source.len() {
            if self.source[self.position] == b'\'' {
                if self.source.get(self.position + 1) == Some(&b'\'') {
                    sheet.push('\'');
                    self.position += 2;
                    continue;
                }
                self.position += 1;
                break;
            }
            let remaining = std::str::from_utf8(&self.source[self.position..]).ok()?;
            let character = remaining.chars().next()?;
            sheet.push(character);
            self.position += character.len_utf8();
        }
        self.skip_spaces();
        (self.source.get(self.position) == Some(&b'!')).then(|| self.position += 1)?;
        self.parse_sheet_cell_reference(&sheet)
    }

    fn parse_sheet_cell_reference(&mut self, sheet: &str) -> Option<FormulaValue> {
        self.skip_spaces();
        let start = self.position;
        while self
            .source
            .get(self.position)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'$')
        {
            self.position += 1;
        }
        let (row, column) =
            parse_cell_reference(std::str::from_utf8(&self.source[start..self.position]).ok()?)?;
        self.formula_value_for_target(&CellTarget {
            sheet: sheet.into(),
            row,
            column,
        })
    }

    fn formula_value_for_target(&mut self, target: &CellTarget) -> Option<FormulaValue> {
        let value = if target.sheet == self.context.sheet_name {
            self.cell_scalar((target.row, target.column))
        } else {
            self.context.external_values.get(target)?.clone()
        };
        Some(formula_value_from_scalar(value))
    }

    fn parse_function(&mut self, identifier: &str) -> Option<FormulaValue> {
        self.position += 1;
        let mut arguments = Vec::new();
        loop {
            self.skip_spaces();
            if self.source.get(self.position) == Some(&b')') {
                self.position += 1;
                break;
            }
            arguments.push(self.parse_comparison()?);
            self.skip_spaces();
            match self.source.get(self.position) {
                Some(b',') | Some(b';') => self.position += 1,
                Some(b')') => {
                    self.position += 1;
                    break;
                }
                _ => return None,
            }
        }
        let name = identifier.trim_start_matches("_xlfn.").to_ascii_uppercase();
        let flattened = || {
            arguments
                .iter()
                .flat_map(FormulaValue::numbers)
                .collect::<Vec<_>>()
        };
        if name == "VLOOKUP" {
            let lookup = arguments.first()?.scalar()?;
            let (table, rows, columns) = arguments.get(1)?.range()?;
            let result_column = arguments.get(2)?.as_number()?.round() as usize;
            let exact_match = arguments
                .get(3)
                .is_some_and(|value| value.as_number() == Some(0.0));
            if !exact_match || result_column == 0 || result_column > columns {
                return None;
            }
            for row in 0..rows {
                let row_start = row.checked_mul(columns)?;
                if formula_scalars_equal(table.get(row_start)?, &lookup) {
                    let result = table.get(row_start + result_column - 1)?.clone();
                    return self.apply_percent(formula_value_from_scalar(result));
                }
            }
            return None;
        }
        if matches!(name.as_str(), "CONCAT" | "CONCATENATE") {
            let text = arguments
                .iter()
                .flat_map(FormulaValue::scalars)
                .map(|value| match value {
                    FormulaScalar::Number(value) => value.to_string(),
                    FormulaScalar::Text(value) => value,
                    FormulaScalar::Blank => String::new(),
                })
                .collect::<String>();
            return self.apply_percent(FormulaValue::Text(text));
        }
        if name == "TRIM" {
            let text = arguments
                .first()?
                .scalar()
                .map(formula_value_from_scalar)
                .and_then(|value| match value {
                    FormulaValue::Text(value) => Some(value),
                    FormulaValue::Number(value) => Some(value.to_string()),
                    FormulaValue::Range { .. } => None,
                })?;
            return self.apply_percent(FormulaValue::Text(
                text.split_whitespace().collect::<Vec<_>>().join(" "),
            ));
        }
        let value = match name.as_str() {
            "SUM" => flattened().iter().sum(),
            "AVERAGE" => {
                let values = flattened();
                values.iter().sum::<f64>() / values.len().max(1) as f64
            }
            "MIN" => flattened().into_iter().fold(f64::INFINITY, f64::min),
            "MAX" => flattened().into_iter().fold(f64::NEG_INFINITY, f64::max),
            "COUNT" => flattened().len() as f64,
            "COUNTA" => arguments
                .iter()
                .flat_map(FormulaValue::scalars)
                .filter(|value| !matches!(value, FormulaScalar::Blank))
                .count() as f64,
            "LEN" => arguments
                .first()?
                .scalar()
                .map(formula_value_from_scalar)
                .and_then(|value| match value {
                    FormulaValue::Text(value) => Some(value.chars().count() as f64),
                    FormulaValue::Number(value) => Some(value.to_string().chars().count() as f64),
                    FormulaValue::Range { .. } => None,
                })?,
            "ABS" => arguments.first()?.as_number()?.abs(),
            "SQRT" => arguments.first()?.as_number()?.sqrt(),
            "AND" => f64::from(flattened().iter().all(|value| *value != 0.0) as u8),
            "OR" => f64::from(flattened().iter().any(|value| *value != 0.0) as u8),
            "NOT" => f64::from((arguments.first()?.as_number()? == 0.0) as u8),
            "ROUND" => {
                let value = arguments.first()?.as_number()?;
                let digits = arguments.get(1)?.as_number()?.round().clamp(-12.0, 12.0) as i32;
                let factor = 10f64.powi(digits);
                (value * factor).round() / factor
            }
            "COUNTIF" => {
                let values = arguments.first()?.scalars();
                let criterion = arguments.get(1)?;
                values
                    .iter()
                    .filter(|value| formula_criterion_matches(value, criterion))
                    .count() as f64
            }
            "COUNTIFS" => {
                if arguments.len() < 2 || arguments.len() % 2 != 0 {
                    return None;
                }
                let pairs = arguments
                    .chunks_exact(2)
                    .map(|pair| (pair[0].scalars(), &pair[1]))
                    .collect::<Vec<_>>();
                let length = pairs.first()?.0.len();
                if pairs.iter().any(|(values, _)| values.len() != length) {
                    return None;
                }
                (0..length)
                    .filter(|index| {
                        pairs.iter().all(|(values, criterion)| {
                            formula_criterion_matches(&values[*index], criterion)
                        })
                    })
                    .count() as f64
            }
            "SUMIF" | "AVERAGEIF" => {
                let criteria_values = arguments.first()?.scalars();
                let criterion = arguments.get(1)?;
                let sum_values = arguments
                    .get(2)
                    .map(FormulaValue::scalars)
                    .unwrap_or_else(|| criteria_values.clone());
                let matched = criteria_values
                    .iter()
                    .zip(sum_values.iter())
                    .filter(|(value, _)| formula_criterion_matches(value, criterion))
                    .filter_map(|(_, value)| value.as_number())
                    .collect::<Vec<_>>();
                if name == "AVERAGEIF" {
                    matched.iter().sum::<f64>() / matched.len().max(1) as f64
                } else {
                    matched.iter().sum()
                }
            }
            "SUMIFS" | "AVERAGEIFS" => {
                if arguments.len() < 3 || arguments.len() % 2 == 0 {
                    return None;
                }
                let aggregate_values = arguments.first()?.scalars();
                let pairs = arguments[1..]
                    .chunks_exact(2)
                    .map(|pair| (pair[0].scalars(), &pair[1]))
                    .collect::<Vec<_>>();
                if pairs
                    .iter()
                    .any(|(values, _)| values.len() != aggregate_values.len())
                {
                    return None;
                }
                let matched = aggregate_values
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| {
                        pairs.iter().all(|(values, criterion)| {
                            formula_criterion_matches(&values[*index], criterion)
                        })
                    })
                    .filter_map(|(_, value)| value.as_number())
                    .collect::<Vec<_>>();
                if name == "AVERAGEIFS" {
                    matched.iter().sum::<f64>() / matched.len().max(1) as f64
                } else {
                    matched.iter().sum()
                }
            }
            "IF" => {
                if arguments.first()?.as_number()? != 0.0 {
                    return arguments.get(1).cloned();
                }
                return arguments.get(2).cloned();
            }
            _ => return None,
        };
        self.apply_percent(FormulaValue::Number(value))
    }

    fn cell_scalar(&mut self, position: (usize, usize)) -> FormulaScalar {
        let Some(cell) = self.cells.get(&position) else {
            return FormulaScalar::Blank;
        };
        if let Ok(value) = cell.raw_value.parse::<f64>() {
            return FormulaScalar::Number(value);
        }
        if !cell.formula.is_empty()
            && let Some(value) = evaluate_formula_position(
                position,
                self.cells,
                self.context,
                self.memo,
                self.visiting,
                self.operations,
                self.depth + 1,
            )
        {
            return value;
        }
        if !cell.value.is_empty() {
            if let Ok(value) = cell.value.parse::<f64>() {
                FormulaScalar::Number(value)
            } else {
                FormulaScalar::Text(cell.value.clone())
            }
        } else if !cell.raw_value.is_empty() {
            FormulaScalar::Text(cell.raw_value.clone())
        } else {
            FormulaScalar::Blank
        }
    }

    fn apply_percent(&mut self, value: FormulaValue) -> Option<FormulaValue> {
        self.skip_spaces();
        if self.source.get(self.position) == Some(&b'%') {
            self.position += 1;
            return Some(FormulaValue::Number(value.as_number()? / 100.0));
        }
        Some(value)
    }

    fn skip_spaces(&mut self) {
        while self
            .source
            .get(self.position)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.position += 1;
        }
    }

    fn remaining(&self) -> &[u8] {
        &self.source[self.position..]
    }

    fn bump(&mut self) -> Option<()> {
        *self.operations += 1;
        (*self.operations <= 100_000).then_some(())
    }
}

fn formula_value_from_scalar(value: FormulaScalar) -> FormulaValue {
    match value {
        FormulaScalar::Number(value) => FormulaValue::Number(value),
        FormulaScalar::Text(value) => FormulaValue::Text(value),
        FormulaScalar::Blank => FormulaValue::Text(String::new()),
    }
}

fn formula_scalars_equal(left: &FormulaScalar, right: &FormulaScalar) -> bool {
    if let (Some(left), Some(right)) = (left.as_number(), right.as_number()) {
        return (left - right).abs() <= 1e-12;
    }
    match (left, right) {
        (FormulaScalar::Blank, FormulaScalar::Blank) => true,
        (FormulaScalar::Blank, FormulaScalar::Text(value))
        | (FormulaScalar::Text(value), FormulaScalar::Blank) => value.is_empty(),
        (FormulaScalar::Text(left), FormulaScalar::Text(right)) => left.eq_ignore_ascii_case(right),
        _ => false,
    }
}

fn formula_comparison_value(
    left: &FormulaValue,
    right: &FormulaValue,
    operator: &str,
) -> Option<FormulaValue> {
    match (left, right) {
        (
            FormulaValue::Range {
                values,
                rows,
                columns,
            },
            FormulaValue::Range {
                values: right_values,
                rows: right_rows,
                columns: right_columns,
            },
        ) if rows == right_rows && columns == right_columns => {
            let compared = values
                .iter()
                .zip(right_values)
                .map(|(left, right)| {
                    formula_values_compare(
                        &formula_value_from_scalar(left.clone()),
                        &formula_value_from_scalar(right.clone()),
                        operator,
                    )
                    .map(|value| FormulaScalar::Number(if value { 1.0 } else { 0.0 }))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(FormulaValue::Range {
                values: compared,
                rows: *rows,
                columns: *columns,
            })
        }
        (
            FormulaValue::Range {
                values,
                rows,
                columns,
            },
            scalar,
        ) => {
            let compared = values
                .iter()
                .map(|value| {
                    formula_values_compare(
                        &formula_value_from_scalar(value.clone()),
                        scalar,
                        operator,
                    )
                    .map(|value| FormulaScalar::Number(if value { 1.0 } else { 0.0 }))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(FormulaValue::Range {
                values: compared,
                rows: *rows,
                columns: *columns,
            })
        }
        (
            scalar,
            FormulaValue::Range {
                values,
                rows,
                columns,
            },
        ) => {
            let compared = values
                .iter()
                .map(|value| {
                    formula_values_compare(
                        scalar,
                        &formula_value_from_scalar(value.clone()),
                        operator,
                    )
                    .map(|value| FormulaScalar::Number(if value { 1.0 } else { 0.0 }))
                })
                .collect::<Option<Vec<_>>>()?;
            Some(FormulaValue::Range {
                values: compared,
                rows: *rows,
                columns: *columns,
            })
        }
        _ => formula_values_compare(left, right, operator)
            .map(|value| FormulaValue::Number(if value { 1.0 } else { 0.0 })),
    }
}

fn formula_values_compare(
    left: &FormulaValue,
    right: &FormulaValue,
    operator: &str,
) -> Option<bool> {
    if let (Some(left), Some(right)) = (left.as_number(), right.as_number()) {
        return Some(match operator {
            "<=" => left <= right,
            ">=" => left >= right,
            "<>" => (left - right).abs() > 1e-12,
            "=" => (left - right).abs() <= 1e-12,
            "<" => left < right,
            ">" => left > right,
            _ => return None,
        });
    }
    let scalar_text = |value: &FormulaValue| match value.scalar()? {
        FormulaScalar::Number(value) => Some(value.to_string()),
        FormulaScalar::Text(value) => Some(value),
        FormulaScalar::Blank => Some(String::new()),
    };
    let left = scalar_text(left)?.to_lowercase();
    let right = scalar_text(right)?.to_lowercase();
    let ordering = left.cmp(&right);
    Some(match operator {
        "<=" => ordering.is_le(),
        ">=" => ordering.is_ge(),
        "<>" => ordering.is_ne(),
        "=" => ordering.is_eq(),
        "<" => ordering.is_lt(),
        ">" => ordering.is_gt(),
        _ => return None,
    })
}

fn formula_criterion_matches(value: &FormulaScalar, criterion: &FormulaValue) -> bool {
    let criterion = match criterion {
        FormulaValue::Number(number) => {
            return value
                .as_number()
                .is_some_and(|value| (value - number).abs() <= 1e-12);
        }
        FormulaValue::Text(text) => text.as_str(),
        FormulaValue::Range { .. } => return false,
    };
    let (operator, operand) = ["<=", ">=", "<>", "=", "<", ">"]
        .into_iter()
        .find_map(|operator| {
            criterion
                .strip_prefix(operator)
                .map(|operand| (operator, operand))
        })
        .unwrap_or(("=", criterion));
    if let Ok(expected) = operand.parse::<f64>() {
        let Some(actual) = value.as_number() else {
            return operator == "<>";
        };
        return match operator {
            "<=" => actual <= expected,
            ">=" => actual >= expected,
            "<>" => (actual - expected).abs() > 1e-12,
            "=" => (actual - expected).abs() <= 1e-12,
            "<" => actual < expected,
            ">" => actual > expected,
            _ => false,
        };
    }
    let Some(actual) = value.as_text() else {
        return operator == "<>";
    };
    let comparison = actual.to_lowercase().cmp(&operand.to_lowercase());
    match operator {
        "<=" => comparison.is_le(),
        ">=" => comparison.is_ge(),
        "<>" => !excel_wildcard_matches(actual, operand),
        "=" => excel_wildcard_matches(actual, operand),
        "<" => comparison.is_lt(),
        ">" => comparison.is_gt(),
        _ => false,
    }
}

fn excel_wildcard_matches(value: &str, pattern: &str) -> bool {
    let value = value.to_lowercase().chars().collect::<Vec<_>>();
    #[derive(Clone, Copy)]
    enum Token {
        Literal(char),
        Any,
        Star,
    }
    let mut tokens = Vec::<Token>::new();
    let mut escaped = false;
    for character in pattern.to_lowercase().chars() {
        if escaped {
            tokens.push(Token::Literal(character));
            escaped = false;
        } else if character == '~' {
            escaped = true;
        } else if character == '*' {
            if !matches!(tokens.last(), Some(Token::Star)) {
                tokens.push(Token::Star);
            }
        } else if character == '?' {
            tokens.push(Token::Any);
        } else {
            tokens.push(Token::Literal(character));
        }
        if tokens.len() > 256 {
            return false;
        }
    }
    if escaped {
        tokens.push(Token::Literal('~'));
    }
    if value.len() > 32_768 {
        return false;
    }
    let mut value_index = 0usize;
    let mut token_index = 0usize;
    let mut last_star = None::<usize>;
    let mut retry_value_index = 0usize;
    while value_index < value.len() {
        match tokens.get(token_index) {
            Some(Token::Literal(character)) if *character == value[value_index] => {
                value_index += 1;
                token_index += 1;
            }
            Some(Token::Any) => {
                value_index += 1;
                token_index += 1;
            }
            Some(Token::Star) => {
                last_star = Some(token_index);
                token_index += 1;
                retry_value_index = value_index;
            }
            _ if last_star.is_some() => {
                retry_value_index += 1;
                value_index = retry_value_index;
                token_index = last_star.expect("checked above") + 1;
            }
            _ => return false,
        }
    }
    tokens[token_index..]
        .iter()
        .all(|token| matches!(token, Token::Star))
}

#[derive(Default)]
struct ConditionalRuleBuilder {
    rule_type: String,
    operator: String,
    text: String,
    differential_style_id: Option<usize>,
    priority: usize,
    stop_if_true: bool,
    formulas: Vec<String>,
    thresholds: Vec<ConditionalThreshold>,
    colors: Vec<String>,
    minimum_length: f64,
    maximum_length: f64,
}

fn parse_conditional_rules(xml: &[u8], max_events: usize) -> Result<Vec<ConditionalRule>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut ranges = Vec::<MergeRange>::new();
    let mut builder = None::<ConditionalRuleBuilder>;
    let mut formula = String::new();
    let mut capture_formula = false;
    let mut output = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "XLSX conditional-format event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if !stack.iter().any(|item| item == "extLst") {
                    conditional_rule_event(
                        &start,
                        &name,
                        &mut ranges,
                        &mut builder,
                        &mut formula,
                        &mut capture_formula,
                    );
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if !stack.iter().any(|item| item == "extLst") {
                    conditional_rule_event(
                        &start,
                        &name,
                        &mut ranges,
                        &mut builder,
                        &mut formula,
                        &mut capture_formula,
                    );
                }
            }
            Event::Text(text) if capture_formula && !stack.iter().any(|item| item == "extLst") => {
                formula.push_str(&decode_xlsx_text(&text, "conditional-format formula")?);
            }
            Event::GeneralRef(reference)
                if capture_formula && !stack.iter().any(|item| item == "extLst") =>
            {
                formula.push_str(&decode_xlsx_reference(
                    &reference,
                    "conditional-format formula",
                )?);
            }
            Event::End(end) => {
                if !stack.iter().any(|item| item == "extLst") {
                    match local_name(end.name().as_ref()) {
                        b"formula" => {
                            if let Some(builder) = builder.as_mut() {
                                builder.formulas.push(std::mem::take(&mut formula));
                            }
                            capture_formula = false;
                        }
                        b"cfRule" => {
                            if let Some(builder) = builder.take() {
                                output.push(finish_conditional_rule(builder, ranges.clone()));
                            }
                        }
                        b"conditionalFormatting" => ranges.clear(),
                        _ => {}
                    }
                }
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    output.sort_by_key(|rule| rule.priority);
    Ok(output)
}

fn parse_manual_page_breaks(xml: &[u8], max_events: usize) -> Result<(Vec<usize>, Vec<usize>)> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut row_breaks = Vec::new();
    let mut column_breaks = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "XLSX page-break event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == "brk" {
                    append_manual_page_break(&start, &stack, &mut row_breaks, &mut column_breaks);
                }
                stack.push(name);
            }
            Event::Empty(start) => {
                if local_name(start.name().as_ref()) == b"brk" {
                    append_manual_page_break(&start, &stack, &mut row_breaks, &mut column_breaks);
                }
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    row_breaks.sort_unstable();
    row_breaks.dedup();
    column_breaks.sort_unstable();
    column_breaks.dedup();
    Ok((row_breaks, column_breaks))
}

fn parse_print_setup(xml: &[u8], max_events: usize) -> Result<PrintSetup> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut setup = PrintSetup::default();
    let mut landscape = false;
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(
                "XLSX print-setup event limit exceeded".into(),
            ));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) | Event::Empty(start) => match local_name(start.name().as_ref()) {
                b"pageSetup" => {
                    setup.enabled = true;
                    let paper_size = parse_i64(attribute(&start, b"paperSize"), 1).max(1) as u32;
                    (setup.paper_width, setup.paper_height) = excel_paper_size(paper_size);
                    landscape = attribute(&start, b"orientation").as_deref() == Some("landscape");
                    setup.fit_to_width =
                        parse_i64(attribute(&start, b"fitToWidth"), 0).max(0) as usize;
                    setup.fit_to_height =
                        parse_i64(attribute(&start, b"fitToHeight"), 0).max(0) as usize;
                    setup.scale =
                        (parse_f64(attribute(&start, b"scale"), 100.0) / 100.0).clamp(0.1, 4.0);
                }
                b"pageMargins" => {
                    setup.margin_left = parse_f64(attribute(&start, b"left"), 0.7).max(0.0) * 72.0;
                    setup.margin_right =
                        parse_f64(attribute(&start, b"right"), 0.7).max(0.0) * 72.0;
                    setup.margin_top = parse_f64(attribute(&start, b"top"), 0.75).max(0.0) * 72.0;
                    setup.margin_bottom =
                        parse_f64(attribute(&start, b"bottom"), 0.75).max(0.0) * 72.0;
                }
                b"pageSetUpPr" => {
                    setup.fit_to_page = attribute(&start, b"fitToPage")
                        .is_some_and(|value| value == "1" || value == "true");
                }
                b"printOptions" => {
                    setup.center_horizontal = attribute(&start, b"horizontalCentered")
                        .is_some_and(|value| value == "1" || value == "true");
                    setup.center_vertical = attribute(&start, b"verticalCentered")
                        .is_some_and(|value| value == "1" || value == "true");
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if landscape {
        std::mem::swap(&mut setup.paper_width, &mut setup.paper_height);
    }
    Ok(setup)
}

fn excel_paper_size(paper_size: u32) -> (f64, f64) {
    match paper_size {
        5 => (612.0, 1_008.0),
        8 => (841.89, 1_190.55),
        9 => (595.28, 841.89),
        11 => (419.53, 595.28),
        13 => (612.0, 936.0),
        _ => (612.0, 792.0),
    }
}

fn append_manual_page_break(
    start: &quick_xml::events::BytesStart<'_>,
    stack: &[String],
    row_breaks: &mut Vec<usize>,
    column_breaks: &mut Vec<usize>,
) {
    if attribute(start, b"man").is_some_and(|value| value != "1" && value != "true") {
        return;
    }
    let id = parse_i64(attribute(start, b"id"), 0).max(0) as usize;
    if id == 0 {
        return;
    }
    if stack.iter().any(|item| item == "rowBreaks") {
        row_breaks.push(id);
    } else if stack.iter().any(|item| item == "colBreaks") {
        column_breaks.push(id);
    }
}

fn conditional_rule_event(
    start: &quick_xml::events::BytesStart<'_>,
    name: &str,
    ranges: &mut Vec<MergeRange>,
    builder: &mut Option<ConditionalRuleBuilder>,
    formula: &mut String,
    capture_formula: &mut bool,
) {
    match name {
        "conditionalFormatting" => {
            *ranges = attribute(start, b"sqref")
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(parse_merge_range)
                .collect();
        }
        "cfRule" => {
            *builder = Some(ConditionalRuleBuilder {
                rule_type: attribute(start, b"type").unwrap_or_default(),
                operator: attribute(start, b"operator").unwrap_or_default(),
                text: attribute(start, b"text").unwrap_or_default(),
                differential_style_id: attribute(start, b"dxfId")
                    .and_then(|value| value.parse().ok()),
                priority: parse_i64(attribute(start, b"priority"), i64::MAX).max(0) as usize,
                stop_if_true: attribute(start, b"stopIfTrue")
                    .is_some_and(|value| value == "1" || value == "true"),
                ..ConditionalRuleBuilder::default()
            });
        }
        "formula" if builder.is_some() => {
            formula.clear();
            *capture_formula = true;
        }
        "cfvo" => {
            if let Some(builder) = builder.as_mut() {
                builder.thresholds.push(ConditionalThreshold {
                    kind: attribute(start, b"type").unwrap_or_else(|| "num".into()),
                    value: attribute(start, b"val").and_then(|value| value.parse().ok()),
                });
            }
        }
        "dataBar" => {
            if let Some(builder) = builder.as_mut() {
                builder.minimum_length = parse_f64(attribute(start, b"minLength"), 10.0) / 100.0;
                builder.maximum_length = parse_f64(attribute(start, b"maxLength"), 90.0) / 100.0;
            }
        }
        "color" => {
            if let Some(builder) = builder.as_mut()
                && let Some(rgb) = attribute(start, b"rgb")
            {
                builder.colors.push(argb_to_rgb(&rgb));
            }
        }
        _ => {}
    }
}

fn finish_conditional_rule(
    builder: ConditionalRuleBuilder,
    ranges: Vec<MergeRange>,
) -> ConditionalRule {
    let kind = match builder.rule_type.as_str() {
        "cellIs" => ConditionalRuleKind::CellIs {
            operator: builder.operator,
            formulas: builder.formulas,
            differential_style_id: builder.differential_style_id,
        },
        "colorScale" if builder.colors.len() >= 2 => ConditionalRuleKind::ColorScale {
            thresholds: builder.thresholds,
            colors: builder.colors,
        },
        "dataBar" if !builder.colors.is_empty() => ConditionalRuleKind::DataBar {
            thresholds: builder.thresholds,
            color: builder.colors[0].clone(),
            minimum_length: builder.minimum_length.clamp(0.0, 1.0),
            maximum_length: builder.maximum_length.clamp(0.0, 1.0),
        },
        "containsBlanks" => ConditionalRuleKind::ContainsBlanks {
            invert: false,
            differential_style_id: builder.differential_style_id,
        },
        "notContainsBlanks" => ConditionalRuleKind::ContainsBlanks {
            invert: true,
            differential_style_id: builder.differential_style_id,
        },
        "containsText" | "notContainsText" => ConditionalRuleKind::ContainsText {
            text: builder.text,
            invert: builder.rule_type == "notContainsText",
            differential_style_id: builder.differential_style_id,
        },
        "expression" if !builder.formulas.is_empty() => ConditionalRuleKind::Expression {
            formula: builder.formulas[0].clone(),
            differential_style_id: builder.differential_style_id,
        },
        _ => ConditionalRuleKind::Unsupported(builder.rule_type),
    };
    ConditionalRule {
        ranges,
        priority: builder.priority,
        stop_if_true: builder.stop_if_true,
        kind,
    }
}

fn apply_row(start: &quick_xml::events::BytesStart<'_>, sheet: &mut Sheet) {
    let row = parse_i64(attribute(start, b"r"), 1).max(1) as usize;
    if let Some(height) = attribute(start, b"ht").and_then(|value| value.parse::<f64>().ok()) {
        sheet.row_heights.insert(row, height.max(0.0));
    }
    if attribute(start, b"hidden").is_some_and(|value| value == "1" || value == "true") {
        sheet.hidden_rows.insert(row);
    }
}

fn apply_column(start: &quick_xml::events::BytesStart<'_>, sheet: &mut Sheet) {
    let minimum = parse_i64(attribute(start, b"min"), 1).max(1) as usize;
    let maximum = parse_i64(attribute(start, b"max"), minimum as i64).max(minimum as i64) as usize;
    let width = attribute(start, b"width")
        .and_then(|value| value.parse::<f64>().ok())
        .map(column_width_to_points)
        .unwrap_or(DEFAULT_COLUMN_POINTS);
    let hidden = attribute(start, b"hidden").is_some_and(|value| value == "1" || value == "true");
    for column in minimum..=maximum.min(16_384) {
        sheet.column_widths.insert(column, width);
        if hidden {
            sheet.hidden_columns.insert(column);
        }
    }
}

fn resolve_cell_value(
    raw: &str,
    data_type: &str,
    style_id: usize,
    shared_strings: &[String],
    styles: &Styles,
) -> String {
    match data_type {
        "s" => raw
            .parse::<usize>()
            .ok()
            .and_then(|index| shared_strings.get(index))
            .cloned()
            .unwrap_or_else(|| raw.to_owned()),
        "b" => match raw {
            "1" => "TRUE".into(),
            "0" => "FALSE".into(),
            _ => raw.into(),
        },
        "e" => format!("#{raw}"),
        _ => styles.format_value(raw, styles.cell_style(style_id)),
    }
}

fn conditional_rule_bounds(
    rule: &ConditionalRule,
    cells: &BTreeMap<(usize, usize), Cell>,
) -> Option<(f64, f64)> {
    if !matches!(
        rule.kind,
        ConditionalRuleKind::ColorScale { .. } | ConditionalRuleKind::DataBar { .. }
    ) {
        return None;
    }
    let mut minimum = f64::INFINITY;
    let mut maximum = f64::NEG_INFINITY;
    for ((row, column), cell) in cells {
        if conditional_ranges_contain(&rule.ranges, *row, *column)
            && let Ok(value) = cell.raw_value.parse::<f64>()
        {
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
    }
    (minimum.is_finite() && maximum.is_finite()).then_some((minimum, maximum))
}

#[allow(clippy::too_many_arguments)]
fn applied_conditional_style(
    row: usize,
    column: usize,
    cell: &Cell,
    cells: &BTreeMap<(usize, usize), Cell>,
    rules: &[ConditionalRule],
    bounds: &[Option<(f64, f64)>],
    styles: &Styles,
    conditional_expression_results: &[Option<bool>],
) -> AppliedConditionalStyle {
    let mut applied = AppliedConditionalStyle::default();
    let numeric_value = cell.raw_value.parse::<f64>().ok();
    for (index, rule) in rules.iter().enumerate() {
        if !conditional_ranges_contain(&rule.ranges, row, column) {
            continue;
        }
        let matched = match &rule.kind {
            ConditionalRuleKind::CellIs {
                operator,
                formulas,
                differential_style_id,
            } => {
                let matched = numeric_value
                    .is_some_and(|value| evaluate_cell_is_rule(value, operator, formulas, cells));
                if matched {
                    apply_conditional_differential_style(
                        &mut applied,
                        *differential_style_id,
                        styles,
                    );
                }
                matched
            }
            ConditionalRuleKind::ColorScale { thresholds, colors } => {
                if let (Some(value), Some((minimum, maximum))) = (numeric_value, bounds[index]) {
                    applied.fill = Some(conditional_scale_color(
                        value, minimum, maximum, thresholds, colors,
                    ));
                    true
                } else {
                    false
                }
            }
            ConditionalRuleKind::DataBar {
                thresholds,
                color,
                minimum_length,
                maximum_length,
            } => {
                if let (Some(value), Some((minimum, maximum))) = (numeric_value, bounds[index]) {
                    let lower =
                        conditional_threshold_value(thresholds.first(), minimum, maximum, minimum);
                    let upper =
                        conditional_threshold_value(thresholds.get(1), minimum, maximum, maximum);
                    let denominator = (upper - lower).abs();
                    let value_position = if denominator <= 1e-15 {
                        1.0
                    } else {
                        ((value - lower) / (upper - lower)).clamp(0.0, 1.0)
                    };
                    let (zero_position, value_position) = if lower < 0.0 && upper > 0.0 {
                        ((-lower / (upper - lower)).clamp(0.0, 1.0), value_position)
                    } else if upper <= 0.0 {
                        (
                            1.0,
                            1.0 - (minimum_length
                                + (1.0 - value_position) * (maximum_length - minimum_length)),
                        )
                    } else {
                        (
                            0.0,
                            minimum_length + value_position * (maximum_length - minimum_length),
                        )
                    };
                    applied.data_bar = Some((zero_position, value_position, color.clone()));
                    true
                } else {
                    false
                }
            }
            ConditionalRuleKind::ContainsBlanks {
                invert,
                differential_style_id,
            } => {
                let blank = cell.value.trim().is_empty();
                let matched = if *invert { !blank } else { blank };
                if matched {
                    apply_conditional_differential_style(
                        &mut applied,
                        *differential_style_id,
                        styles,
                    );
                }
                matched
            }
            ConditionalRuleKind::ContainsText {
                text,
                invert,
                differential_style_id,
            } => {
                let contains = cell.value.to_lowercase().contains(&text.to_lowercase());
                let matched = if *invert { !contains } else { contains };
                if matched {
                    apply_conditional_differential_style(
                        &mut applied,
                        *differential_style_id,
                        styles,
                    );
                }
                matched
            }
            ConditionalRuleKind::Expression {
                differential_style_id,
                ..
            } => {
                let matched = conditional_expression_results
                    .get(index)
                    .copied()
                    .flatten()
                    .unwrap_or(false);
                if matched {
                    apply_conditional_differential_style(
                        &mut applied,
                        *differential_style_id,
                        styles,
                    );
                }
                matched
            }
            ConditionalRuleKind::Unsupported(_) => false,
        };
        if matched && rule.stop_if_true {
            break;
        }
    }
    applied
}

fn apply_conditional_differential_style(
    applied: &mut AppliedConditionalStyle,
    differential_style_id: Option<usize>,
    styles: &Styles,
) {
    let Some(style) =
        differential_style_id.and_then(|style_id| styles.differential_styles.get(style_id))
    else {
        return;
    };
    if style.fill.is_some() {
        applied.fill.clone_from(&style.fill);
    }
    if style.font_color.is_some() {
        applied.font_color.clone_from(&style.font_color);
    }
}

fn conditional_ranges_contain(ranges: &[MergeRange], row: usize, column: usize) -> bool {
    ranges.iter().any(|range| {
        row >= range.start_row
            && row <= range.end_row
            && column >= range.start_column
            && column <= range.end_column
    })
}

fn evaluate_cell_is_rule(
    value: f64,
    operator: &str,
    formulas: &[String],
    cells: &BTreeMap<(usize, usize), Cell>,
) -> bool {
    let first = formulas
        .first()
        .and_then(|formula| conditional_formula_value(formula, cells));
    let second = formulas
        .get(1)
        .and_then(|formula| conditional_formula_value(formula, cells));
    match (operator, first, second) {
        ("equal", Some(first), _) => (value - first).abs() <= 1e-12,
        ("notEqual", Some(first), _) => (value - first).abs() > 1e-12,
        ("lessThan", Some(first), _) => value < first,
        ("lessThanOrEqual", Some(first), _) => value <= first,
        ("greaterThan", Some(first), _) => value > first,
        ("greaterThanOrEqual", Some(first), _) => value >= first,
        ("between", Some(first), Some(second)) => {
            value >= first.min(second) && value <= first.max(second)
        }
        ("notBetween", Some(first), Some(second)) => {
            value < first.min(second) || value > first.max(second)
        }
        _ => false,
    }
}

fn conditional_formula_value(formula: &str, cells: &BTreeMap<(usize, usize), Cell>) -> Option<f64> {
    let formula = formula.trim().trim_start_matches('=');
    if let Ok(value) = formula.parse::<f64>() {
        return Some(value);
    }
    let reference = formula.rsplit('!').next().unwrap_or(formula);
    let position = parse_cell_reference(reference.trim_matches('\''))?;
    cells.get(&position)?.raw_value.parse().ok()
}

fn conditional_threshold_value(
    threshold: Option<&ConditionalThreshold>,
    minimum: f64,
    maximum: f64,
    fallback: f64,
) -> f64 {
    let Some(threshold) = threshold else {
        return fallback;
    };
    match threshold.kind.as_str() {
        "min" | "autoMin" => minimum,
        "max" | "autoMax" => maximum,
        "percent" | "percentile" => {
            minimum + (maximum - minimum) * threshold.value.unwrap_or(0.0) / 100.0
        }
        _ => threshold.value.unwrap_or(fallback),
    }
}

fn conditional_scale_color(
    value: f64,
    minimum: f64,
    maximum: f64,
    thresholds: &[ConditionalThreshold],
    colors: &[String],
) -> String {
    let first = conditional_threshold_value(thresholds.first(), minimum, maximum, minimum);
    let last = conditional_threshold_value(
        thresholds.get(colors.len().saturating_sub(1)),
        minimum,
        maximum,
        maximum,
    );
    if colors.len() >= 3 {
        let middle =
            conditional_threshold_value(thresholds.get(1), minimum, maximum, (first + last) * 0.5);
        if value <= middle {
            return interpolate_hex_color(
                &colors[0],
                &colors[1],
                normalized_between(value, first, middle),
            );
        }
        return interpolate_hex_color(
            &colors[1],
            &colors[2],
            normalized_between(value, middle, last),
        );
    }
    interpolate_hex_color(
        &colors[0],
        &colors[1],
        normalized_between(value, first, last),
    )
}

fn normalized_between(value: f64, first: f64, second: f64) -> f64 {
    if (second - first).abs() <= 1e-15 {
        0.0
    } else {
        ((value - first) / (second - first)).clamp(0.0, 1.0)
    }
}

fn interpolate_hex_color(first: &str, second: &str, ratio: f64) -> String {
    let parse = |color: &str| {
        let color = color.trim_start_matches('#');
        (color.len() == 6).then(|| {
            [
                u8::from_str_radix(&color[0..2], 16).unwrap_or(0),
                u8::from_str_radix(&color[2..4], 16).unwrap_or(0),
                u8::from_str_radix(&color[4..6], 16).unwrap_or(0),
            ]
        })
    };
    let first = parse(first).unwrap_or([0, 0, 0]);
    let second = parse(second).unwrap_or(first);
    let ratio = ratio.clamp(0.0, 1.0);
    let mixed: [u8; 3] = std::array::from_fn(|index| {
        (f64::from(first[index]) + (f64::from(second[index]) - f64::from(first[index])) * ratio)
            .round() as u8
    });
    format!("#{:02X}{:02X}{:02X}", mixed[0], mixed[1], mixed[2])
}

impl Sheet {
    fn column_width(&self, column: usize) -> f64 {
        if self.hidden_columns.contains(&column) {
            0.0
        } else {
            self.column_widths
                .get(&column)
                .copied()
                .unwrap_or(DEFAULT_COLUMN_POINTS)
        }
    }

    fn row_height(&self, row: usize) -> f64 {
        if self.hidden_rows.contains(&row) {
            0.0
        } else {
            self.row_heights
                .get(&row)
                .copied()
                .unwrap_or(DEFAULT_ROW_POINTS)
        }
    }

    fn build_grid(&self, drawing_objects: &[DrawingObject]) -> SheetGrid {
        let drawing_max_column = drawing_objects
            .iter()
            .map(|object| {
                let (from, to, _, _) = drawing_object_geometry(object);
                to.map_or(from.column + 1, |marker| marker.column + 1)
            })
            .max()
            .unwrap_or(1);
        let drawing_max_row = drawing_objects
            .iter()
            .map(|object| {
                let (from, to, _, _) = drawing_object_geometry(object);
                to.map_or(from.row + 1, |marker| marker.row + 1)
            })
            .max()
            .unwrap_or(1);
        let print_max_column = self
            .print_areas
            .iter()
            .map(|area| area.end_column)
            .max()
            .unwrap_or(1);
        let print_max_row = self
            .print_areas
            .iter()
            .map(|area| area.end_row)
            .max()
            .unwrap_or(1);
        let max_column = self
            .max_column
            .max(drawing_max_column)
            .max(print_max_column)
            .max(self.print_titles.columns.map_or(1, |(_, end)| end));
        let max_row = self
            .max_row
            .max(drawing_max_row)
            .max(print_max_row)
            .max(self.print_titles.rows.map_or(1, |(_, end)| end));
        let mut column_positions = vec![0.0; max_column + 2];
        for column in 1..=max_column {
            column_positions[column + 1] = column_positions[column] + self.column_width(column);
        }
        let mut row_positions = vec![0.0; max_row + 2];
        for row in 1..=max_row {
            row_positions[row + 1] = row_positions[row] + self.row_height(row);
        }
        SheetGrid {
            column_positions,
            row_positions,
        }
    }

    fn render_regions(&self, drawing_objects: &[DrawingObject]) -> Vec<MergeRange> {
        let drawing_max_column = drawing_objects
            .iter()
            .map(|object| {
                let (from, to, _, _) = drawing_object_geometry(object);
                to.map_or(from.column + 1, |marker| marker.column + 1)
            })
            .max()
            .unwrap_or(1);
        let drawing_max_row = drawing_objects
            .iter()
            .map(|object| {
                let (from, to, _, _) = drawing_object_geometry(object);
                to.map_or(from.row + 1, |marker| marker.row + 1)
            })
            .max()
            .unwrap_or(1);
        let full = MergeRange {
            start_row: 1,
            start_column: 1,
            end_row: self.max_row.max(drawing_max_row),
            end_column: self.max_column.max(drawing_max_column),
        };
        let areas = if self.print_areas.is_empty() {
            vec![full]
        } else {
            self.print_areas.clone()
        };
        let auto_tile = self.print_areas.is_empty() && !self.print_setup.enabled;
        let mut regions = Vec::new();
        for area in areas {
            let base_row_segments =
                split_print_axis(area.start_row, area.end_row, &self.row_breaks);
            let base_column_segments =
                split_print_axis(area.start_column, area.end_column, &self.column_breaks);
            let column_segments = if auto_tile {
                base_column_segments
                    .into_iter()
                    .flat_map(|(start, end)| {
                        split_axis_by_limits(
                            start,
                            end,
                            AUTO_TILE_POINTS,
                            AUTO_TILE_CELLS,
                            |index| self.column_width(index),
                        )
                    })
                    .collect::<Vec<_>>()
            } else {
                base_column_segments
            };
            for (start_column, end_column) in column_segments {
                let column_count = end_column - start_column + 1;
                let maximum_rows = (AUTO_TILE_CELLS / column_count).max(1);
                for (base_start_row, base_end_row) in &base_row_segments {
                    let row_segments = if auto_tile {
                        split_axis_by_limits(
                            *base_start_row,
                            *base_end_row,
                            AUTO_TILE_POINTS,
                            maximum_rows,
                            |index| self.row_height(index),
                        )
                    } else {
                        vec![(*base_start_row, *base_end_row)]
                    };
                    for (start_row, end_row) in row_segments {
                        regions.push(MergeRange {
                            start_row,
                            start_column,
                            end_row,
                            end_column,
                        });
                    }
                }
            }
        }
        if regions.is_empty() {
            regions.push(full);
        }
        regions
    }

    #[allow(clippy::too_many_arguments)]
    fn render_region(
        &self,
        page_number: usize,
        styles: &Styles,
        drawing_objects: &[DrawingObject],
        grid: &SheetGrid,
        conditional_expression_results: &[Option<bool>],
        region: MergeRange,
        region_index: usize,
        region_count: usize,
    ) -> Result<Page> {
        let region_cells = (region.end_row - region.start_row + 1)
            .checked_mul(region.end_column - region.start_column + 1)
            .ok_or_else(|| Error::LimitExceeded("XLSX print region size overflow".into()))?;
        if region_cells > MAX_RENDERED_CELLS {
            return Err(Error::LimitExceeded(format!(
                "worksheet {} print region {} spans {region_cells} cells; maximum is {MAX_RENDERED_CELLS}",
                self.name,
                cell_range_reference(region)
            )));
        }
        let column_positions = &grid.column_positions;
        let row_positions = &grid.row_positions;
        let origin_x = column_positions[region.start_column];
        let origin_y = row_positions[region.start_row];
        let repeated_columns = self
            .print_titles
            .columns
            .filter(|(_, end)| region.start_column > *end);
        let repeated_rows = self
            .print_titles
            .rows
            .filter(|(_, end)| region.start_row > *end);
        let title_origin_x = repeated_columns
            .map(|(start, _)| column_positions[start])
            .unwrap_or(0.0);
        let title_origin_y = repeated_rows
            .map(|(start, _)| row_positions[start])
            .unwrap_or(0.0);
        let title_width = repeated_columns
            .map(|(start, end)| column_positions[end + 1] - column_positions[start])
            .unwrap_or(0.0);
        let title_height = repeated_rows
            .map(|(start, end)| row_positions[end + 1] - row_positions[start])
            .unwrap_or(0.0);
        let body_width = (column_positions[region.end_column + 1] - origin_x).max(1.0);
        let body_height = (row_positions[region.end_row + 1] - origin_y).max(1.0);
        let width = body_width + title_width;
        let height = body_height + title_height;
        if width > MAX_CANVAS_POINTS || height > MAX_CANVAS_POINTS {
            return Err(Error::LimitExceeded(format!(
                "worksheet {} canvas is {width:.0}x{height:.0} points",
                self.name
            )));
        }
        let (page_width, page_height, content_scale, offset_x, offset_y) =
            if self.print_setup.enabled {
                let printable_width = (self.print_setup.paper_width
                    - self.print_setup.margin_left
                    - self.print_setup.margin_right)
                    .max(1.0);
                let printable_height = (self.print_setup.paper_height
                    - self.print_setup.margin_top
                    - self.print_setup.margin_bottom)
                    .max(1.0);
                let mut scale = self.print_setup.scale;
                if self.print_setup.fit_to_page
                    || self.print_setup.fit_to_width > 0
                    || self.print_setup.fit_to_height > 0
                {
                    let mut candidates = Vec::new();
                    if self.print_setup.fit_to_width > 0 {
                        candidates.push(printable_width / width);
                    }
                    if self.print_setup.fit_to_height > 0 {
                        candidates.push(printable_height / height);
                    }
                    if let Some(fitted) = candidates.into_iter().reduce(f64::min) {
                        scale = fitted.clamp(0.1, 4.0);
                    }
                }
                let scaled_width = width * scale;
                let scaled_height = height * scale;
                let offset_x = self.print_setup.margin_left
                    + if self.print_setup.center_horizontal {
                        (printable_width - scaled_width).max(0.0) / 2.0
                    } else {
                        0.0
                    };
                let offset_y = self.print_setup.margin_top
                    + if self.print_setup.center_vertical {
                        (printable_height - scaled_height).max(0.0) / 2.0
                    } else {
                        0.0
                    };
                (
                    self.print_setup.paper_width,
                    self.print_setup.paper_height,
                    scale,
                    offset_x,
                    offset_y,
                )
            } else {
                (width, height, 1.0, 0.0, 0.0)
            };
        let mut page = Page::new(page_number, page_width, page_height, "xlsx");
        page.title = if region_count > 1 {
            format!("{} ({region_index}/{region_count})", self.name)
        } else {
            self.name.clone()
        };
        page.description = format!(
            "Worksheet {} region {} rendered as an SVG grid",
            self.name,
            cell_range_reference(region)
        );
        page.warnings.clone_from(&self.warnings);
        for rule in &self.conditional_rules {
            if let ConditionalRuleKind::Unsupported(rule_type) = &rule.kind {
                page.warn(format!(
                    "XLSX conditional-format rule {rule_type} is not yet rendered"
                ));
            }
        }
        let conditional_bounds = self
            .conditional_rules
            .iter()
            .map(|rule| conditional_rule_bounds(rule, &self.cells))
            .collect::<Vec<_>>();
        let hidden_by_merge = merged_hidden_cells(&self.merges);
        for ((row, column), cell) in &self.cells {
            let row_in_body = *row >= region.start_row && *row <= region.end_row;
            let column_in_body = *column >= region.start_column && *column <= region.end_column;
            let row_in_title =
                repeated_rows.is_some_and(|(start, end)| *row >= start && *row <= end);
            let column_in_title =
                repeated_columns.is_some_and(|(start, end)| *column >= start && *column <= end);
            if !(row_in_body || row_in_title) || !(column_in_body || column_in_title) {
                continue;
            }
            let shift_x = if column_in_title {
                -title_origin_x
            } else {
                title_width - origin_x
            };
            let shift_y = if row_in_title {
                -title_origin_y
            } else {
                title_height - origin_y
            };
            let node_start = page.nodes.len();
            if hidden_by_merge.contains(&(*row, *column)) {
                continue;
            }
            let merge = self
                .merges
                .iter()
                .find(|range| range.start_row == *row && range.start_column == *column);
            let end_row = merge.map_or(*row, |range| range.end_row);
            let end_column = merge.map_or(*column, |range| range.end_column);
            let x = column_positions[*column];
            let y = row_positions[*row];
            let cell_width = column_positions[end_column + 1] - x;
            let cell_height = row_positions[end_row + 1] - y;
            if cell_width <= 0.0 || cell_height <= 0.0 {
                continue;
            }
            let style = styles.cell_style(cell.style_id);
            let conditional = applied_conditional_style(
                *row,
                *column,
                cell,
                &self.cells,
                &self.conditional_rules,
                &conditional_bounds,
                styles,
                conditional_expression_results,
            );
            let cell_fill = conditional
                .fill
                .as_deref()
                .or_else(|| styles.fill(style.fill_id));
            if let Some(fill) = cell_fill {
                page.nodes.push(rect_node(
                    format!("cell-fill-{row}-{column}"),
                    x,
                    y,
                    cell_width,
                    cell_height,
                    Paint::solid(fill),
                    Stroke::default(),
                    "cell",
                ));
            }
            if let Some((start, end, color)) = &conditional.data_bar {
                let start_x = x + 1.0 + (cell_width - 2.0) * start.clamp(0.0, 1.0);
                let end_x = x + 1.0 + (cell_width - 2.0) * end.clamp(0.0, 1.0);
                if (end_x - start_x).abs() > 0.1 {
                    page.nodes.push(rect_node(
                        format!("cell-data-bar-{row}-{column}"),
                        start_x.min(end_x),
                        y + 2.0,
                        (end_x - start_x).abs(),
                        (cell_height - 4.0).max(1.0),
                        Paint::Solid {
                            color: color.clone(),
                            opacity: 0.65,
                        },
                        Stroke::default(),
                        "conditional-data-bar",
                    ));
                }
            }
            append_borders(
                &mut page,
                styles.border(style.border_id),
                *row,
                *column,
                x,
                y,
                cell_width,
                cell_height,
            );
            if !cell.value.is_empty() {
                let font = styles.font(style.font_id);
                let anchor = if style.horizontal == TextAnchor::Start
                    && cell.raw_value.parse::<f64>().is_ok()
                {
                    TextAnchor::End
                } else {
                    style.horizontal
                };
                let text_x = match anchor {
                    TextAnchor::Start => x + 3.0,
                    TextAnchor::Middle => x + cell_width / 2.0,
                    TextAnchor::End => x + cell_width - 3.0,
                };
                let text_y = y + (cell_height + font.size * 0.72) / 2.0;
                let clip_id = format!("cell-clip-{row}-{column}");
                page.clips.push(crate::ir::ClipPath {
                    id: clip_id.clone(),
                    d: format!(
                        "M {} {} H {} V {} H {} Z",
                        fmt(x),
                        fmt(y),
                        fmt(x + cell_width),
                        fmt(y + cell_height),
                        fmt(x)
                    ),
                    transform: IDENTITY,
                    fill_rule: "nonzero".into(),
                    parent_id: None,
                    additional_paths: Vec::new(),
                });
                page.nodes.push(Node::Text {
                    id: format!("cell-text-{row}-{column}"),
                    x: text_x,
                    y: text_y,
                    runs: vec![TextRun {
                        text: cell.value.clone(),
                        font_family: font.family.clone(),
                        font_size: font.size,
                        bold: font.bold,
                        italic: font.italic,
                        fill: Paint::solid(
                            conditional.font_color.as_deref().unwrap_or(&font.color),
                        ),
                        baseline_shift: 0.0,
                        glyph_x_offsets: Vec::new(),
                        target_advance: None,
                    }],
                    anchor,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: Some(clip_id),
                    meta: SourceMeta {
                        kind: "cell-text".into(),
                        source_id: format!("{}!{}", self.name, cell_reference(*row, *column)),
                        semantic_role: "cell".into(),
                        alt_text: String::new(),
                        ..SourceMeta::default()
                    },
                });
            }
            if (shift_x.abs() > 1e-12 || shift_y.abs() > 1e-12) && page.nodes.len() > node_start {
                let nodes = page.nodes.split_off(node_start);
                page.nodes.push(Node::Group {
                    id: format!("cell-region-{page_number}-{row}-{column}"),
                    nodes,
                    transform: [1.0, 0.0, 0.0, 1.0, shift_x, shift_y],
                    opacity: 1.0,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "worksheet-cell-region".into(),
                        source_id: format!("{}!{}", self.name, cell_reference(*row, *column)),
                        ..SourceMeta::default()
                    },
                });
            }
        }
        let grid_color = "#D9D9D9";
        let mut grid_x = Vec::new();
        if let Some((start, end)) = repeated_columns {
            grid_x.extend(
                column_positions[start..=end + 1]
                    .iter()
                    .map(|position| position - title_origin_x),
            );
        }
        grid_x.extend(
            column_positions[region.start_column..=region.end_column + 1]
                .iter()
                .map(|position| title_width + position - origin_x),
        );
        grid_x.sort_by(f64::total_cmp);
        grid_x.dedup_by(|left, right| (*left - *right).abs() < 1e-9);

        let mut grid_y = Vec::new();
        if let Some((start, end)) = repeated_rows {
            grid_y.extend(
                row_positions[start..=end + 1]
                    .iter()
                    .map(|position| position - title_origin_y),
            );
        }
        grid_y.extend(
            row_positions[region.start_row..=region.end_row + 1]
                .iter()
                .map(|position| title_height + position - origin_y),
        );
        grid_y.sort_by(f64::total_cmp);
        grid_y.dedup_by(|top, bottom| (*top - *bottom).abs() < 1e-9);

        let mut vertical = String::new();
        for position in grid_x {
            vertical.push_str(&format!(
                "M {} {} V {} ",
                fmt(position),
                fmt(0.0),
                fmt(height)
            ));
        }
        let mut horizontal = String::new();
        for position in grid_y {
            horizontal.push_str(&format!(
                "M {} {} H {} ",
                fmt(0.0),
                fmt(position),
                fmt(width)
            ));
        }
        let grid_stroke = Stroke {
            paint: Paint::solid(grid_color),
            width: 0.5,
            miter_limit: 10.0,
            ..Stroke::default()
        };
        page.nodes.insert(
            0,
            Node::Path {
                id: format!("sheet-grid-{page_number}"),
                d: format!("{vertical}{horizontal}"),
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke: grid_stroke,
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "grid".into(),
                    source_id: self.name.clone(),
                    ..SourceMeta::default()
                },
            },
        );
        for (index, object) in drawing_objects.iter().enumerate() {
            let (from, _, _, _) = drawing_object_geometry(object);
            let (x, y) = drawing_object_position(object)
                .unwrap_or_else(|| drawing_marker_position(from, column_positions, row_positions));
            let (object_width, object_height) =
                drawing_object_size(object, column_positions, row_positions);
            let drawing_row = from.row + 1;
            let drawing_column = from.column + 1;
            let row_in_body =
                y + object_height >= origin_y && y <= row_positions[region.end_row + 1];
            let column_in_body =
                x + object_width >= origin_x && x <= column_positions[region.end_column + 1];
            let row_in_title = repeated_rows
                .is_some_and(|(start, end)| drawing_row >= start && drawing_row <= end);
            let column_in_title = repeated_columns
                .is_some_and(|(start, end)| drawing_column >= start && drawing_column <= end);
            if !(row_in_body || row_in_title) || !(column_in_body || column_in_title) {
                continue;
            }
            let shift_x = if column_in_title {
                -title_origin_x
            } else {
                title_width - origin_x
            };
            let shift_y = if row_in_title {
                -title_origin_y
            } else {
                title_height - origin_y
            };
            let is_line = matches!(object, DrawingObject::Shape(shape) if shape.preset == "line");
            let invalid_extent = if is_line {
                object_width <= 0.0 && object_height <= 0.0
            } else {
                object_width <= 0.0 || object_height <= 0.0
            };
            if invalid_extent {
                page.warn(format!(
                    "drawing object {} has no positive anchor extent",
                    drawing_object_name(object)
                ));
                continue;
            }
            let node_start = page.nodes.len();
            match object {
                DrawingObject::Image(image) => page.nodes.push(Node::Image {
                    id: format!("drawing-image-{}-{}", page_number, index + 1),
                    href: image.href.clone(),
                    x,
                    y,
                    width: object_width,
                    height: object_height,
                    transform: IDENTITY,
                    opacity: 1.0,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "drawing-image".into(),
                        source_id: image.name.clone(),
                        alt_text: image.alt_text.clone(),
                        ..SourceMeta::default()
                    },
                }),
                DrawingObject::Shape(shape) => {
                    page.nodes.push(Node::Path {
                        id: format!("drawing-shape-{}-{}", page_number, index + 1),
                        d: drawing_shape_path(&shape.preset, x, y, object_width, object_height),
                        fill_rule: "nonzero".into(),
                        fill: shape.fill.clone(),
                        stroke: shape.stroke.clone(),
                        transform: IDENTITY,
                        clip_id: None,
                        meta: SourceMeta {
                            kind: "drawing-shape".into(),
                            source_id: shape.name.clone(),
                            alt_text: shape.alt_text.clone(),
                            ..SourceMeta::default()
                        },
                    });
                    for (line_index, line) in shape.text.lines().enumerate() {
                        page.nodes.push(Node::Text {
                            id: format!(
                                "drawing-text-{}-{}-{}",
                                page_number,
                                index + 1,
                                line_index + 1
                            ),
                            x: x + 3.0,
                            y: y + shape.font_size * (line_index as f64 + 1.0),
                            runs: vec![TextRun {
                                text: line.into(),
                                font_family: shape.font_family.clone(),
                                font_size: shape.font_size,
                                bold: false,
                                italic: false,
                                fill: shape.text_fill.clone(),
                                baseline_shift: 0.0,
                                glyph_x_offsets: Vec::new(),
                                target_advance: None,
                            }],
                            anchor: TextAnchor::Start,
                            transform: IDENTITY,
                            opacity: 1.0,
                            stroke: Stroke::default(),
                            clip_id: None,
                            meta: SourceMeta {
                                kind: "drawing-text".into(),
                                source_id: shape.name.clone(),
                                ..SourceMeta::default()
                            },
                        });
                    }
                }
                DrawingObject::Chart(chart) => {
                    page.nodes.extend(render_chart(
                        &chart.data,
                        x,
                        y,
                        object_width,
                        object_height,
                        &format!("drawing-chart-{}-{}", page_number, index + 1),
                    ));
                }
            }
            if (shift_x.abs() > 1e-12 || shift_y.abs() > 1e-12) && page.nodes.len() > node_start {
                let nodes = page.nodes.split_off(node_start);
                page.nodes.push(Node::Group {
                    id: format!("drawing-region-{page_number}-{}", index + 1),
                    nodes,
                    transform: [1.0, 0.0, 0.0, 1.0, shift_x, shift_y],
                    opacity: 1.0,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "worksheet-drawing-region".into(),
                        source_id: drawing_object_name(object).into(),
                        ..SourceMeta::default()
                    },
                });
            }
        }
        if ((content_scale - 1.0).abs() > 1e-12 || offset_x.abs() > 1e-12 || offset_y.abs() > 1e-12)
            && !page.nodes.is_empty()
        {
            page.nodes = vec![Node::Group {
                id: format!("sheet-region-{page_number}"),
                nodes: std::mem::take(&mut page.nodes),
                transform: [content_scale, 0.0, 0.0, content_scale, offset_x, offset_y],
                opacity: 1.0,
                clip_id: None,
                meta: SourceMeta {
                    kind: "worksheet-region".into(),
                    source_id: format!("{}!{}", self.name, cell_range_reference(region)),
                    ..SourceMeta::default()
                },
            }];
        }
        Ok(page)
    }
}

fn drawing_marker_position(
    marker: &DrawingMarker,
    column_positions: &[f64],
    row_positions: &[f64],
) -> (f64, f64) {
    let column_index = (marker.column + 1).min(column_positions.len().saturating_sub(1));
    let row_index = (marker.row + 1).min(row_positions.len().saturating_sub(1));
    (
        column_positions[column_index] + marker.column_offset,
        row_positions[row_index] + marker.row_offset,
    )
}

fn drawing_object_geometry(
    object: &DrawingObject,
) -> (&DrawingMarker, Option<&DrawingMarker>, f64, f64) {
    match object {
        DrawingObject::Image(image) => (&image.from, image.to.as_ref(), image.width, image.height),
        DrawingObject::Shape(shape) => (&shape.from, shape.to.as_ref(), shape.width, shape.height),
        DrawingObject::Chart(chart) => (&chart.from, chart.to.as_ref(), chart.width, chart.height),
    }
}

fn drawing_object_position(object: &DrawingObject) -> Option<(f64, f64)> {
    match object {
        DrawingObject::Image(image) => image.position,
        DrawingObject::Shape(shape) => shape.position,
        DrawingObject::Chart(chart) => chart.position,
    }
}

fn drawing_object_size(
    object: &DrawingObject,
    column_positions: &[f64],
    row_positions: &[f64],
) -> (f64, f64) {
    let (from, to, width, height) = drawing_object_geometry(object);
    if let Some(to) = to {
        let (x, y) = drawing_marker_position(from, column_positions, row_positions);
        let (to_x, to_y) = drawing_marker_position(to, column_positions, row_positions);
        let anchor_width = (to_x - x).max(0.0);
        let anchor_height = (to_y - y).max(0.0);
        (
            if anchor_width > 0.0 {
                anchor_width
            } else {
                width.max(0.0)
            },
            if anchor_height > 0.0 {
                anchor_height
            } else {
                height.max(0.0)
            },
        )
    } else {
        (width.max(0.0), height.max(0.0))
    }
}

fn drawing_object_name(object: &DrawingObject) -> &str {
    match object {
        DrawingObject::Image(image) => &image.name,
        DrawingObject::Shape(shape) => &shape.name,
        DrawingObject::Chart(chart) => &chart.name,
    }
}

fn drawing_shape_path(preset: &str, x: f64, y: f64, width: f64, height: f64) -> String {
    match preset {
        "line" => format!(
            "M {} {} L {} {}",
            fmt(x),
            fmt(y),
            fmt(x + width),
            fmt(y + height)
        ),
        "rightBrace" => format!(
            "M {} {} C {} {} {} {} {} {} C {} {} {} {} {} {}",
            fmt(x),
            fmt(y),
            fmt(x + width * 0.7),
            fmt(y),
            fmt(x + width * 0.7),
            fmt(y + height * 0.35),
            fmt(x + width),
            fmt(y + height / 2.0),
            fmt(x + width * 0.7),
            fmt(y + height * 0.65),
            fmt(x + width * 0.7),
            fmt(y + height),
            fmt(x),
            fmt(y + height)
        ),
        "leftBrace" => format!(
            "M {} {} C {} {} {} {} {} {} C {} {} {} {} {} {}",
            fmt(x + width),
            fmt(y),
            fmt(x + width * 0.3),
            fmt(y),
            fmt(x + width * 0.3),
            fmt(y + height * 0.35),
            fmt(x),
            fmt(y + height / 2.0),
            fmt(x + width * 0.3),
            fmt(y + height * 0.65),
            fmt(x + width * 0.3),
            fmt(y + height),
            fmt(x + width),
            fmt(y + height)
        ),
        "ellipse" => format!(
            "M {} {} A {} {} 0 1 0 {} {} A {} {} 0 1 0 {} {} Z",
            fmt(x),
            fmt(y + height / 2.0),
            fmt(width / 2.0),
            fmt(height / 2.0),
            fmt(x + width),
            fmt(y + height / 2.0),
            fmt(width / 2.0),
            fmt(height / 2.0),
            fmt(x),
            fmt(y + height / 2.0)
        ),
        "rightArrow" => format!(
            "M {} {} H {} V {} L {} {} L {} {} V {} H {} Z",
            fmt(x),
            fmt(y + height * 0.25),
            fmt(x + width * 0.6),
            fmt(y),
            fmt(x + width),
            fmt(y + height * 0.5),
            fmt(x + width * 0.6),
            fmt(y + height),
            fmt(y + height * 0.75),
            fmt(x),
        ),
        _ => format!(
            "M {} {} H {} V {} H {} Z",
            fmt(x),
            fmt(y),
            fmt(x + width),
            fmt(y + height),
            fmt(x)
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn append_borders(
    page: &mut Page,
    border: &BorderStyle,
    row: usize,
    column: usize,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) {
    let sides = [
        (
            &border.left,
            format!("M {} {} V {}", fmt(x), fmt(y), fmt(y + height)),
        ),
        (
            &border.right,
            format!("M {} {} V {}", fmt(x + width), fmt(y), fmt(y + height)),
        ),
        (
            &border.top,
            format!("M {} {} H {}", fmt(x), fmt(y), fmt(x + width)),
        ),
        (
            &border.bottom,
            format!("M {} {} H {}", fmt(x), fmt(y + height), fmt(x + width)),
        ),
    ];
    for (index, (side, d)) in sides.into_iter().enumerate() {
        if let Some((color, line_width)) = side {
            page.nodes.push(Node::Path {
                id: format!("cell-border-{row}-{column}-{index}"),
                d,
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke: Stroke {
                    paint: Paint::solid(color),
                    width: *line_width,
                    miter_limit: 10.0,
                    ..Stroke::default()
                },
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta {
                    kind: "cell-border".into(),
                    source_id: cell_reference(row, column),
                    ..SourceMeta::default()
                },
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn rect_node(
    id: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    fill: Paint,
    stroke: Stroke,
    kind: &str,
) -> Node {
    Node::Path {
        id,
        d: format!(
            "M {} {} H {} V {} H {} Z",
            fmt(x),
            fmt(y),
            fmt(x + width),
            fmt(y + height),
            fmt(x)
        ),
        fill_rule: "nonzero".into(),
        fill,
        stroke,
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta {
            kind: kind.into(),
            ..SourceMeta::default()
        },
    }
}

fn merged_hidden_cells(ranges: &[MergeRange]) -> HashSet<(usize, usize)> {
    let mut hidden = HashSet::new();
    for range in ranges {
        for row in range.start_row..=range.end_row {
            for column in range.start_column..=range.end_column {
                if row != range.start_row || column != range.start_column {
                    hidden.insert((row, column));
                }
            }
        }
    }
    hidden
}

fn parse_cell_reference(value: &str) -> Option<(usize, usize)> {
    let value = value.replace('$', "");
    let split = value.find(|character: char| character.is_ascii_digit())?;
    let (letters, digits) = value.split_at(split);
    let row = digits.parse::<usize>().ok()?;
    let mut column = 0usize;
    for byte in letters.bytes() {
        let upper = byte.to_ascii_uppercase();
        if !upper.is_ascii_uppercase() {
            return None;
        }
        column = column
            .checked_mul(26)?
            .checked_add((upper - b'A' + 1) as usize)?;
    }
    (row > 0 && column > 0).then_some((row, column))
}

fn parse_merge_range(value: &str) -> Option<MergeRange> {
    let mut parts = value.split(':');
    let (start_row, start_column) = parse_cell_reference(parts.next()?)?;
    let (end_row, end_column) = parse_cell_reference(parts.next().unwrap_or(value))?;
    Some(MergeRange {
        start_row: start_row.min(end_row),
        start_column: start_column.min(end_column),
        end_row: start_row.max(end_row),
        end_column: start_column.max(end_column),
    })
}

fn split_print_axis(start: usize, end: usize, breaks: &[usize]) -> Vec<(usize, usize)> {
    if start > end {
        return Vec::new();
    }
    let mut output = Vec::new();
    let mut current = start;
    for breakpoint in breaks {
        if *breakpoint >= current && *breakpoint < end {
            output.push((current, *breakpoint));
            current = breakpoint.saturating_add(1);
        }
    }
    output.push((current, end));
    output
}

fn split_axis_by_limits(
    start: usize,
    end: usize,
    maximum_points: f64,
    maximum_items: usize,
    size: impl Fn(usize) -> f64,
) -> Vec<(usize, usize)> {
    if start > end {
        return Vec::new();
    }
    let mut output = Vec::new();
    let mut segment_start = start;
    let mut points = 0.0;
    let mut items = 0usize;
    for index in start..=end {
        let item_points = size(index).max(0.0);
        if items > 0 && (points + item_points > maximum_points || items >= maximum_items.max(1)) {
            output.push((segment_start, index - 1));
            segment_start = index;
            points = 0.0;
            items = 0;
        }
        points += item_points;
        items += 1;
    }
    output.push((segment_start, end));
    output
}

fn cell_range_reference(range: MergeRange) -> String {
    format!(
        "{}:{}",
        cell_reference(range.start_row, range.start_column),
        cell_reference(range.end_row, range.end_column)
    )
}

fn cell_reference(row: usize, mut column: usize) -> String {
    let mut letters = Vec::new();
    while column > 0 {
        column -= 1;
        letters.push((b'A' + (column % 26) as u8) as char);
        column /= 26;
    }
    letters.reverse();
    format!("{}{row}", letters.into_iter().collect::<String>())
}

fn column_width_to_points(width: f64) -> f64 {
    let pixels = ((256.0 * width + (128.0_f64 / 7.0).floor()) / 256.0 * 7.0).floor();
    pixels * 0.75
}

fn argb_to_rgb(value: &str) -> String {
    let trimmed = value.trim().trim_start_matches('#');
    let rgb = if trimmed.len() == 8 {
        &trimmed[2..]
    } else {
        trimmed
    };
    color_from_hex(rgb, "#000000")
}

fn decode_xlsx_text(text: &quick_xml::events::BytesText<'_>, context: &str) -> Result<String> {
    let decoded = text
        .decode()
        .map_err(|error| Error::InvalidInput(format!("invalid {context}: {error}")))?;
    quick_xml::escape::unescape(&decoded)
        .map(|value| value.into_owned())
        .map_err(|error| Error::InvalidInput(format!("invalid XML escape in {context}: {error}")))
}

fn decode_xlsx_reference(
    reference: &quick_xml::events::BytesRef<'_>,
    context: &str,
) -> Result<String> {
    let name = reference.decode().map_err(|error| {
        Error::InvalidInput(format!("invalid XML reference in {context}: {error}"))
    })?;
    quick_xml::escape::unescape(&format!("&{name};"))
        .map(|value| value.into_owned())
        .map_err(|error| {
            Error::InvalidInput(format!("invalid XML reference in {context}: {error}"))
        })
}

fn fmt(value: f64) -> String {
    format!("{value:.4}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn deduplicate(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_excel_cell_references() {
        assert_eq!(parse_cell_reference("A1"), Some((1, 1)));
        assert_eq!(parse_cell_reference("$AA$42"), Some((42, 27)));
        assert_eq!(cell_reference(42, 27), "AA42");
    }

    #[test]
    fn formats_excel_dates_times_and_elapsed_hours() {
        let styles = Styles::default();
        let style = |number_format_id| CellStyle {
            number_format_id,
            ..CellStyle::default()
        };
        assert_eq!(styles.format_value("45292", &style(14)), "1/1/24");
        assert_eq!(styles.format_value("0.5", &style(18)), "12:00 PM");
        assert_eq!(styles.format_value("1.5", &style(46)), "36:00:00");

        let mut styles_1904 = Styles {
            date_1904: true,
            ..Styles::default()
        };
        assert_eq!(styles_1904.format_value("0", &style(14)), "1/1/04");
        styles_1904.date_1904 = false;
    }

    #[test]
    fn formats_excel_numeric_patterns() {
        let mut styles = Styles::default();
        styles.custom_number_formats.insert(200, "¥#,##0.00".into());
        let style = |number_format_id| CellStyle {
            number_format_id,
            ..CellStyle::default()
        };
        assert_eq!(styles.format_value("12345.6", &style(4)), "12,345.60");
        assert_eq!(styles.format_value("12345.6", &style(200)), "¥12,345.60");
        assert_eq!(styles.format_value("0.125", &style(10)), "12.50%");
        assert_eq!(styles.format_value("1234", &style(11)), "1.23E3");
        assert_eq!(styles.format_value("-1234", &style(37)), "(1,234)");
    }

    #[test]
    fn evaluates_nested_formula_functions_and_comparisons() {
        let cells = BTreeMap::from([
            (
                (2, 2),
                Cell {
                    row: 2,
                    column: 2,
                    raw_value: "7".into(),
                    ..Cell::default()
                },
            ),
            (
                (3, 2),
                Cell {
                    row: 3,
                    column: 2,
                    formula: "IF(6<B2,ROUND(9.6,0),0)".into(),
                    ..Cell::default()
                },
            ),
        ]);
        let mut operations = 0;
        let value = evaluate_formula_position(
            (3, 2),
            &cells,
            &FormulaContext::default(),
            &mut HashMap::new(),
            &mut HashSet::new(),
            &mut operations,
            0,
        )
        .and_then(|value| value.as_number());
        assert_eq!(value, Some(10.0));
    }

    #[test]
    fn evaluates_conditional_aggregate_formula_functions() {
        let cells = BTreeMap::from([
            (
                (1, 1),
                Cell {
                    row: 1,
                    column: 1,
                    value: "East".into(),
                    ..Cell::default()
                },
            ),
            (
                (2, 1),
                Cell {
                    row: 2,
                    column: 1,
                    value: "West".into(),
                    ..Cell::default()
                },
            ),
            (
                (3, 1),
                Cell {
                    row: 3,
                    column: 1,
                    value: "East".into(),
                    ..Cell::default()
                },
            ),
            (
                (1, 2),
                Cell {
                    row: 1,
                    column: 2,
                    raw_value: "10".into(),
                    ..Cell::default()
                },
            ),
            (
                (2, 2),
                Cell {
                    row: 2,
                    column: 2,
                    raw_value: "20".into(),
                    ..Cell::default()
                },
            ),
            (
                (3, 2),
                Cell {
                    row: 3,
                    column: 2,
                    raw_value: "30".into(),
                    ..Cell::default()
                },
            ),
            (
                (1, 3),
                Cell {
                    row: 1,
                    column: 3,
                    formula: "COUNTIF(A1:A3,\"e*\")".into(),
                    ..Cell::default()
                },
            ),
            (
                (2, 3),
                Cell {
                    row: 2,
                    column: 3,
                    formula: "SUMIF(A1:A3,\"east\",B1:B3)".into(),
                    ..Cell::default()
                },
            ),
            (
                (3, 3),
                Cell {
                    row: 3,
                    column: 3,
                    formula: "AVERAGEIF(B1:B3,\">10\")".into(),
                    ..Cell::default()
                },
            ),
            (
                (1, 4),
                Cell {
                    row: 1,
                    column: 4,
                    formula: "COUNTIFS(A1:A3,\"east\",B1:B3,\">20\")".into(),
                    ..Cell::default()
                },
            ),
            (
                (2, 4),
                Cell {
                    row: 2,
                    column: 4,
                    formula: "SUMIFS(B1:B3,A1:A3,\"east\",B1:B3,\">10\")".into(),
                    ..Cell::default()
                },
            ),
            (
                (3, 4),
                Cell {
                    row: 3,
                    column: 4,
                    formula: "AVERAGEIFS(B1:B3,A1:A3,\"east\")".into(),
                    ..Cell::default()
                },
            ),
        ]);
        for (position, expected) in [
            ((1, 3), 2.0),
            ((2, 3), 40.0),
            ((3, 3), 25.0),
            ((1, 4), 1.0),
            ((2, 4), 30.0),
            ((3, 4), 20.0),
        ] {
            let mut operations = 0;
            let value = evaluate_formula_position(
                position,
                &cells,
                &FormulaContext::default(),
                &mut HashMap::new(),
                &mut HashSet::new(),
                &mut operations,
                0,
            )
            .and_then(|value| value.as_number());
            assert_eq!(value, Some(expected));
        }
    }

    #[test]
    fn unescapes_formula_comparison_entities() {
        let text = quick_xml::events::BytesText::from_escaped("6&lt;B2");
        assert_eq!(decode_xlsx_text(&text, "formula").unwrap(), "6<B2");
    }
}
