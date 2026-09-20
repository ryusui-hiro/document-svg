//! Bounded Visio Open XML drawing preview.
//!
//! A VSDX package stores page order and page relationships separately from
//! each page's ShapeSheet XML. This reader follows only internal package
//! relationships and renders direct page geometry, direct styles, and text;
//! it never opens external links or executes embedded macros.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};
use crate::ooxml::{ZipPackage, attribute, decode_xml_reference, local_name, text_advance_factor};

const MAX_VSDX_PART_BYTES: u64 = 32 * 1024 * 1024;
const MAX_VSDX_TOTAL_XML_BYTES: u64 = 512 * 1024 * 1024;
const MAX_VSDX_ENTRIES: usize = 100_000;
const MAX_VSDX_PAGES: usize = 10_000;
const MAX_VSDX_SHAPES_PER_PAGE: usize = 100_000;
const MAX_VSDX_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_VSDX_TEXT_LINES: usize = 100_000;
const MAX_VSDX_DEPTH: usize = 256;
const DEFAULT_PAGE_WIDTH_IN: f64 = 8.5;
const DEFAULT_PAGE_HEIGHT_IN: f64 = 11.0;
const MIN_PAGE_IN: f64 = 0.1;
const MAX_PAGE_IN: f64 = 10_000.0;
const MAX_COORD_IN: f64 = 100_000.0;

#[derive(Clone, Debug)]
struct PageRef {
    id: String,
    name: String,
    relationship_id: String,
    page_sheet_cells: HashMap<String, String>,
    is_background: bool,
    references_background: bool,
}

#[derive(Clone, Debug, Default)]
struct GeometryRow {
    kind: String,
    values: HashMap<String, f64>,
    deleted: bool,
}

#[derive(Clone, Debug, Default)]
struct VisioShape {
    id: String,
    name: String,
    shape_type: String,
    master: Option<String>,
    cells: HashMap<String, String>,
    character_cells: HashMap<String, String>,
    geometry: Vec<GeometryRow>,
    text: String,
    has_nested_shapes: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ParsedPage {
    width_in: f64,
    height_in: f64,
    has_width: bool,
    has_height: bool,
    shapes: Vec<VisioShape>,
    has_foreign_data: bool,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut package = ZipPackage::open(path, options.max_zip_entry_bytes.min(MAX_VSDX_PART_BYTES))
        .map_err(|error| Error::InvalidInput(format!("failed to open Visio package: {error}")))?;
    if package.entry_count() > MAX_VSDX_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "Visio package contains {} entries; maximum is {MAX_VSDX_ENTRIES}",
            package.entry_count()
        )));
    }

    let mut total_xml_bytes = 0u64;
    let (package_rels, rel_bytes) = package.package_relationships(options.max_xml_events)?;
    total_xml_bytes = total_xml_bytes.saturating_add(rel_bytes as u64);
    let document_part = package_rels
        .ids_of_type("visioDocument")
        .chain(package_rels.ids_of_type("officeDocument"))
        .find_map(|(id, _)| package_rels.target(id, ""))
        .or_else(|| {
            package
                .contains("visio/document.xml")
                .then(|| "visio/document.xml".into())
        })
        .ok_or_else(|| Error::InvalidInput("Visio package has no internal document part".into()))?;

    // Reading the document part verifies the package target and rejects a
    // missing/truncated part before following its page relationship table.
    let document_xml = read_xml_part(&mut package, &document_part, &mut total_xml_bytes, options)?;
    validate_xml_root(
        &document_xml,
        "VisioDocument",
        options.max_xml_events,
        &document_part,
    )?;
    let (document_rels, relationship_bytes) = package.relationships_with_size(
        &document_part,
        options.max_xml_events,
        MAX_VSDX_PART_BYTES,
    )?;
    add_xml_bytes(&mut total_xml_bytes, relationship_bytes, options)?;
    let pages_part = document_rels
        .ids_of_type("pages")
        .find_map(|(id, _)| document_rels.target(id, &document_part))
        .or_else(|| {
            package
                .contains("visio/pages/pages.xml")
                .then(|| "visio/pages/pages.xml".into())
        })
        .ok_or_else(|| Error::InvalidInput("Visio document has no pages part".into()))?;
    let pages_xml = read_xml_part(&mut package, &pages_part, &mut total_xml_bytes, options)?;
    let all_page_refs = parse_page_refs(&pages_xml, options.max_xml_events)?;
    let has_unrendered_background = all_page_refs
        .iter()
        .any(|page| page.is_background || page.references_background);
    let page_refs = all_page_refs
        .into_iter()
        .filter(|page| !page.is_background)
        .collect::<Vec<_>>();
    if page_refs.is_empty() {
        return Err(Error::InvalidInput(
            "Visio drawing contains no pages".into(),
        ));
    }
    if page_refs.len() > options.max_pages.min(MAX_VSDX_PAGES) {
        return Err(Error::LimitExceeded(format!(
            "Visio drawing contains {} pages; maximum is {}",
            page_refs.len(),
            options.max_pages.min(MAX_VSDX_PAGES)
        )));
    }

    let (pages_rels, relationship_bytes) = package.relationships_with_size(
        &pages_part,
        options.max_xml_events,
        MAX_VSDX_PART_BYTES,
    )?;
    add_xml_bytes(&mut total_xml_bytes, relationship_bytes, options)?;
    let mut warnings = Vec::new();
    if has_unrendered_background {
        warnings
            .push("Visio background pages are not rendered or applied to foreground pages".into());
    }
    for (index, page_ref) in page_refs.iter().enumerate() {
        let Some(page_part) = pages_rels.target(&page_ref.relationship_id, &pages_part) else {
            return Err(Error::InvalidInput(format!(
                "Visio page '{}' has no safe internal page relationship",
                page_ref.name
            )));
        };
        let xml = read_xml_part(&mut package, &page_part, &mut total_xml_bytes, options)?;
        validate_xml_root(&xml, "PageContents", options.max_xml_events, &page_part)?;
        let mut parsed = parse_page(&xml, options.max_xml_events, &page_part)?;
        if page_ref.page_sheet_cells.contains_key("PageWidth") {
            parsed.width_in = page_dimension(
                &page_ref.page_sheet_cells,
                "PageWidth",
                parsed.width_in,
                &pages_part,
            )?;
            parsed.has_width = true;
        }
        if page_ref.page_sheet_cells.contains_key("PageHeight") {
            parsed.height_in = page_dimension(
                &page_ref.page_sheet_cells,
                "PageHeight",
                parsed.height_in,
                &pages_part,
            )?;
            parsed.has_height = true;
        }
        let mut page = render_page(parsed, index + 1, &page_ref.name, &page_ref.id);
        if page_ref.name.is_empty() {
            page.warn(format!(
                "Visio page {} has no name; a generic title is used",
                index + 1
            ));
        }
        if page.nodes.is_empty() {
            page.warn("Visio page contains no supported visible shapes");
        }
        for warning in &page.warnings {
            if !warnings.contains(warning) {
                warnings.push(warning.clone());
            }
        }
        sink.consume(page)?;
    }
    Ok(warnings)
}

fn read_xml_part(
    package: &mut ZipPackage<File>,
    part: &str,
    total_bytes: &mut u64,
    options: &ConvertOptions,
) -> Result<Vec<u8>> {
    let bytes = package.read_limited(part, MAX_VSDX_PART_BYTES)?;
    *total_bytes = total_bytes
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| Error::LimitExceeded("Visio XML byte count overflowed".into()))?;
    let limit = options.max_input_bytes.min(MAX_VSDX_TOTAL_XML_BYTES);
    if *total_bytes > limit {
        return Err(Error::LimitExceeded(format!(
            "Visio package XML exceeds the {limit}-byte total read limit"
        )));
    }
    Ok(bytes)
}

fn add_xml_bytes(total_bytes: &mut u64, bytes: usize, options: &ConvertOptions) -> Result<()> {
    *total_bytes = total_bytes
        .checked_add(bytes as u64)
        .ok_or_else(|| Error::LimitExceeded("Visio XML byte count overflowed".into()))?;
    let limit = options.max_input_bytes.min(MAX_VSDX_TOTAL_XML_BYTES);
    if *total_bytes > limit {
        return Err(Error::LimitExceeded(format!(
            "Visio package XML exceeds the {limit}-byte total read limit"
        )));
    }
    Ok(())
}

fn validate_xml_root(xml: &[u8], expected: &str, max_events: usize, part: &str) -> Result<()> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut events = 0usize;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "Visio XML part {part} exceeds {max_events} events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) | Event::Empty(start) => {
                let root = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if root != expected {
                    return Err(Error::InvalidInput(format!(
                        "Visio part {part} has root <{root}>; expected <{expected}>"
                    )));
                }
                return Ok(());
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(format!(
                    "Visio XML part {part} contains a document type declaration"
                )));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Err(Error::InvalidInput(format!(
        "Visio XML part {part} is empty"
    )))
}

fn parse_page_refs(xml: &[u8], max_events: usize) -> Result<Vec<PageRef>> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut events = 0usize;
    let mut pages = Vec::new();
    let mut current_page: Option<PageRef> = None;
    let mut in_page_sheet = false;
    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "Visio pages part exceeds {max_events} XML events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(start) if local_name(start.name().as_ref()) == b"Page" => {
                if current_page.is_some() {
                    return Err(Error::InvalidInput(
                        "Visio pages part contains nested Page entries".into(),
                    ));
                }
                current_page = Some(page_ref_from_start(&start));
            }
            Event::Empty(start) if local_name(start.name().as_ref()) == b"Page" => {
                let page = page_ref_from_start(&start);
                push_page_ref(&mut pages, page)?;
            }
            Event::Start(start) | Event::Empty(start)
                if local_name(start.name().as_ref()) == b"Rel" && current_page.is_some() =>
            {
                if let Some(relationship_id) = attribute(&start, b"id")
                    && let Some(page) = current_page.as_mut()
                {
                    page.relationship_id = relationship_id;
                }
            }
            Event::Start(start)
                if local_name(start.name().as_ref()) == b"PageSheet" && current_page.is_some() =>
            {
                in_page_sheet = true;
            }
            Event::Start(start) | Event::Empty(start)
                if local_name(start.name().as_ref()) == b"Cell"
                    && in_page_sheet
                    && current_page.is_some() =>
            {
                capture_page_dimension_cell(&start, current_page.as_mut());
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"Page" => {
                if let Some(page) = current_page.take() {
                    push_page_ref(&mut pages, page)?;
                }
            }
            Event::End(end) if local_name(end.name().as_ref()) == b"PageSheet" => {
                in_page_sheet = false;
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "Visio pages part contains a document type declaration".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if current_page.is_some() {
        return Err(Error::InvalidInput(
            "Visio pages part ended inside a Page entry".into(),
        ));
    }
    Ok(pages)
}

fn page_ref_from_start(start: &BytesStart<'_>) -> PageRef {
    let background_reference = attribute(start, b"BackPage").unwrap_or_default();
    PageRef {
        id: attribute(start, b"ID").unwrap_or_default(),
        name: attribute(start, b"NameU")
            .filter(|value| !value.is_empty())
            .or_else(|| attribute(start, b"Name"))
            .unwrap_or_default(),
        relationship_id: attribute(start, b"id").unwrap_or_default(),
        page_sheet_cells: HashMap::new(),
        is_background: attribute(start, b"IsBackground")
            .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true")),
        references_background: !background_reference.is_empty() && background_reference != "0",
    }
}

fn capture_page_dimension_cell(cell: &BytesStart<'_>, page: Option<&mut PageRef>) {
    let (Some(name), Some(value), Some(page)) =
        (attribute(cell, b"N"), attribute(cell, b"V"), page)
    else {
        return;
    };
    if name == "PageWidth" || name == "PageHeight" {
        page.page_sheet_cells.insert(name, value);
    }
}

fn push_page_ref(pages: &mut Vec<PageRef>, page: PageRef) -> Result<()> {
    if page.id.is_empty() || page.relationship_id.is_empty() {
        return Err(Error::InvalidInput(
            "Visio Page entry is missing ID or relationship id".into(),
        ));
    }
    if pages.len() >= MAX_VSDX_PAGES {
        return Err(Error::LimitExceeded(format!(
            "Visio pages part exceeds {MAX_VSDX_PAGES} pages"
        )));
    }
    pages.push(page);
    Ok(())
}

pub(crate) fn parse_page(xml: &[u8], max_events: usize, part: &str) -> Result<ParsedPage> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut events = 0usize;
    let mut page_cells = HashMap::<String, String>::new();
    let mut active_section: Option<String> = None;
    let mut active_row: Option<GeometryRow> = None;
    let mut active_shape: Option<VisioShape> = None;
    let mut shapes = Vec::new();
    let mut text_depth: Option<usize> = None;
    let mut text_bytes = 0usize;
    let mut has_foreign_data = false;

    loop {
        events += 1;
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "Visio page part {part} exceeds {max_events} XML events"
            )));
        }
        let event = reader.read_event_into(&mut buffer)?;
        match event {
            Event::Start(start) => {
                let name = String::from_utf8_lossy(local_name(start.name().as_ref())).into_owned();
                if name == "ForeignData" || name == "Foreign" {
                    has_foreign_data = true;
                }
                let enclosing_shapes = stack
                    .iter()
                    .filter(|parent| parent.as_str() == "Shape")
                    .count();
                if enclosing_shapes > 1 {
                    stack.push(name);
                    if stack.len() > MAX_VSDX_DEPTH {
                        return Err(Error::LimitExceeded(format!(
                            "Visio page {part} exceeds XML nesting limit {MAX_VSDX_DEPTH}"
                        )));
                    }
                    buffer.clear();
                    continue;
                }
                if name == "Shape" {
                    if enclosing_shapes == 1 {
                        if let Some(shape) = active_shape.as_mut() {
                            shape.has_nested_shapes = true;
                        }
                        stack.push(name);
                        buffer.clear();
                        continue;
                    } else {
                        if shapes.len() >= MAX_VSDX_SHAPES_PER_PAGE {
                            return Err(Error::LimitExceeded(format!(
                                "Visio page {part} exceeds {MAX_VSDX_SHAPES_PER_PAGE} top-level shapes"
                            )));
                        }
                        active_shape = Some(shape_from_start(&start)?);
                    }
                } else if name == "Section" {
                    active_section = attribute(&start, b"N");
                } else if name == "Row"
                    && enclosing_shapes == 1
                    && active_section.as_deref() == Some("Geometry")
                {
                    active_row = Some(GeometryRow {
                        kind: attribute(&start, b"T").unwrap_or_default(),
                        deleted: attribute(&start, b"Del").is_some_and(|value| value == "1"),
                        ..GeometryRow::default()
                    });
                } else if name == "Text"
                    && active_shape.is_some()
                    && stack.iter().filter(|item| *item == "Shape").count() == 1
                {
                    text_depth = Some(stack.len() + 1);
                }
                if name == "Cell" {
                    capture_cell(
                        &start,
                        &mut active_shape,
                        active_section.as_deref(),
                        active_row.as_mut(),
                        &mut page_cells,
                        &stack,
                    );
                }
                stack.push(name);
                if stack.len() > MAX_VSDX_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "Visio page {part} exceeds XML nesting limit {MAX_VSDX_DEPTH}"
                    )));
                }
            }
            Event::Empty(start) => {
                let name = local_name(start.name().as_ref()).to_vec();
                if name.as_slice() == b"ForeignData" || name.as_slice() == b"Foreign" {
                    has_foreign_data = true;
                }
                if name.as_slice() == b"Shape" {
                    if stack.iter().any(|parent| parent == "Shape") {
                        if let Some(shape) = active_shape.as_mut() {
                            shape.has_nested_shapes = true;
                        }
                    } else {
                        if shapes.len() >= MAX_VSDX_SHAPES_PER_PAGE {
                            return Err(Error::LimitExceeded(format!(
                                "Visio page {part} exceeds {MAX_VSDX_SHAPES_PER_PAGE} top-level shapes"
                            )));
                        }
                        shapes.push(shape_from_start(&start)?);
                    }
                } else if name.as_slice() == b"Cell" {
                    capture_cell(
                        &start,
                        &mut active_shape,
                        active_section.as_deref(),
                        active_row.as_mut(),
                        &mut page_cells,
                        &stack,
                    );
                }
            }
            Event::Text(text) if text_depth.is_some() => {
                let decoded = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid Visio text in {part}: {error}"))
                })?;
                text_bytes = text_bytes.saturating_add(decoded.len());
                if text_bytes > MAX_VSDX_TEXT_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "Visio page {part} text exceeds {MAX_VSDX_TEXT_BYTES} bytes"
                    )));
                }
                if let Some(shape) = active_shape.as_mut() {
                    shape.text.push_str(&decoded);
                }
            }
            Event::CData(text) if text_depth.is_some() => {
                let decoded = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid Visio CDATA in {part}: {error}"))
                })?;
                text_bytes = text_bytes.saturating_add(decoded.len());
                if text_bytes > MAX_VSDX_TEXT_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "Visio page {part} text exceeds {MAX_VSDX_TEXT_BYTES} bytes"
                    )));
                }
                if let Some(shape) = active_shape.as_mut() {
                    shape.text.push_str(&decoded);
                }
            }
            Event::GeneralRef(reference) if text_depth.is_some() => {
                let decoded = decode_xml_reference(&reference, "Visio shape text")?;
                text_bytes = text_bytes.saturating_add(decoded.len());
                if text_bytes > MAX_VSDX_TEXT_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "Visio page {part} text exceeds {MAX_VSDX_TEXT_BYTES} bytes"
                    )));
                }
                if let Some(shape) = active_shape.as_mut() {
                    shape.text.push_str(&decoded);
                }
            }
            Event::End(end) => {
                let name = local_name(end.name().as_ref()).to_vec();
                let nested_shape = stack.iter().filter(|item| item.as_str() == "Shape").count() > 1;
                if nested_shape {
                    stack.pop();
                    buffer.clear();
                    continue;
                }
                if name.as_slice() == b"Text" {
                    text_depth = None;
                } else if name.as_slice() == b"Row" {
                    if let Some(row) = active_row.take()
                        && !row.deleted
                        && let Some(shape) = active_shape.as_mut()
                    {
                        shape.geometry.push(row);
                    }
                } else if name.as_slice() == b"Section" {
                    active_section = None;
                } else if name.as_slice() == b"Shape"
                    && !stack.iter().rev().skip(1).any(|item| item == "Shape")
                    && let Some(mut shape) = active_shape.take()
                {
                    shape.text = shape.text.trim().to_owned();
                    shapes.push(shape);
                }
                stack.pop();
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(format!(
                    "Visio page part {part} contains a document type declaration"
                )));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if active_shape.is_some() || active_row.is_some() {
        return Err(Error::InvalidInput(format!(
            "Visio page part {part} ended inside a shape or geometry row"
        )));
    }
    let width_in = page_dimension(&page_cells, "PageWidth", DEFAULT_PAGE_WIDTH_IN, part)?;
    let height_in = page_dimension(&page_cells, "PageHeight", DEFAULT_PAGE_HEIGHT_IN, part)?;
    Ok(ParsedPage {
        width_in,
        height_in,
        has_width: page_cells.contains_key("PageWidth"),
        has_height: page_cells.contains_key("PageHeight"),
        shapes,
        has_foreign_data,
    })
}

fn shape_from_start(start: &BytesStart<'_>) -> Result<VisioShape> {
    let id = attribute(start, b"ID")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::InvalidInput("Visio Shape is missing its required ID".into()))?;
    Ok(VisioShape {
        id,
        name: attribute(start, b"NameU")
            .filter(|value| !value.is_empty())
            .or_else(|| attribute(start, b"Name"))
            .unwrap_or_default(),
        shape_type: attribute(start, b"Type").unwrap_or_default(),
        master: attribute(start, b"Master"),
        ..VisioShape::default()
    })
}

fn capture_cell(
    cell: &BytesStart<'_>,
    active_shape: &mut Option<VisioShape>,
    active_section: Option<&str>,
    active_row: Option<&mut GeometryRow>,
    page_cells: &mut HashMap<String, String>,
    stack: &[String],
) {
    if stack.iter().filter(|name| name.as_str() == "Shape").count() > 1 {
        return;
    }
    let Some(name) = attribute(cell, b"N") else {
        return;
    };
    let Some(value) = attribute(cell, b"V") else {
        return;
    };
    if let Some(row) = active_row {
        row.values
            .insert(name, value.parse::<f64>().unwrap_or(f64::NAN));
    } else if let Some(shape) = active_shape.as_mut() {
        if active_section == Some("Character") {
            shape.character_cells.entry(name).or_insert(value);
        } else if active_section.is_none() {
            shape.cells.entry(name).or_insert(value);
        }
    } else if stack.iter().any(|name| name == "PageSheet") {
        page_cells.insert(name, value);
    }
}

fn page_dimension(
    cells: &HashMap<String, String>,
    name: &str,
    fallback: f64,
    part: &str,
) -> Result<f64> {
    let Some(value) = cells.get(name) else {
        return Ok(fallback);
    };
    let parsed = value.parse::<f64>().map_err(|_| {
        Error::InvalidInput(format!(
            "Visio {name} is not a plain numeric value in {part}"
        ))
    })?;
    if !parsed.is_finite() || !(MIN_PAGE_IN..=MAX_PAGE_IN).contains(&parsed) {
        return Err(Error::InvalidInput(format!(
            "Visio {name} is outside supported page bounds in {part}"
        )));
    }
    Ok(parsed)
}

pub(crate) fn render_page(parsed: ParsedPage, number: usize, name: &str, page_id: &str) -> Page {
    let mut page = Page::new(
        number,
        parsed.width_in * 72.0,
        parsed.height_in * 72.0,
        "visio",
    );
    page.title = if name.is_empty() {
        format!("Visio page {number}")
    } else {
        name.to_owned()
    };
    page.description = format!("Visio page {page_id}");
    if !parsed.has_width || !parsed.has_height {
        page.warn("Visio page dimensions are missing; an 8.5 × 11 inch fallback is used for missing values");
    }
    if parsed.has_foreign_data {
        page.warn("Visio foreign objects and embedded picture data are not rendered");
    }

    let mut total_text_lines = 0usize;
    for shape in parsed.shapes {
        if cell_number(&shape.cells, "NoShow", 0.0) != 0.0 {
            continue;
        }
        if let Some(path_data) = one_dimensional_path(&shape.cells) {
            let transform = [72.0, 0.0, 0.0, -72.0, 0.0, parsed.height_in * 72.0];
            let meta = SourceMeta {
                kind: "visio-connector".into(),
                source_id: shape.id.clone(),
                semantic_role: "office:visio-connector".into(),
                ..SourceMeta::default()
            };
            page.nodes.push(Node::Path {
                id: format!("visio-shape-{}", safe_id(&shape.id)),
                d: path_data,
                fill_rule: "nonzero".into(),
                fill: Paint::None,
                stroke: line_stroke(&shape.cells),
                transform,
                clip_id: None,
                meta: meta.clone(),
            });
            if cell_number(&shape.cells, "BeginArrow", 0.0) != 0.0
                || cell_number(&shape.cells, "EndArrow", 0.0) != 0.0
            {
                page.warn(format!(
                    "Visio connector '{}' has arrowheads that are not rendered",
                    shape.name
                ));
            }
            if !shape.geometry.is_empty() {
                page.warn(format!(
                    "Visio connector '{}' contains route geometry; only the begin/end endpoint line is rendered",
                    shape.name
                ));
            }
            if shape.master.is_some() {
                page.warn(format!(
                    "Visio connector '{}' may inherit line styles from a master; only direct ShapeSheet data is rendered",
                    shape.name
                ));
            }
            if !shape.text.is_empty() {
                let (begin_x, begin_y, end_x, end_y) = line_endpoints(&shape.cells).unwrap();
                let font_size = cell_number(&shape.character_cells, "Size", 12.0 / 72.0);
                let font_size = if (4.0 / 72.0..=2.0).contains(&font_size) {
                    font_size
                } else {
                    12.0 / 72.0
                };
                let color = shape
                    .character_cells
                    .get("Color")
                    .and_then(|value| parse_visio_color(value))
                    .unwrap_or_else(|| "#1f2937".into());
                let available_width = ((end_x - begin_x).hypot(end_y - begin_y) * 0.8).max(1.0);
                let mut lines = wrap_visio_text(&shape.text, available_width, font_size);
                let remaining_lines = MAX_VSDX_TEXT_LINES.saturating_sub(total_text_lines);
                if lines.len() > remaining_lines {
                    page.warn(format!(
                        "Visio connector '{}' text lines were truncated at the page limit of {MAX_VSDX_TEXT_LINES}",
                        shape.name
                    ));
                    lines.truncate(remaining_lines);
                }
                let rendered_line_count = lines.len();
                let line_height = font_size * 1.2;
                let center_y = (begin_y + end_y) / 2.0;
                let start_offset = (lines.len().saturating_sub(1) as f64) * line_height / 2.0;
                for (line_index, line) in lines.into_iter().enumerate() {
                    let baseline = center_y + start_offset
                        - line_index as f64 * line_height
                        - font_size * 0.35;
                    page.nodes.push(Node::Text {
                        id: format!("visio-text-{}-{line_index}", safe_id(&shape.id)),
                        x: 0.0,
                        y: 0.0,
                        runs: vec![TextRun {
                            text: line,
                            font_family: "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into(),
                            font_size,
                            fill: Paint::solid(color.clone()),
                            ..TextRun::default()
                        }],
                        anchor: TextAnchor::Middle,
                        transform: page_text_transform(
                            parsed.height_in,
                            (begin_x + end_x) / 2.0,
                            baseline,
                        ),
                        opacity: 1.0,
                        stroke: Stroke::default(),
                        clip_id: None,
                        meta: meta.clone(),
                    });
                }
                total_text_lines = total_text_lines.saturating_add(rendered_line_count);
            }
            continue;
        }
        let width = cell_number(&shape.cells, "Width", 0.0);
        let height = cell_number(&shape.cells, "Height", 0.0);
        let pin_x = cell_number(&shape.cells, "PinX", 0.0);
        let pin_y = cell_number(&shape.cells, "PinY", 0.0);
        let loc_pin_x = cell_number(&shape.cells, "LocPinX", width / 2.0);
        let loc_pin_y = cell_number(&shape.cells, "LocPinY", height / 2.0);
        let angle = cell_number(&shape.cells, "Angle", 0.0);
        let Some(transform) = shape_transform(
            parsed.height_in,
            (pin_x, pin_y),
            (loc_pin_x, loc_pin_y),
            angle,
            (
                cell_number(&shape.cells, "FlipX", 0.0) != 0.0,
                cell_number(&shape.cells, "FlipY", 0.0) != 0.0,
            ),
        ) else {
            page.warn(format!(
                "Visio shape '{}' has invalid or out-of-range position cells and was skipped",
                shape.name
            ));
            continue;
        };
        if shape.has_nested_shapes || shape.shape_type.eq_ignore_ascii_case("Group") {
            page.warn(format!(
                "Visio group '{}' contains nested shapes that are not rendered",
                shape.name
            ));
            continue;
        }
        if shape.master.is_some() {
            page.warn(format!(
                "Visio shape '{}' may inherit geometry or styles from a master; only direct ShapeSheet data is rendered",
                shape.name
            ));
        }

        let mut geometry_warning = false;
        let geometry = geometry_path(&shape.geometry, width, height, &mut geometry_warning)
            .or_else(|| fallback_shape_path(&shape.name, width, height));
        let Some(path_data) = geometry else {
            page.warn(format!(
                "Visio shape '{}' has no supported geometry and was skipped",
                shape.name
            ));
            continue;
        };
        if geometry_warning {
            page.warn(format!(
                "Visio shape '{}' uses geometry rows outside the supported MoveTo/LineTo subset; unsupported rows were omitted",
                shape.name
            ));
        }

        let fill = fill_paint(&shape.cells);
        let stroke = line_stroke(&shape.cells);
        let meta = SourceMeta {
            kind: "visio-shape".into(),
            source_id: shape.id.clone(),
            semantic_role: "office:visio-shape".into(),
            ..SourceMeta::default()
        };
        page.nodes.push(Node::Path {
            id: format!("visio-shape-{}", safe_id(&shape.id)),
            d: path_data,
            fill_rule: "nonzero".into(),
            fill,
            stroke,
            transform,
            clip_id: None,
            meta: meta.clone(),
        });

        if !shape.text.is_empty() {
            let font_size = cell_number(&shape.character_cells, "Size", 12.0 / 72.0);
            let font_size = if font_size.is_finite() && (4.0 / 72.0..=2.0).contains(&font_size) {
                font_size
            } else {
                page.warn(format!(
                    "Visio shape '{}' has unsupported character sizing; 12-point text is used",
                    shape.name
                ));
                12.0 / 72.0
            };
            let color = shape
                .character_cells
                .get("Color")
                .and_then(|value| parse_visio_color(value))
                .unwrap_or_else(|| "#1f2937".into());
            let anchor = match cell_number(&shape.cells, "HorzAlign", 1.0) as i32 {
                0 => TextAnchor::Start,
                2 => TextAnchor::End,
                _ => TextAnchor::Middle,
            };
            let left_margin =
                cell_number(&shape.cells, "LeftMargin", 0.1).clamp(0.0, width.max(0.0));
            let right_margin =
                cell_number(&shape.cells, "RightMargin", 0.1).clamp(0.0, width.max(0.0));
            let local_x = match anchor {
                TextAnchor::Start => left_margin,
                TextAnchor::Middle => cell_number(&shape.cells, "TxtPinX", width / 2.0),
                TextAnchor::End => width - right_margin,
            };
            let available_width = cell_number(
                &shape.cells,
                "TxtWidth",
                (width - left_margin - right_margin).max(0.05),
            )
            .clamp(0.05, MAX_COORD_IN);
            let mut lines = wrap_visio_text(&shape.text, available_width, font_size);
            let remaining_lines = MAX_VSDX_TEXT_LINES.saturating_sub(total_text_lines);
            if lines.len() > remaining_lines {
                page.warn(format!(
                    "Visio shape '{}' text lines were truncated at the page limit of {MAX_VSDX_TEXT_LINES}",
                    shape.name
                ));
                lines.truncate(remaining_lines);
            }
            let rendered_line_count = lines.len();
            let line_height = font_size * 1.2;
            let center_y = cell_number(&shape.cells, "TxtPinY", height / 2.0);
            let start_offset = (lines.len().saturating_sub(1) as f64) * line_height / 2.0;
            for (line_index, line) in lines.into_iter().enumerate() {
                let local_y =
                    center_y + start_offset - line_index as f64 * line_height - font_size * 0.35;
                page.nodes.push(Node::Text {
                    id: format!("visio-text-{}-{line_index}", safe_id(&shape.id)),
                    x: 0.0,
                    y: 0.0,
                    runs: vec![TextRun {
                        text: line,
                        font_family: "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into(),
                        font_size,
                        fill: Paint::solid(color.clone()),
                        ..TextRun::default()
                    }],
                    anchor,
                    transform: shape_text_transform(transform, angle, local_x, local_y),
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: meta.clone(),
                });
            }
            total_text_lines = total_text_lines.saturating_add(rendered_line_count);
        }
    }
    page
}

fn shape_transform(
    page_height: f64,
    pin: (f64, f64),
    loc_pin: (f64, f64),
    angle: f64,
    flips: (bool, bool),
) -> Option<[f64; 6]> {
    let (pin_x, pin_y) = pin;
    let (loc_x, loc_y) = loc_pin;
    let (flip_x, flip_y) = flips;
    if [page_height, pin_x, pin_y, loc_x, loc_y, angle]
        .iter()
        .any(|value| !value.is_finite() || value.abs() > MAX_COORD_IN)
    {
        return None;
    }
    let (sin, cos) = angle.sin_cos();
    let mirror_x = if flip_x { -1.0 } else { 1.0 };
    let mirror_y = if flip_y { -1.0 } else { 1.0 };
    Some([
        72.0 * cos * mirror_x,
        -72.0 * sin * mirror_x,
        -72.0 * sin * mirror_y,
        -72.0 * cos * mirror_y,
        72.0 * (pin_x - cos * mirror_x * loc_x + sin * mirror_y * loc_y),
        72.0 * (page_height - pin_y + sin * mirror_x * loc_x + cos * mirror_y * loc_y),
    ])
}

fn one_dimensional_path(cells: &HashMap<String, String>) -> Option<String> {
    let (begin_x, begin_y, end_x, end_y) = line_endpoints(cells)?;
    Some(format!(
        "M {} {} L {} {}",
        clean_number(begin_x),
        clean_number(begin_y),
        clean_number(end_x),
        clean_number(end_y)
    ))
}

fn page_text_transform(page_height: f64, x: f64, y: f64) -> [f64; 6] {
    [72.0, 0.0, 0.0, 72.0, x * 72.0, (page_height - y) * 72.0]
}

fn shape_text_transform(path_transform: [f64; 6], angle: f64, x: f64, y: f64) -> [f64; 6] {
    let (sin, cos) = angle.sin_cos();
    let [a, b, c, d, e, f] = path_transform;
    [
        72.0 * cos,
        -72.0 * sin,
        72.0 * sin,
        72.0 * cos,
        e + a * x + c * y,
        f + b * x + d * y,
    ]
}

fn wrap_visio_text(text: &str, max_width: f64, font_size: f64) -> Vec<String> {
    let max_width = if max_width.is_finite() {
        max_width.clamp(0.05, MAX_COORD_IN)
    } else {
        1.0
    };
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.trim().is_empty() {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            continue;
        }
        let mut line = String::new();
        let mut line_width = 0.0;
        for token in paragraph.split_whitespace() {
            let token_width = token
                .chars()
                .map(|character| text_advance_factor(character) * font_size)
                .sum::<f64>();
            let space_width = if line.is_empty() {
                0.0
            } else {
                text_advance_factor(' ') * font_size
            };
            if !line.is_empty() && line_width + space_width + token_width > max_width {
                lines.push(std::mem::take(&mut line));
                line_width = 0.0;
            }
            if token_width <= max_width {
                if !line.is_empty() {
                    line.push(' ');
                    line_width += text_advance_factor(' ') * font_size;
                }
                line.push_str(token);
                line_width += token_width;
                continue;
            }
            for character in token.chars() {
                let character_width = text_advance_factor(character) * font_size;
                if !line.is_empty() && line_width + character_width > max_width {
                    lines.push(std::mem::take(&mut line));
                    line_width = 0.0;
                }
                line.push(character);
                line_width += character_width;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        lines.push(text.to_owned());
    }
    lines
}

fn line_endpoints(cells: &HashMap<String, String>) -> Option<(f64, f64, f64, f64)> {
    let parse = |name: &str| {
        cells
            .get(name)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && value.abs() <= MAX_COORD_IN)
    };
    Some((
        parse("BeginX")?,
        parse("BeginY")?,
        parse("EndX")?,
        parse("EndY")?,
    ))
}

fn geometry_path(
    rows: &[GeometryRow],
    width: f64,
    height: f64,
    warning: &mut bool,
) -> Option<String> {
    let mut path = String::new();
    let mut has_point = false;
    for row in rows {
        let x = *row.values.get("X").unwrap_or(&f64::NAN);
        let y = *row.values.get("Y").unwrap_or(&f64::NAN);
        if !x.is_finite() || !y.is_finite() || x.abs() > MAX_COORD_IN || y.abs() > MAX_COORD_IN {
            *warning = true;
            continue;
        }
        let point = match row.kind.as_str() {
            "MoveTo" | "LineTo" => (x, y),
            "RelMoveTo" => (x * width, y * height),
            "RelLineTo" => (x * width, y * height),
            _ => {
                *warning = true;
                continue;
            }
        };
        match row.kind.as_str() {
            "MoveTo" | "RelMoveTo" => {
                path.push_str(&format!(
                    "M {} {} ",
                    clean_number(point.0),
                    clean_number(point.1)
                ));
                has_point = true;
            }
            "LineTo" | "RelLineTo" if has_point => {
                path.push_str(&format!(
                    "L {} {} ",
                    clean_number(point.0),
                    clean_number(point.1)
                ));
            }
            "LineTo" | "RelLineTo" => {
                *warning = true;
            }
            _ => {}
        }
    }
    (!path.is_empty()).then(|| path.trim().to_owned())
}

fn fallback_shape_path(name: &str, width: f64, height: f64) -> Option<String> {
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || width > MAX_COORD_IN
        || height > MAX_COORD_IN
    {
        return None;
    }
    let w = clean_number(width);
    let h = clean_number(height);
    let normalized = name.to_ascii_lowercase();
    if normalized.contains("decision") || normalized.contains("diamond") {
        Some(format!(
            "M {} 0 L {w} {} L {} {h} L 0 {} Z",
            width / 2.0,
            height / 2.0,
            width / 2.0,
            height / 2.0
        ))
    } else if normalized.contains("ellipse")
        || normalized.contains("terminator")
        || normalized.contains("start/end")
    {
        let rx = width / 2.0;
        let ry = height / 2.0;
        let k = 0.5522847498307936;
        let kx = rx * k;
        let ky = ry * k;
        Some(format!(
            "M {} 0 C {} 0 {w} {} {w} {} C {w} {} {} {h} {} {h} C {} {h} 0 {} 0 {} C 0 {} {} 0 {} 0 Z",
            clean_number(rx),
            clean_number(rx + kx),
            clean_number(ry - ky),
            clean_number(ry),
            clean_number(ry + ky),
            clean_number(rx + kx),
            clean_number(rx),
            clean_number(rx - kx),
            clean_number(ry + ky),
            clean_number(ry),
            clean_number(ry - ky),
            clean_number(rx - kx),
            clean_number(rx)
        ))
    } else {
        Some(format!("M 0 0 L {w} 0 L {w} {h} L 0 {h} Z"))
    }
}

fn fill_paint(cells: &HashMap<String, String>) -> Paint {
    if cell_number(cells, "FillPattern", 1.0) == 0.0 {
        return Paint::None;
    }
    cells
        .get("FillForegnd")
        .and_then(|value| parse_visio_color(value))
        .map(Paint::solid)
        .unwrap_or_else(|| Paint::solid("#f3f4f6"))
}

fn line_stroke(cells: &HashMap<String, String>) -> Stroke {
    if cell_number(cells, "LinePattern", 1.0) == 0.0 {
        return Stroke::default();
    }
    let color = cells
        .get("LineColor")
        .and_then(|value| parse_visio_color(value))
        .unwrap_or_else(|| "#334155".into());
    let width = cell_number(cells, "LineWeight", 0.01);
    Stroke {
        paint: Paint::solid(color),
        width: if width.is_finite() && (0.0..=1.0).contains(&width) {
            width
        } else {
            0.01
        },
        line_cap: LineCap::Butt,
        line_join: LineJoin::Miter,
        miter_limit: 4.0,
        ..Stroke::default()
    }
}

fn parse_visio_color(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('#') {
        let color = value.trim_start_matches('#');
        if color.len() == 6 && color.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Some(format!("#{}", color.to_ascii_uppercase()));
        }
    }
    let inside = value
        .strip_prefix("RGB(")
        .or_else(|| value.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let values = inside
        .split(',')
        .map(|value| value.trim().parse::<u8>().ok())
        .collect::<Option<Vec<_>>>()?;
    if values.len() != 3 {
        return None;
    }
    Some(format!(
        "#{:02X}{:02X}{:02X}",
        values[0], values[1], values[2]
    ))
}

fn cell_number(cells: &HashMap<String, String>, name: &str, fallback: f64) -> f64 {
    cells
        .get(name)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(fallback)
}

fn clean_number(value: f64) -> String {
    let value = if value.abs() < 1e-12 { 0.0 } else { value };
    format!("{value:.6}")
}

fn safe_id(value: &str) -> String {
    if value.is_empty() {
        "unnamed".into()
    } else {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                    character
                } else {
                    '_'
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        fallback_shape_path, geometry_path, one_dimensional_path, parse_page, parse_page_refs,
        render_page, shape_transform, wrap_visio_text,
    };

    #[test]
    fn parses_page_dimensions_geometry_styles_and_text() {
        let xml = br##"<PageContents xmlns="urn:visio"><PageSheet><Cell N="PageWidth" V="5"/><Cell N="PageHeight" V="4"/></PageSheet><Shapes><Shape ID="1" NameU="Process" Type="Shape"><Cell N="PinX" V="2.5"/><Cell N="PinY" V="2"/><Cell N="Width" V="2"/><Cell N="Height" V="1"/><Section N="Geometry"><Row T="RelMoveTo"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="0"/></Row><Row T="RelLineTo"><Cell N="X" V="1"/><Cell N="Y" V="1"/></Row></Section><Text>R&amp;D</Text><Section N="Character"><Row IX="0"><Cell N="Color" V="#112233"/><Cell N="Size" V="0.166667"/></Row></Section></Shape></Shapes></PageContents>"##;
        let parsed = parse_page(xml, 1_000, "visio/pages/page1.xml").unwrap();
        assert_eq!((parsed.width_in, parsed.height_in), (5.0, 4.0));
        assert_eq!(parsed.shapes.len(), 1);
        assert_eq!(parsed.shapes[0].text, "R&D");
        assert_eq!(parsed.shapes[0].geometry.len(), 3);
        let page = render_page(parsed, 1, "Main", "0");
        assert_eq!(page.nodes.len(), 2);
        assert_eq!(page.width, 360.0);
        assert_eq!(page.height, 288.0);
        match &page.nodes[1] {
            crate::ir::Node::Text { transform, .. } => assert_eq!(transform[3], 72.0),
            other => panic!("expected editable text node, found {other:?}"),
        }
        let serialized = serde_json::to_string(&page).unwrap();
        assert!(serialized.contains("R&D"));
        assert!(serialized.contains("#112233"));
    }

    #[test]
    fn reads_page_relationship_and_dimensions_from_nested_pages_part_xml() {
        let xml = br#"<Pages xmlns="urn:visio" xmlns:r="urn:relationships"><Page ID="3" NameU="Landscape"><PageSheet><Cell N="PageWidth" V="32"/><Cell N="PageHeight" V="16.5"/></PageSheet><Rel r:id="rId3"/></Page></Pages>"#;
        let pages = parse_page_refs(xml, 1_000).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].relationship_id, "rId3");
        assert_eq!(pages[0].page_sheet_cells.get("PageWidth").unwrap(), "32");
        assert_eq!(pages[0].page_sheet_cells.get("PageHeight").unwrap(), "16.5");

        let backgrounds = br#"<Pages xmlns="urn:visio" xmlns:r="urn:relationships"><Page ID="1" NameU="Background" IsBackground="1"><Rel r:id="rId1"/></Page><Page ID="2" NameU="Foreground" BackPage="1"><Rel r:id="rId2"/></Page></Pages>"#;
        let pages = parse_page_refs(backgrounds, 1_000).unwrap();
        assert!(pages[0].is_background);
        assert!(pages[1].references_background);
    }

    #[test]
    fn transforms_local_coordinates_about_the_visio_pin() {
        let transform = shape_transform(11.0, (4.0, 5.5), (1.0, 0.5), 0.0, (false, false)).unwrap();
        assert_eq!(transform, [72.0, -0.0, -0.0, -72.0, 216.0, 432.0]);
        let rotated = shape_transform(
            11.0,
            (4.0, 5.5),
            (1.0, 0.5),
            std::f64::consts::FRAC_PI_2,
            (false, false),
        )
        .unwrap();
        assert!((rotated[0]).abs() < 1e-10);
        assert!((rotated[1] + 72.0).abs() < 1e-10);
        let flipped = shape_transform(11.0, (4.0, 5.5), (1.0, 0.5), 0.0, (true, false)).unwrap();
        assert_eq!(flipped[0], -72.0);
        assert_eq!(flipped[3], -72.0);
    }

    #[test]
    fn renders_bounded_geometry_rows_and_fallback_shapes() {
        let rows = vec![
            super::GeometryRow {
                kind: "RelMoveTo".into(),
                values: [("X".into(), 0.0), ("Y".into(), 0.0)].into(),
                ..Default::default()
            },
            super::GeometryRow {
                kind: "RelLineTo".into(),
                values: [("X".into(), 1.0), ("Y".into(), 1.0)].into(),
                ..Default::default()
            },
            super::GeometryRow {
                kind: "ArcTo".into(),
                ..Default::default()
            },
        ];
        let mut warning = false;
        assert_eq!(
            geometry_path(&rows, 2.0, 4.0, &mut warning).as_deref(),
            Some("M 0.000000 0.000000 L 2.000000 4.000000")
        );
        assert!(warning);
        assert!(
            fallback_shape_path("Decision", 4.0, 2.0)
                .unwrap()
                .contains("Z")
        );
        assert!(
            fallback_shape_path("Ellipse", 4.0, 2.0)
                .unwrap()
                .contains("C")
        );
        assert!(
            fallback_shape_path("Process", 4.0, 2.0)
                .unwrap()
                .contains("L 4")
        );
        assert!(fallback_shape_path("Process", 0.0, 2.0).is_none());
        let endpoints = [
            ("BeginX".to_owned(), "1".to_owned()),
            ("BeginY".to_owned(), "2".to_owned()),
            ("EndX".to_owned(), "3".to_owned()),
            ("EndY".to_owned(), "4".to_owned()),
        ]
        .into();
        assert_eq!(
            one_dimensional_path(&endpoints).as_deref(),
            Some("M 1.000000 2.000000 L 3.000000 4.000000")
        );
    }

    #[test]
    fn renders_visio_one_dimensional_shape_endpoints_in_page_coordinates() {
        let shape = super::VisioShape {
            id: "connector-1".into(),
            name: "Dynamic connector".into(),
            cells: [
                ("BeginX".into(), "1".into()),
                ("BeginY".into(), "1".into()),
                ("EndX".into(), "4".into()),
                ("EndY".into(), "3".into()),
                ("LineColor".into(), "#334455".into()),
            ]
            .into(),
            text: "Connects".into(),
            ..Default::default()
        };
        let page = render_page(
            super::ParsedPage {
                width_in: 5.0,
                height_in: 4.0,
                has_width: true,
                has_height: true,
                shapes: vec![shape],
                has_foreign_data: false,
            },
            1,
            "Main",
            "0",
        );
        assert_eq!(page.nodes.len(), 2);
        let serialized = serde_json::to_string(&page).unwrap();
        assert!(serialized.contains("office:visio-connector"));
        assert!(serialized.contains("Connects"));
        assert!(serialized.contains("334455"));
    }

    #[test]
    fn wraps_shape_text_to_its_width_and_preserves_cjk_lines() {
        let lines = wrap_visio_text(
            "Architecture review requires careful coordination",
            1.0,
            0.1667,
        );
        assert!(lines.len() > 2, "{lines:?}");
        assert!(lines.iter().all(|line| !line.is_empty()));
        let cjk = wrap_visio_text("文書をページ幅に合わせて折り返す", 1.0, 0.1667);
        assert!(cjk.len() > 2, "{cjk:?}");
        assert_eq!(cjk.concat(), "文書をページ幅に合わせて折り返す");
    }

    #[test]
    fn rejects_page_xml_with_a_doctype() {
        let xml = br#"<!DOCTYPE PageContents [<!ENTITY x "unsafe">]><PageContents/>"#;
        assert!(parse_page(xml, 100, "page1.xml").is_err());
    }

    #[test]
    fn warns_when_a_page_has_no_declared_dimensions() {
        let page = parse_page(b"<PageContents><Shapes/></PageContents>", 100, "page1.xml").unwrap();
        let page = render_page(page, 1, "Page", "0");
        assert!(
            page.warnings
                .iter()
                .any(|warning| warning.contains("dimensions"))
        );
    }
}
