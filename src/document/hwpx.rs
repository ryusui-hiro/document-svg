//! Bounded Hancom HWPX (HWPML) package preview.
//!
//! HWPX is an OPC-like ZIP package containing HWPML section XML and optional
//! BinData resources. This reader renders section paragraphs, simple tables,
//! and package-local PNG/JPEG images. It never follows external references,
//! executes macros, or interprets embedded controls.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use quick_xml::Reader;
use quick_xml::events::Event;
use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, InlineHtmlImage, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};
use crate::ooxml::{attribute, local_name, sniff_image_mime};
use crate::table::{TableAlign, TableData};

const MAX_HWPX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_HWPX_ENTRIES: usize = 100_000;
const MAX_HWPX_PART_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HWPX_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_HWPX_SECTIONS: usize = 10_000;
const MAX_HWPX_XML_EVENTS: usize = 1_000_000;
const MAX_HWPX_XML_DEPTH: usize = 256;
const MAX_HWPX_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_HWPX_TABLE_CELLS: usize = 200_000;
const MAX_HWPX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_HWPX_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_HWPX_TOTAL_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_HWPX_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_HWPX_TOTAL_PIXELS: u64 = 100_000_000;

#[derive(Default)]
struct ImageBudget {
    bytes: usize,
    uri_bytes: usize,
    pixels: u64,
}

#[derive(Default)]
struct HwpxTable {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_cell: bool,
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn looks_like_hwpx_archive(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = ZipArchive::new(file) else {
        return false;
    };
    (0..archive.len()).take(MAX_HWPX_ENTRIES).any(|index| {
        archive
            .by_index(index)
            .ok()
            .map(|entry| {
                entry.name().starts_with("Contents/section")
                    && entry.name().to_ascii_lowercase().ends_with(".xml")
            })
            .unwrap_or(false)
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_HWPX_INPUT_BYTES),
        "HWPX input",
    )?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("invalid HWPX ZIP package: {error}")))?;
    if archive.len() > MAX_HWPX_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "HWPX package contains more than {MAX_HWPX_ENTRIES} entries"
        )));
    }
    let mut section_names = Vec::new();
    let mut image_entries = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        validate_entry_name(&name)?;
        if name.starts_with("Contents/section") && name.to_ascii_lowercase().ends_with(".xml") {
            section_names.push(name.clone());
        }
        if name.to_ascii_lowercase().starts_with("bindata/") && !entry.is_dir() {
            image_entries.push(name);
        }
    }
    if section_names.is_empty() {
        return Err(Error::InvalidInput(
            "HWPX package contains no Contents/section*.xml parts".into(),
        ));
    }
    if section_names.len() > MAX_HWPX_SECTIONS {
        return Err(Error::LimitExceeded(format!(
            "HWPX package contains more than {MAX_HWPX_SECTIONS} sections"
        )));
    }
    section_names.sort_by_key(|name| natural_key(name));
    image_entries.sort_by_key(|name| name.to_ascii_lowercase());
    let (images, mut warnings) = load_images(&mut archive, &image_entries)?;
    let mut expanded_bytes = 0u64;
    let mut blocks = Vec::new();
    for section_name in section_names {
        let xml = read_zip_part(&mut archive, &section_name)?;
        expanded_bytes = expanded_bytes.saturating_add(xml.len() as u64);
        if expanded_bytes > MAX_HWPX_EXPANDED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "HWPX expanded parts exceed {MAX_HWPX_EXPANDED_BYTES} bytes"
            )));
        }
        let (section_blocks, section_warnings) =
            parse_section(&xml, &images, options.max_xml_events)?;
        blocks.extend(section_blocks);
        warnings.extend(section_warnings);
    }
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "HWPX sections contain no renderable text, table, or image content".into(),
        ));
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(dedup_warnings(warnings))
}

fn parse_section(
    xml: &[u8],
    images: &HashMap<String, InlineHtmlImage>,
    max_events: usize,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut reader = Reader::from_reader(Cursor::new(xml));
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<Vec<u8>>::new();
    let mut paragraph = None::<String>;
    let mut table = None::<HwpxTable>;
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut text_bytes = 0usize;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events.min(MAX_HWPX_XML_EVENTS) {
            return Err(Error::LimitExceeded(format!(
                "HWPX section exceeds {} parser events",
                max_events.min(MAX_HWPX_XML_EVENTS)
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref()).to_vec();
                if stack.len() >= MAX_HWPX_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "HWPX XML nesting exceeds {MAX_HWPX_XML_DEPTH}"
                    )));
                }
                match name.as_slice() {
                    b"p" if table.is_none() => paragraph = Some(String::new()),
                    b"tbl" => table = Some(HwpxTable::default()),
                    b"tr" => {
                        if let Some(table) = table.as_mut() {
                            table.row.clear();
                        }
                    }
                    b"tc" => {
                        if let Some(table) = table.as_mut() {
                            table.cell.clear();
                            table.in_cell = true;
                        }
                    }
                    b"img" | b"image" => {
                        append_image(
                            &mut blocks,
                            &mut warnings,
                            images,
                            attribute(&element, b"binaryItemIDRef"),
                        );
                    }
                    b"ole" | b"ctrl" | b"script" => push_warning_once(
                        &mut warnings,
                        "HWPX controls, OLE objects, and scripts were omitted",
                    ),
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if name == b"img" || name == b"image" {
                    append_image(
                        &mut blocks,
                        &mut warnings,
                        images,
                        attribute(&element, b"binaryItemIDRef"),
                    );
                }
            }
            Event::Text(text) => {
                let value = text
                    .decode()
                    .map_err(|error| Error::InvalidInput(format!("invalid HWPX text: {error}")))?;
                append_text(&value, &mut paragraph, &mut table, &mut text_bytes)?;
            }
            Event::CData(text) => {
                let value = text
                    .decode()
                    .map_err(|error| Error::InvalidInput(format!("invalid HWPX CDATA: {error}")))?;
                append_text(&value, &mut paragraph, &mut table, &mut text_bytes)?;
            }
            Event::End(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                match name {
                    b"p" => {
                        if let Some(text) = paragraph.take() {
                            let text = clean_text(&text);
                            if !text.is_empty() {
                                blocks.push(HtmlBlock::Paragraph { text });
                            }
                        }
                    }
                    b"tc" => {
                        if let Some(table) = table.as_mut()
                            && table.in_cell
                        {
                            table.row.push(clean_text(&table.cell));
                            table.cell.clear();
                            table.in_cell = false;
                        }
                    }
                    b"tr" => {
                        if let Some(table) = table.as_mut()
                            && !table.row.is_empty()
                        {
                            table.rows.push(std::mem::take(&mut table.row));
                            if table.rows.iter().map(Vec::len).sum::<usize>() > MAX_HWPX_TABLE_CELLS
                            {
                                return Err(Error::LimitExceeded(format!(
                                    "HWPX table exceeds {MAX_HWPX_TABLE_CELLS} cells"
                                )));
                            }
                        }
                    }
                    b"tbl" => {
                        if let Some(table) = table.take() {
                            let table = finish_table(table);
                            if !table.headers.is_empty() || !table.rows.is_empty() {
                                blocks.push(HtmlBlock::Table(table));
                            }
                        }
                    }
                    _ => {}
                }
                stack.pop();
            }
            Event::DocType(_) => push_warning_once(
                &mut warnings,
                "HWPX document type declarations were ignored; external entities were not loaded",
            ),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((blocks, warnings))
}

fn load_images(
    archive: &mut ZipArchive<Cursor<Vec<u8>>>,
    entries: &[String],
) -> Result<(HashMap<String, InlineHtmlImage>, Vec<String>)> {
    let mut images = HashMap::new();
    let mut warnings = Vec::new();
    let mut budget = ImageBudget::default();
    for entry_name in entries {
        let bytes = read_zip_part(archive, entry_name)?;
        if bytes.len() > MAX_HWPX_IMAGE_BYTES {
            push_warning_once(
                &mut warnings,
                "HWPX images exceeding the per-image byte limit were omitted",
            );
            continue;
        }
        let Some(mime) =
            sniff_image_mime(&bytes).filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
        else {
            push_warning_once(
                &mut warnings,
                "unsupported HWPX BinData images were omitted; only PNG/JPEG are embedded",
            );
            continue;
        };
        let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
            push_warning_once(
                &mut warnings,
                "invalid HWPX PNG/JPEG BinData images were omitted",
            );
            continue;
        };
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if pixels == 0 || pixels > MAX_HWPX_IMAGE_PIXELS {
            push_warning_once(
                &mut warnings,
                "HWPX images exceeding the pixel limit were omitted",
            );
            continue;
        }
        let uri = format!("data:{mime};base64,{}", BASE64_STANDARD.encode(&bytes));
        let next_bytes = budget.bytes.saturating_add(bytes.len());
        let next_uri = budget.uri_bytes.saturating_add(uri.len());
        let next_pixels = budget.pixels.saturating_add(pixels);
        if next_bytes > MAX_HWPX_TOTAL_IMAGE_BYTES
            || next_uri > MAX_HWPX_TOTAL_URI_BYTES
            || next_pixels > MAX_HWPX_TOTAL_PIXELS
        {
            push_warning_once(
                &mut warnings,
                "HWPX total image budget was exceeded; remaining images were omitted",
            );
            continue;
        }
        budget.bytes = next_bytes;
        budget.uri_bytes = next_uri;
        budget.pixels = next_pixels;
        let key = entry_name
            .rsplit('/')
            .next()
            .unwrap_or(entry_name)
            .trim_end_matches(['.', ' '])
            .to_ascii_lowercase();
        let image = InlineHtmlImage {
            href: uri,
            pixel_width: width,
            pixel_height: height,
        };
        images.insert(key.clone(), image.clone());
        if let Some(stem) = key.rsplit_once('.').map(|(stem, _)| stem.to_owned()) {
            images.insert(stem, image);
        }
    }
    Ok((images, warnings))
}

fn append_image(
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    images: &HashMap<String, InlineHtmlImage>,
    reference: Option<String>,
) {
    let Some(reference) = reference else {
        push_warning_once(warnings, "HWPX image without binaryItemIDRef was omitted");
        return;
    };
    let key = reference
        .rsplit('/')
        .next()
        .unwrap_or(&reference)
        .to_ascii_lowercase();
    if let Some(image) = images.get(&key) {
        blocks.push(HtmlBlock::Image {
            href: image.href.clone(),
            pixel_width: image.pixel_width,
            pixel_height: image.pixel_height,
            alt: "HWPX image".into(),
        });
    } else {
        push_warning_once(
            warnings,
            "HWPX image BinData reference was missing or unsupported",
        );
    }
}

fn read_zip_part(archive: &mut ZipArchive<Cursor<Vec<u8>>>, name: &str) -> Result<Vec<u8>> {
    let mut entry = archive.by_name(name)?;
    if entry.is_dir() || entry.size() > MAX_HWPX_PART_BYTES {
        return Err(Error::LimitExceeded(format!(
            "HWPX part '{name}' exceeds {MAX_HWPX_PART_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut entry)
        .take(MAX_HWPX_PART_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_HWPX_PART_BYTES {
        return Err(Error::LimitExceeded(format!(
            "HWPX part '{name}' exceeds {MAX_HWPX_PART_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

fn validate_entry_name(name: &str) -> Result<()> {
    if name.starts_with('/') || name.contains('\\') || name.split('/').any(|part| part == "..") {
        return Err(Error::InvalidInput(format!(
            "HWPX package contains an unsafe entry name: {name}"
        )));
    }
    Ok(())
}

fn append_text(
    value: &str,
    paragraph: &mut Option<String>,
    table: &mut Option<HwpxTable>,
    text_bytes: &mut usize,
) -> Result<()> {
    *text_bytes = text_bytes.saturating_add(value.len());
    if *text_bytes > MAX_HWPX_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "HWPX text exceeds {MAX_HWPX_TEXT_BYTES} bytes"
        )));
    }
    if let Some(table) = table.as_mut()
        && table.in_cell
    {
        table.cell.push_str(value);
    } else if let Some(paragraph) = paragraph.as_mut() {
        paragraph.push_str(value);
    }
    Ok(())
}

fn finish_table(table: HwpxTable) -> TableData {
    let mut rows = table.rows;
    let headers = rows.first().cloned().unwrap_or_default();
    if !rows.is_empty() {
        rows.remove(0);
    }
    let columns = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
    let mut headers = headers;
    headers.resize(columns, String::new());
    for row in &mut rows {
        row.resize(columns, String::new());
    }
    TableData {
        headers,
        rows,
        alignments: vec![TableAlign::Left; columns],
        raw_source: String::new(),
    }
}

fn natural_key(name: &str) -> (usize, String) {
    let lower = name.to_ascii_lowercase();
    let digits = lower
        .strip_prefix("contents/section")
        .and_then(|value| value.strip_suffix(".xml"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    (digits, lower)
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn dedup_warnings(mut warnings: Vec<String>) -> Vec<String> {
    warnings.sort();
    warnings.dedup();
    warnings
}
