//! OpenDocument Spreadsheet (ODS/OTS/FODS) table preview.
//!
//! Reads cached cell display text, row/column repetitions, sheet order, and
//! bounded package-linked PNG/JPEG images from `content.xml`. It does not
//! recalculate formulas or reproduce spreadsheet styles and drawing geometry.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ooxml::{
    ZipPackage, attribute, decode_xml_reference, local_name, resolve_part_target, sniff_image_mime,
};
use crate::table::{TableAlign, TableData};

const ODS_MIMETYPE_LIMIT: u64 = 256;

pub(crate) fn looks_like_legacy_calc_archive(path: &Path) -> bool {
    let Ok(mut package) = ZipPackage::open(path, ODS_MIMETYPE_LIMIT) else {
        return false;
    };
    let Ok(mimetype) = package.read_limited("mimetype", ODS_MIMETYPE_LIMIT) else {
        return false;
    };
    std::str::from_utf8(&mimetype).ok().is_some_and(|value| {
        matches!(
            value.trim(),
            "application/vnd.sun.xml.calc" | "application/vnd.sun.xml.calc.template"
        )
    })
}
const MAX_ODS_REPEAT: usize = 10_000;
const MAX_ODS_TABLE_CELLS: usize = 200_000;
const MAX_ODS_TABLE_ROWS: usize = 100_000;
const MAX_ODS_XML_DEPTH: usize = 256;
const MAX_ODS_IMAGES: usize = 10_000;
const MAX_ODS_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ODS_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_ODS_TOTAL_DATA_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_ODS_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_ODS_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;

#[derive(Default)]
struct SheetBuilder {
    name: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    total_cells: usize,
    total_rows: usize,
    images: Vec<HtmlBlock>,
}

#[derive(Default)]
struct OdsImageBudget {
    references: usize,
    image_bytes: usize,
    data_uri_bytes: usize,
    pixels: u64,
}

struct PendingImage {
    depth: usize,
    href: Option<String>,
    alt: String,
    has_inline_binary: bool,
    inline_data: Option<String>,
    over_limit: bool,
}

struct OdsImageContext<'a> {
    package: Option<&'a mut ZipPackage<File>>,
    budget: &'a mut OdsImageBudget,
    warnings: &'a mut Vec<String>,
    sheet: &'a mut Option<SheetBuilder>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let (bytes, mut package) = if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fods"))
    {
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        Read::take(&mut file, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > options.max_input_bytes {
            return Err(Error::LimitExceeded(format!(
                "FODS input exceeds maximum bytes ({})",
                options.max_input_bytes
            )));
        }
        (bytes, None)
    } else {
        let mut package = ZipPackage::open(path, options.max_zip_entry_bytes)?;
        let mimetype = package.read_limited("mimetype", ODS_MIMETYPE_LIMIT)?;
        let mimetype = std::str::from_utf8(&mimetype)
            .map_err(|error| Error::InvalidInput(format!("ODS mimetype is not UTF-8: {error}")))?
            .trim();
        if !matches!(
            mimetype,
            "application/vnd.oasis.opendocument.spreadsheet"
                | "application/vnd.oasis.opendocument.spreadsheet-template"
                | "application/vnd.sun.xml.calc"
                | "application/vnd.sun.xml.calc.template"
        ) {
            return Err(Error::InvalidInput(format!(
                "unsupported OpenDocument spreadsheet mimetype '{mimetype}'"
            )));
        }
        let content = package.read("content.xml")?;
        (content, Some(package))
    };

    let xml = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("ODS content is not UTF-8: {error}")))?;
    let (blocks, mut warnings) = parse_content(&xml, options.max_xml_events, package.as_mut())?;
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "OpenDocument Spreadsheet contains no renderable sheets".into(),
        ));
    }
    warnings.insert(
        0,
        "ODS cell styles, number formats, conditional formatting, and print layout are not reproduced".into(),
    );
    render_blocks_to_pages(&blocks, sink, options)?;
    Ok(warnings)
}

fn parse_content(
    xml: &str,
    max_events: usize,
    mut package: Option<&mut ZipPackage<File>>,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut in_spreadsheet = false;
    let mut sheet: Option<SheetBuilder> = None;
    let mut row: Option<Vec<String>> = None;
    let mut row_is_header = false;
    let mut row_repeats = 1usize;
    let mut header_depth = 0usize;
    let mut cell: Option<String> = None;
    let mut cell_fallback = String::new();
    let mut cell_has_text = false;
    let mut cell_repeats = 1usize;
    let mut paragraph: Option<String> = None;
    let mut event_count = 0usize;
    let mut warned_graphics = false;
    let mut warned_formulas = false;
    let mut warned_image_flow = false;
    let mut image_budget = OdsImageBudget::default();
    let mut pending_image: Option<PendingImage> = None;
    let mut frame_alt: Vec<String> = Vec::new();

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "ODS content.xml exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(ref element) => {
                let name =
                    String::from_utf8_lossy(local_name(element.name().as_ref())).into_owned();
                if name == "spreadsheet" && stack.last().is_some_and(|parent| parent == "body") {
                    in_spreadsheet = true;
                }
                if in_spreadsheet {
                    match name.as_str() {
                        "table" => {
                            if sheet.is_some() {
                                return Err(Error::Unsupported(
                                    "nested OpenDocument spreadsheet tables are unsupported".into(),
                                ));
                            }
                            sheet = Some(SheetBuilder {
                                name: attribute(element, b"name")
                                    .unwrap_or_else(|| format!("Sheet {}", blocks.len() + 1)),
                                ..SheetBuilder::default()
                            });
                        }
                        "table-header-rows" => header_depth = header_depth.saturating_add(1),
                        "table-row" => {
                            if row.is_some() {
                                return Err(Error::InvalidInput(
                                    "nested ODS table rows are invalid".into(),
                                ));
                            }
                            row = Some(Vec::new());
                            row_is_header = header_depth > 0;
                            row_repeats = parse_repeat(element, b"number-rows-repeated")?;
                        }
                        "table-cell" => {
                            if cell.is_some() {
                                return Err(Error::InvalidInput(
                                    "nested ODS table cells are invalid".into(),
                                ));
                            }
                            cell = Some(String::new());
                            cell_has_text = false;
                            cell_repeats = parse_repeat(element, b"number-columns-repeated")?;
                            cell_fallback = cached_cell_value(element);
                            if attribute(element, b"formula").is_some() && !warned_formulas {
                                warnings.push(
                                    "ODS formulas are not recalculated; cached cell values are shown".into(),
                                );
                                warned_formulas = true;
                            }
                        }
                        "p" => {
                            if cell.is_some() {
                                paragraph = Some(String::new());
                            }
                        }
                        "frame" => {
                            frame_alt.push(attribute(element, b"name").unwrap_or_default());
                        }
                        "image" => {
                            if pending_image.is_some() {
                                return Err(Error::InvalidInput(
                                    "nested ODS images are invalid".into(),
                                ));
                            }
                            pending_image = Some(PendingImage {
                                depth: stack.len() + 1,
                                href: attribute(element, b"href"),
                                alt: attribute(element, b"name")
                                    .filter(|value| !value.trim().is_empty())
                                    .or_else(|| {
                                        frame_alt
                                            .last()
                                            .filter(|value| !value.trim().is_empty())
                                            .cloned()
                                    })
                                    .unwrap_or_else(|| "Embedded image".into()),
                                has_inline_binary: false,
                                inline_data: None,
                                over_limit: !reserve_ods_image(&mut image_budget, &mut warnings),
                            });
                            if !warned_image_flow {
                                push_ods_warning_once(
                                    &mut warnings,
                                    "ODS package-linked PNG/JPEG images are rendered after the sheet table; cell anchors, frame dimensions, and z-order are approximated",
                                );
                                warned_image_flow = true;
                            }
                        }
                        "binary-data" if pending_image.is_some() => {
                            if let Some(image) = pending_image.as_mut() {
                                image.has_inline_binary = true;
                                image.inline_data = Some(String::new());
                            }
                        }
                        "object" | "chart" | "text-box" | "custom-shape" | "g" | "rect"
                        | "circle" | "ellipse" | "line" | "measure" | "regular-polygon"
                        | "polygon" | "polyline" | "path" | "caption" | "page-thumbnail"
                        | "connector" | "control" | "plugin" | "applet" | "floating-frame" => {
                            if !warned_graphics {
                                warnings.push(
                                    "ODS embedded charts, vector drawings, text boxes, and OLE objects are omitted; package-linked PNG/JPEG images are supported"
                                        .into(),
                                );
                                warned_graphics = true;
                            }
                        }
                        _ => {}
                    }
                }
                if stack.len() >= MAX_ODS_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "ODS XML nesting exceeds {MAX_ODS_XML_DEPTH} elements"
                    )));
                }
                stack.push(name);
            }
            Event::Empty(ref element) => {
                if in_spreadsheet {
                    match local_name(element.name().as_ref()) {
                        b"table-cell" => {
                            let value = cached_cell_value(element);
                            if attribute(element, b"formula").is_some() && !warned_formulas {
                                warnings.push(
                                    "ODS formulas are not recalculated; cached cell values are shown".into(),
                                );
                                warned_formulas = true;
                            }
                            append_cell(
                                value,
                                parse_repeat(element, b"number-columns-repeated")?,
                                &mut row,
                            )?;
                        }
                        b"covered-table-cell" => append_cell(String::new(), 1, &mut row)?,
                        b"image" => {
                            let alt = attribute(element, b"name")
                                .filter(|value| !value.trim().is_empty())
                                .or_else(|| {
                                    frame_alt
                                        .last()
                                        .filter(|value| !value.trim().is_empty())
                                        .cloned()
                                })
                                .unwrap_or_else(|| "Embedded image".into());
                            if !warned_image_flow {
                                push_ods_warning_once(
                                    &mut warnings,
                                    "ODS package-linked PNG/JPEG images are rendered after the sheet table; cell anchors, frame dimensions, and z-order are approximated",
                                );
                                warned_image_flow = true;
                            }
                            if reserve_ods_image(&mut image_budget, &mut warnings) {
                                attach_ods_image(
                                    attribute(element, b"href").as_deref(),
                                    None,
                                    &alt,
                                    OdsImageContext {
                                        package: package.as_deref_mut(),
                                        budget: &mut image_budget,
                                        warnings: &mut warnings,
                                        sheet: &mut sheet,
                                    },
                                )?;
                            }
                        }
                        b"object" | b"chart" | b"text-box" | b"custom-shape" | b"g" | b"rect"
                        | b"circle" | b"ellipse" | b"line" | b"measure" | b"regular-polygon"
                        | b"polygon" | b"polyline" | b"path" | b"caption" | b"page-thumbnail"
                        | b"connector" | b"control" | b"plugin" | b"applet" | b"floating-frame" => {
                            if !warned_graphics {
                                push_ods_warning_once(
                                    &mut warnings,
                                    "ODS embedded charts, vector drawings, text boxes, and OLE objects are omitted; package-linked PNG/JPEG images are supported",
                                );
                                warned_graphics = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::Text(ref value) => {
                if in_spreadsheet && stack.iter().any(|element| element == "binary-data") {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODS inline image encoding: {error}"))
                    })?;
                    if let Some(image) = pending_image.as_mut()
                        && let Some(data) = image.inline_data.as_mut()
                    {
                        if data.len().saturating_add(decoded.len())
                            > (MAX_ODS_IMAGE_BYTES as usize).saturating_mul(2)
                        {
                            return Err(Error::LimitExceeded(
                                "ODS inline image base64 exceeds the per-image limit".into(),
                            ));
                        }
                        data.push_str(&decoded);
                    }
                } else if in_spreadsheet && cell.is_some() {
                    let decoded = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODS text encoding: {error}"))
                    })?;
                    let text = quick_xml::escape::unescape(&decoded).map_err(|error| {
                        Error::InvalidInput(format!("invalid ODS XML text: {error}"))
                    })?;
                    if let Some(paragraph) = paragraph.as_mut() {
                        paragraph.push_str(&text);
                    } else if let Some(cell) = cell.as_mut() {
                        cell.push_str(&text);
                    }
                    cell_has_text = true;
                }
            }
            Event::GeneralRef(ref reference) => {
                if in_spreadsheet
                    && cell.is_some()
                    && !stack.iter().any(|element| element == "binary-data")
                {
                    let text = decode_xml_reference(reference, "ODS cell text")?;
                    if let Some(paragraph) = paragraph.as_mut() {
                        paragraph.push_str(&text);
                    } else if let Some(cell) = cell.as_mut() {
                        cell.push_str(&text);
                    }
                    cell_has_text = true;
                }
            }
            Event::CData(ref value) => {
                if in_spreadsheet && stack.iter().any(|element| element == "binary-data") {
                    let text = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODS inline image encoding: {error}"))
                    })?;
                    if let Some(image) = pending_image.as_mut()
                        && let Some(data) = image.inline_data.as_mut()
                    {
                        if data.len().saturating_add(text.len())
                            > (MAX_ODS_IMAGE_BYTES as usize).saturating_mul(2)
                        {
                            return Err(Error::LimitExceeded(
                                "ODS inline image base64 exceeds the per-image limit".into(),
                            ));
                        }
                        data.push_str(&text);
                    }
                } else if in_spreadsheet && cell.is_some() {
                    let text = value.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid ODS CDATA encoding: {error}"))
                    })?;
                    if let Some(paragraph) = paragraph.as_mut() {
                        paragraph.push_str(&text);
                    } else if let Some(cell) = cell.as_mut() {
                        cell.push_str(&text);
                    }
                    cell_has_text = true;
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "OpenDocument spreadsheet must not contain a document type declaration".into(),
                ));
            }
            Event::End(ref element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                match name {
                    b"image" if pending_image.is_some() => {
                        let pending = pending_image.take().ok_or_else(|| {
                            Error::InvalidInput("ODS image ended without opening".into())
                        })?;
                        if pending.depth != stack.len() {
                            return Err(Error::InvalidInput("mismatched ODS image nesting".into()));
                        }
                        if pending.over_limit {
                            // The common count-limit warning was added when the reference was seen.
                        } else if pending.has_inline_binary {
                            attach_ods_image(
                                pending.href.as_deref(),
                                pending.inline_data.as_deref(),
                                &pending.alt,
                                OdsImageContext {
                                    package: package.as_deref_mut(),
                                    budget: &mut image_budget,
                                    warnings: &mut warnings,
                                    sheet: &mut sheet,
                                },
                            )?;
                        } else {
                            attach_ods_image(
                                pending.href.as_deref(),
                                None,
                                &pending.alt,
                                OdsImageContext {
                                    package: package.as_deref_mut(),
                                    budget: &mut image_budget,
                                    warnings: &mut warnings,
                                    sheet: &mut sheet,
                                },
                            )?;
                        }
                    }
                    b"frame" => {
                        frame_alt.pop();
                    }
                    b"p" => {
                        if let Some(text) = paragraph.take()
                            && let Some(cell) = cell.as_mut()
                        {
                            if !cell.is_empty() {
                                cell.push('\n');
                            }
                            cell.push_str(text.trim());
                        }
                    }
                    b"table-cell" => {
                        let text = cell.take().ok_or_else(|| {
                            Error::InvalidInput("ODS table cell ended without opening".into())
                        })?;
                        let text_value = text.trim();
                        let value = if cell_has_text && !text_value.is_empty() {
                            text_value.to_owned()
                        } else {
                            cell_fallback.clone()
                        };
                        append_cell(value, cell_repeats, &mut row)?;
                        cell_fallback.clear();
                        cell_has_text = false;
                        cell_repeats = 1;
                    }
                    b"covered-table-cell" => append_cell(String::new(), 1, &mut row)?,
                    b"table-row" => {
                        let values = row.take().ok_or_else(|| {
                            Error::InvalidInput("ODS table row ended without opening".into())
                        })?;
                        append_row(values, row_repeats, row_is_header, &mut sheet)?;
                        row_repeats = 1;
                        row_is_header = false;
                    }
                    b"table-header-rows" => header_depth = header_depth.saturating_sub(1),
                    b"table" => {
                        let builder = sheet.take().ok_or_else(|| {
                            Error::InvalidInput("ODS table ended without opening".into())
                        })?;
                        let mut headers = builder.headers;
                        let mut rows = builder.rows;
                        if headers.is_empty() && !rows.is_empty() {
                            headers = rows.remove(0);
                        }
                        let column_count = headers
                            .len()
                            .max(rows.iter().map(Vec::len).max().unwrap_or(0));
                        let has_table = column_count > 0;
                        if has_table {
                            blocks.push(HtmlBlock::Heading {
                                level: 2,
                                text: builder.name.clone(),
                            });
                            blocks.push(HtmlBlock::Table(TableData {
                                headers,
                                rows,
                                alignments: vec![TableAlign::Left; column_count],
                                raw_source: String::new(),
                            }));
                        }
                        if !builder.images.is_empty() {
                            if !has_table {
                                blocks.push(HtmlBlock::Heading {
                                    level: 2,
                                    text: builder.name,
                                });
                            }
                            blocks.extend(builder.images);
                        }
                    }
                    b"spreadsheet" => in_spreadsheet = false,
                    _ => {}
                }
                let closing = String::from_utf8_lossy(name).into_owned();
                if stack.pop().as_deref() != Some(closing.as_str()) {
                    return Err(Error::InvalidInput(format!(
                        "mismatched ODS XML end tag '{closing}'"
                    )));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if sheet.is_some() || row.is_some() || cell.is_some() || paragraph.is_some() {
        return Err(Error::InvalidInput(
            "incomplete ODS spreadsheet XML structure".into(),
        ));
    }
    Ok((blocks, warnings))
}

fn attach_ods_image(
    href: Option<&str>,
    inline_data: Option<&str>,
    alt: &str,
    context: OdsImageContext<'_>,
) -> Result<()> {
    let OdsImageContext {
        package,
        budget,
        warnings,
        sheet,
    } = context;
    if sheet.is_none() {
        push_ods_warning_once(warnings, "ODS image outside a supported sheet was omitted");
        return Ok(());
    }
    if let Some(inline_data) = inline_data {
        return attach_ods_inline_image(inline_data, alt, warnings, budget, sheet);
    }
    let Some(package) = package else {
        push_ods_warning_once(
            warnings,
            "flat OpenDocument images and external resources are omitted; package-linked PNG/JPEG images are supported",
        );
        return Ok(());
    };
    let Some(href) = href.filter(|href| !href.trim().is_empty()) else {
        push_ods_warning_once(warnings, "ODS image without a package href was omitted");
        return Ok(());
    };
    if href.contains(':') || href.starts_with("//") || href.starts_with('\\') {
        push_ods_warning_once(warnings, "external ODS image resources are not fetched");
        return Ok(());
    }
    let package_href = href.strip_prefix('/').unwrap_or(href);
    let target = match resolve_part_target("content.xml", package_href) {
        Ok(target) => target,
        Err(_) => {
            push_ods_warning_once(
                warnings,
                "ODS image path escaped the package or was invalid and was omitted",
            );
            return Ok(());
        }
    };
    let bytes = match package.read_optional_limited(&target, MAX_ODS_IMAGE_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            push_ods_warning_once(warnings, "missing ODS image parts were omitted");
            return Ok(());
        }
        Err(Error::LimitExceeded(_)) => {
            push_ods_warning_once(
                warnings,
                "ODS image parts exceeding the per-image byte limit were omitted",
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let Some(mime) =
        sniff_image_mime(&bytes).filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
    else {
        push_ods_warning_once(
            warnings,
            "unsupported ODS image types were omitted; only PNG and JPEG are embedded",
        );
        return Ok(());
    };
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_ods_warning_once(warnings, "invalid ODS PNG/JPEG images were omitted");
        return Ok(());
    };
    let pixels = u64::from(width) * u64::from(height);
    let next_pixels = budget.pixels.saturating_add(pixels);
    if width == 0
        || height == 0
        || pixels > MAX_ODS_IMAGE_PIXELS
        || next_pixels > MAX_ODS_TOTAL_IMAGE_PIXELS
    {
        push_ods_warning_once(
            warnings,
            "ODS images exceeding the per-image or total pixel limit were omitted",
        );
        return Ok(());
    }
    let next_image_bytes = budget.image_bytes.saturating_add(bytes.len());
    if next_image_bytes > MAX_ODS_TOTAL_IMAGE_BYTES {
        push_ods_warning_once(
            warnings,
            "ODS images exceeding the total image byte limit were omitted",
        );
        return Ok(());
    }
    let prefix = format!("data:{mime};base64,");
    let uri_bytes = prefix
        .len()
        .saturating_add(bytes.len().div_ceil(3).saturating_mul(4));
    let next_uri_bytes = budget.data_uri_bytes.saturating_add(uri_bytes);
    if next_uri_bytes > MAX_ODS_TOTAL_DATA_URI_BYTES {
        push_ods_warning_once(
            warnings,
            "ODS images exceeding the total data URI byte limit were omitted",
        );
        return Ok(());
    }
    let image = HtmlBlock::Image {
        href: format!("{prefix}{}", BASE64_STANDARD.encode(&bytes)),
        pixel_width: width,
        pixel_height: height,
        alt: alt.to_owned(),
    };
    if let Some(sheet) = sheet.as_mut() {
        sheet.images.push(image);
    } else {
        push_ods_warning_once(warnings, "ODS image outside a supported sheet was omitted");
        return Ok(());
    }
    budget.image_bytes = next_image_bytes;
    budget.data_uri_bytes = next_uri_bytes;
    budget.pixels = next_pixels;
    Ok(())
}

fn attach_ods_inline_image(
    data: &str,
    alt: &str,
    warnings: &mut Vec<String>,
    budget: &mut OdsImageBudget,
    sheet: &mut Option<SheetBuilder>,
) -> Result<()> {
    let compact: String = data
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    let bytes = match BASE64_STANDARD.decode(compact.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => {
            push_ods_warning_once(
                warnings,
                "malformed ODS inline office:binary-data image was omitted",
            );
            return Ok(());
        }
    };
    if bytes.len() as u64 > MAX_ODS_IMAGE_BYTES {
        push_ods_warning_once(
            warnings,
            "ODS inline image exceeded the per-image byte limit",
        );
        return Ok(());
    }
    let Some(mime) =
        sniff_image_mime(&bytes).filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
    else {
        push_ods_warning_once(
            warnings,
            "unsupported ODS inline image type was omitted; only PNG and JPEG are embedded",
        );
        return Ok(());
    };
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_ods_warning_once(warnings, "invalid ODS inline PNG/JPEG image was omitted");
        return Ok(());
    };
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    let next_pixels = budget.pixels.saturating_add(pixels);
    if width == 0
        || height == 0
        || pixels > MAX_ODS_IMAGE_PIXELS
        || next_pixels > MAX_ODS_TOTAL_IMAGE_PIXELS
    {
        push_ods_warning_once(warnings, "ODS inline images exceeded the pixel limit");
        return Ok(());
    }
    let next_bytes = budget.image_bytes.saturating_add(bytes.len());
    if next_bytes > MAX_ODS_TOTAL_IMAGE_BYTES {
        push_ods_warning_once(warnings, "ODS inline images exceeded the total byte limit");
        return Ok(());
    }
    let prefix = format!("data:{mime};base64,");
    let next_uri = budget
        .data_uri_bytes
        .saturating_add(prefix.len() + bytes.len().div_ceil(3) * 4);
    if next_uri > MAX_ODS_TOTAL_DATA_URI_BYTES {
        push_ods_warning_once(
            warnings,
            "ODS inline images exceeded the total data URI limit",
        );
        return Ok(());
    }
    if let Some(sheet) = sheet.as_mut() {
        sheet.images.push(HtmlBlock::Image {
            href: format!("{prefix}{}", BASE64_STANDARD.encode(&bytes)),
            pixel_width: width,
            pixel_height: height,
            alt: alt.to_owned(),
        });
    } else {
        push_ods_warning_once(
            warnings,
            "ODS inline image outside a supported sheet was omitted",
        );
        return Ok(());
    }
    budget.image_bytes = next_bytes;
    budget.data_uri_bytes = next_uri;
    budget.pixels = next_pixels;
    Ok(())
}

fn reserve_ods_image(budget: &mut OdsImageBudget, warnings: &mut Vec<String>) -> bool {
    if budget.references >= MAX_ODS_IMAGES {
        push_ods_warning_once(
            warnings,
            "ODS image count exceeded the supported limit; remaining images were omitted",
        );
        false
    } else {
        budget.references += 1;
        true
    }
}

fn push_ods_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn cached_cell_value(element: &BytesStart<'_>) -> String {
    let value_type = attribute(element, b"value-type").unwrap_or_default();
    let key = match value_type.as_str() {
        "string" => b"string-value" as &[u8],
        "date" => b"date-value",
        "time" => b"time-value",
        "boolean" => b"boolean-value",
        _ => b"value",
    };
    attribute(element, key).unwrap_or_default()
}

fn parse_repeat(element: &BytesStart<'_>, name: &[u8]) -> Result<usize> {
    let count = attribute(element, name)
        .map(|value| {
            value.parse::<usize>().map_err(|_| {
                Error::InvalidInput(format!(
                    "invalid ODS repeat count for '{}'",
                    String::from_utf8_lossy(name)
                ))
            })
        })
        .transpose()?
        .unwrap_or(1);
    if count == 0 || count > MAX_ODS_REPEAT {
        return Err(Error::LimitExceeded(format!(
            "ODS repeat count must be between 1 and {MAX_ODS_REPEAT}"
        )));
    }
    Ok(count)
}

fn append_cell(value: String, repeats: usize, row: &mut Option<Vec<String>>) -> Result<()> {
    let row = row
        .as_mut()
        .ok_or_else(|| Error::InvalidInput("ODS cell appears outside a table row".into()))?;
    if row.len().saturating_add(repeats) > MAX_ODS_TABLE_CELLS {
        return Err(Error::LimitExceeded(format!(
            "ODS table row exceeds {MAX_ODS_TABLE_CELLS} cells"
        )));
    }
    row.extend(std::iter::repeat_n(value, repeats));
    Ok(())
}

fn append_row(
    values: Vec<String>,
    repeats: usize,
    is_header: bool,
    sheet: &mut Option<SheetBuilder>,
) -> Result<()> {
    let sheet = sheet
        .as_mut()
        .ok_or_else(|| Error::InvalidInput("ODS row appears outside a table".into()))?;
    let added_cells = values
        .len()
        .checked_mul(repeats)
        .ok_or_else(|| Error::LimitExceeded("ODS table cell count overflowed".into()))?;
    sheet.total_cells = sheet
        .total_cells
        .checked_add(added_cells)
        .ok_or_else(|| Error::LimitExceeded("ODS table cell count overflowed".into()))?;
    if sheet.total_cells > MAX_ODS_TABLE_CELLS {
        return Err(Error::LimitExceeded(format!(
            "ODS sheet exceeds {MAX_ODS_TABLE_CELLS} cells"
        )));
    }
    sheet.total_rows = sheet
        .total_rows
        .checked_add(repeats)
        .ok_or_else(|| Error::LimitExceeded("ODS table row count overflowed".into()))?;
    if sheet.total_rows > MAX_ODS_TABLE_ROWS {
        return Err(Error::LimitExceeded(format!(
            "ODS sheet exceeds {MAX_ODS_TABLE_ROWS} rows"
        )));
    }
    for _ in 0..repeats {
        if is_header && sheet.headers.is_empty() {
            sheet.headers = values.clone();
        } else {
            sheet.rows.push(values.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dtd_and_unbounded_spreadsheet_repetition() {
        let doctype = r#"<!DOCTYPE office:document-content [<!ENTITY x "expanded">]><office:document-content/>"#;
        assert!(matches!(
            parse_content(doctype, 100, None),
            Err(Error::InvalidInput(_))
        ));

        let repeated = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0"><office:body><office:spreadsheet><table:table><table:table-row><table:table-cell table:number-columns-repeated="10001"/></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#;
        assert!(matches!(
            parse_content(repeated, 1_000, None),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn inline_binary_image_payload_is_omitted_without_becoming_a_cell_value() {
        let xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:spreadsheet><table:table table:name="Sheet"><table:table-row><table:table-cell><text:p>Cell value</text:p><draw:frame draw:name="inline"><draw:image xlink:href="Pictures/fallback.png"><office:binary-data>BASE64_SECRET</office:binary-data></draw:image></draw:frame></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#;
        let (blocks, warnings) = parse_content(xml, 1_000, None).unwrap();
        let rendered = format!("{blocks:?}");

        assert!(rendered.contains("Cell value"));
        assert!(!rendered.contains("BASE64_SECRET"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("office:binary-data"))
        );
    }

    #[test]
    fn valid_inline_binary_png_is_embedded_in_the_sheet_preview() {
        let xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:spreadsheet><table:table table:name="Sheet"><table:table-row><table:table-cell><text:p>Cell</text:p><draw:frame draw:name="inline"><draw:image xlink:href="fallback.png"><office:binary-data>iVBORw0KGgoAAAANSUhEUgAAACAAAAAQCAIAAAD4YuoOAAAAIklEQVR4nGP4z8BAEiJR+X9SlY9aMGrBqAWjFoxaMCAWAABQpv4QX+h4RQAAAABJRU5ErkJggg==</office:binary-data></draw:image></draw:frame></table:table-cell></table:table-row></table:table></office:spreadsheet></office:body></office:document-content>"#;
        let (blocks, warnings) = parse_content(xml, 2_000, None).unwrap();
        let rendered = format!("{blocks:?}");
        assert!(rendered.contains("data:image/png;base64,"));
        assert!(
            blocks
                .iter()
                .any(|block| matches!(block, HtmlBlock::Image { .. }))
        );
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("binary-data images are omitted"))
        );
    }

    #[test]
    fn ods_image_reference_count_is_bounded() {
        let mut budget = OdsImageBudget::default();
        let mut warnings = Vec::new();
        for _ in 0..MAX_ODS_IMAGES {
            assert!(reserve_ods_image(&mut budget, &mut warnings));
        }
        assert!(!reserve_ods_image(&mut budget, &mut warnings));
        assert_eq!(budget.references, MAX_ODS_IMAGES);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("image count exceeded"))
        );
    }
}
