//! Bounded FictionBook 2 (`.fb2`) e-book preview.
//!
//! FictionBook is one XML document containing a description, one or more
//! bodies, and optional base64 `<binary>` resources. This reader renders the
//! first main body as safe flowing SVG blocks and resolves only validated
//! embedded PNG/JPEG images. It never follows links, evaluates markup, or
//! executes any book content.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, InlineHtmlImage, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};
use crate::ooxml::{local_name, sniff_image_mime};
use crate::table::{TableAlign, TableData};

const MAX_FB2_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FB2_XML_DEPTH: usize = 256;
const MAX_FB2_BINARY_BYTES: usize = 8 * 1024 * 1024;
const MAX_FB2_TOTAL_BINARY_BYTES: usize = 32 * 1024 * 1024;
const MAX_FB2_TOTAL_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_FB2_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_FB2_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;
const MAX_FB2_IMAGES: usize = 10_000;
const MAX_FB2_TEXT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct ImageBudget {
    decoded_bytes: usize,
    uri_bytes: usize,
    pixels: u64,
    count: usize,
}

#[derive(Default)]
struct BinaryBuilder {
    id: String,
    content_type: String,
    text: String,
}

#[derive(Default)]
struct Metadata {
    title: String,
    author: String,
}

#[derive(Default)]
struct TableBuilder {
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    in_cell: bool,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.contains("<FictionBook")
        || trimmed.contains("<fictionbook")
        || (trimmed.contains("FictionBook") && trimmed.contains("fictionbook/2.0"))
        || trimmed.contains("http://www.gribuser.ru/xml/fictionbook/2.0")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".fb2.zip"))
    {
        let metadata = std::fs::metadata(path)?;
        let input_limit = options.max_input_bytes.min(MAX_FB2_INPUT_BYTES);
        if metadata.len() > input_limit {
            return Err(Error::LimitExceeded(format!(
                "FictionBook ZIP input exceeds maximum bytes ({input_limit})"
            )));
        }
        let mut archive =
            zip::ZipArchive::new(BufReader::new(File::open(path)?)).map_err(|error| {
                Error::InvalidInput(format!("invalid FictionBook ZIP archive: {error}"))
            })?;
        if archive.len() > 10_000 {
            return Err(Error::LimitExceeded(
                "FictionBook ZIP archive contains more than 10,000 entries".into(),
            ));
        }
        let mut selected = None;
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            let name = entry.name();
            if name.len() > 4096
                || name.starts_with('/')
                || name.contains('\\')
                || name.split('/').any(|part| part == "..")
            {
                return Err(Error::InvalidInput(
                    "FictionBook ZIP contains an unsafe entry name".into(),
                ));
            }
            if !entry.is_dir() && name.to_ascii_lowercase().ends_with(".fb2") {
                if selected.is_some() {
                    return Err(Error::InvalidInput(
                        "FictionBook ZIP must contain exactly one FB2 document".into(),
                    ));
                }
                selected = Some(index);
            }
        }
        let index = selected.ok_or_else(|| {
            Error::InvalidInput("FictionBook ZIP contains no .fb2 document".into())
        })?;
        let mut entry = archive.by_index(index)?;
        if entry.size() > input_limit {
            return Err(Error::LimitExceeded(format!(
                "FictionBook ZIP document exceeds maximum bytes ({input_limit})"
            )));
        }
        let mut bytes = Vec::new();
        Read::take(&mut entry, input_limit.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > input_limit {
            return Err(Error::LimitExceeded(format!(
                "FictionBook ZIP document exceeds maximum bytes ({input_limit})"
            )));
        }
        let xml = String::from_utf8(bytes).map_err(|error| {
            Error::InvalidInput(format!("FictionBook ZIP document is not UTF-8: {error}"))
        })?;
        return convert_source(&xml, options, sink);
    }
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_FB2_INPUT_BYTES),
        "FictionBook input",
    )?;
    let xml = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("FictionBook input is not UTF-8: {error}")))?;
    convert_source(&xml, options, sink)
}

fn convert_source(
    xml: &str,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let (images, mut warnings) = collect_binaries(xml, options.max_xml_events)?;
    let (blocks, parser_warnings) = parse_body(xml, &images, options.max_xml_events)?;
    warnings.extend(parser_warnings);
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "FictionBook contains no renderable main-body content".into(),
        ));
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn collect_binaries(
    xml: &str,
    max_events: usize,
) -> Result<(HashMap<String, InlineHtmlImage>, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut current = None::<BinaryBuilder>;
    let mut depth = 0usize;
    let mut events = 0usize;
    let mut budget = ImageBudget::default();
    let mut images = HashMap::new();
    let mut warnings = Vec::new();
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "FictionBook XML exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"binary" {
                    let id = attribute(&element, b"id").unwrap_or_default();
                    let content_type = attribute(&element, b"content-type").unwrap_or_default();
                    if id.len() > 4096 || id.is_empty() {
                        return Err(Error::InvalidInput(
                            "FictionBook binary id is missing or overlong".into(),
                        ));
                    }
                    current = Some(BinaryBuilder {
                        id,
                        content_type,
                        text: String::new(),
                    });
                }
                depth = depth.saturating_add(1);
                if depth > MAX_FB2_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "FictionBook XML nesting exceeds {MAX_FB2_XML_DEPTH}"
                    )));
                }
            }
            Event::Empty(element) => {
                if local_name(element.name().as_ref()) == b"binary" {
                    push_warning_once(
                        &mut warnings,
                        "empty FictionBook binary resources were omitted",
                    );
                }
            }
            Event::Text(text) => {
                if let Some(binary) = current.as_mut() {
                    let value = String::from_utf8_lossy(text.as_ref());
                    if binary.text.len().saturating_add(value.len()) > MAX_FB2_TOTAL_BINARY_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "FictionBook binary data exceeds {MAX_FB2_TOTAL_BINARY_BYTES} bytes"
                        )));
                    }
                    binary.text.push_str(&value);
                }
            }
            Event::CData(text) => {
                if let Some(binary) = current.as_mut() {
                    let value = String::from_utf8_lossy(text.as_ref());
                    if binary.text.len().saturating_add(value.len()) > MAX_FB2_TOTAL_BINARY_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "FictionBook binary data exceeds {MAX_FB2_TOTAL_BINARY_BYTES} bytes"
                        )));
                    }
                    binary.text.push_str(&value);
                }
            }
            Event::End(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"binary"
                    && let Some(binary) = current.take()
                    && let Some(image) = decode_binary(&binary, &mut budget, &mut warnings)?
                {
                    images.insert(binary.id, image);
                }
                depth = depth.saturating_sub(1);
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "FictionBook document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((images, warnings))
}

fn decode_binary(
    binary: &BinaryBuilder,
    budget: &mut ImageBudget,
    warnings: &mut Vec<String>,
) -> Result<Option<InlineHtmlImage>> {
    let compact: String = binary
        .text
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    if compact.len() > MAX_FB2_BINARY_BYTES.saturating_mul(2) {
        push_warning_once(
            warnings,
            "FictionBook binary image exceeded the base64 size limit",
        );
        return Ok(None);
    }
    let bytes = match BASE64_STANDARD.decode(compact.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => {
            push_warning_once(
                warnings,
                "malformed FictionBook binary resources were omitted",
            );
            return Ok(None);
        }
    };
    if bytes.len() > MAX_FB2_BINARY_BYTES {
        push_warning_once(
            warnings,
            "FictionBook binary image exceeded the per-image byte limit",
        );
        return Ok(None);
    }
    let Some(mime) =
        sniff_image_mime(&bytes).filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
    else {
        push_warning_once(
            warnings,
            "unsupported FictionBook binary resources were omitted; only PNG and JPEG are embedded",
        );
        return Ok(None);
    };
    if !binary.content_type.is_empty()
        && !binary.content_type.to_ascii_lowercase().starts_with(mime)
    {
        push_warning_once(
            warnings,
            "FictionBook binary content type did not match its image signature",
        );
        return Ok(None);
    }
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_warning_once(
            warnings,
            "invalid FictionBook PNG/JPEG image dimensions were omitted",
        );
        return Ok(None);
    };
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width == 0 || height == 0 || pixels > MAX_FB2_IMAGE_PIXELS {
        push_warning_once(
            warnings,
            "FictionBook binary image exceeded the per-image pixel limit",
        );
        return Ok(None);
    }
    let uri_bytes = format!("data:{mime};base64,").len() + bytes.len().div_ceil(3) * 4;
    let next_bytes = budget.decoded_bytes.saturating_add(bytes.len());
    let next_uri = budget.uri_bytes.saturating_add(uri_bytes);
    let next_pixels = budget.pixels.saturating_add(pixels);
    if next_bytes > MAX_FB2_TOTAL_BINARY_BYTES
        || next_uri > MAX_FB2_TOTAL_URI_BYTES
        || next_pixels > MAX_FB2_TOTAL_IMAGE_PIXELS
        || budget.count >= MAX_FB2_IMAGES
    {
        push_warning_once(
            warnings,
            "FictionBook images exceeded cumulative resource limits",
        );
        return Ok(None);
    }
    budget.decoded_bytes = next_bytes;
    budget.uri_bytes = next_uri;
    budget.pixels = next_pixels;
    budget.count += 1;
    let prefix = format!("data:{mime};base64,");
    Ok(Some(InlineHtmlImage {
        href: format!("{prefix}{}", BASE64_STANDARD.encode(bytes)),
        pixel_width: width,
        pixel_height: height,
    }))
}

fn parse_body(
    xml: &str,
    images: &HashMap<String, InlineHtmlImage>,
    max_events: usize,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut depth = 0usize;
    let mut events = 0usize;
    let mut main_body = false;
    let mut body_seen = false;
    let mut ignored_body_depth = 0usize;
    let mut section_depth = 0usize;
    let mut in_title = false;
    let mut title_text = String::new();
    let mut paragraph: Option<String> = None;
    let mut cell = String::new();
    let mut table = None::<TableBuilder>;
    let mut rendered_text_bytes = 0usize;
    let metadata = extract_metadata(xml, max_events)?;
    if !metadata.title.is_empty() {
        blocks.push(HtmlBlock::Heading {
            level: 1,
            text: metadata.title,
        });
    }
    if !metadata.author.is_empty() {
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Author: {}", metadata.author),
        });
    }
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "FictionBook body exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"body" {
                    if body_seen {
                        ignored_body_depth = 1;
                    } else if attribute(&element, b"name").is_none() {
                        main_body = true;
                        body_seen = true;
                    } else {
                        ignored_body_depth = 1;
                        body_seen = true;
                    }
                } else if ignored_body_depth > 0 {
                    ignored_body_depth += 1;
                } else if main_body {
                    match name {
                        b"section" => section_depth = section_depth.saturating_add(1),
                        b"title" => {
                            flush_paragraph(&mut blocks, &mut paragraph, &mut rendered_text_bytes)?;
                            in_title = true;
                            title_text.clear();
                        }
                        b"p" | b"subtitle" | b"v" => {
                            if paragraph.is_none() {
                                paragraph = Some(String::new());
                            }
                        }
                        b"table" => table = Some(TableBuilder::default()),
                        b"th" | b"td" => {
                            cell.clear();
                            if let Some(table) = table.as_mut() {
                                table.in_cell = true;
                            }
                        }
                        _ => {}
                    }
                }
                depth = depth.saturating_add(1);
                if depth > MAX_FB2_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "FictionBook XML nesting exceeds {MAX_FB2_XML_DEPTH}"
                    )));
                }
            }
            Event::Empty(element) => {
                if ignored_body_depth == 0 && main_body {
                    let qualified_name = element.name();
                    let name = local_name(qualified_name.as_ref());
                    match name {
                        b"empty-line" => {
                            flush_paragraph(&mut blocks, &mut paragraph, &mut rendered_text_bytes)?
                        }
                        b"image" => {
                            if let Some(reference) = attribute(&element, b"href")
                                .or_else(|| attribute(&element, b"l:href"))
                            {
                                append_image(
                                    &mut blocks,
                                    &mut warnings,
                                    images,
                                    &reference,
                                    &mut paragraph,
                                    &mut cell,
                                    table.as_ref().is_some_and(|table| table.in_cell),
                                    &mut rendered_text_bytes,
                                )?;
                            } else {
                                push_warning_once(
                                    &mut warnings,
                                    "FictionBook image without xlink:href was omitted",
                                );
                            }
                        }
                        b"br" => {
                            if let Some(text) = paragraph.as_mut() {
                                text.push('\n');
                            }
                        }
                        _ => {}
                    }
                }
            }
            Event::Text(text) => {
                if ignored_body_depth == 0 && main_body {
                    let decoded = String::from_utf8_lossy(text.as_ref());
                    let decoded = quick_xml::escape::unescape(&decoded).map_err(|error| {
                        Error::InvalidInput(format!("invalid FictionBook XML text: {error}"))
                    })?;
                    if in_title {
                        title_text.push_str(&decoded);
                    } else if let Some(table) = table.as_ref() {
                        if table.in_cell {
                            cell.push_str(&decoded);
                        } else if let Some(text) = paragraph.as_mut() {
                            text.push_str(&decoded);
                        }
                    } else if let Some(text) = paragraph.as_mut() {
                        text.push_str(&decoded);
                    }
                }
            }
            Event::CData(text) => {
                if ignored_body_depth == 0 && main_body {
                    let decoded = String::from_utf8_lossy(text.as_ref());
                    if in_title {
                        title_text.push_str(&decoded);
                    } else if let Some(table) = table.as_ref() {
                        if table.in_cell {
                            cell.push_str(&decoded);
                        } else if let Some(text) = paragraph.as_mut() {
                            text.push_str(&decoded);
                        }
                    } else if let Some(text) = paragraph.as_mut() {
                        text.push_str(&decoded);
                    }
                }
            }
            Event::GeneralRef(reference) => {
                if ignored_body_depth == 0 && main_body {
                    let value = reference.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid FictionBook reference: {error}"))
                    })?;
                    if let Some(text) = paragraph.as_mut() {
                        text.push_str(&format!("&{value};"));
                    }
                }
            }
            Event::End(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if ignored_body_depth > 0 {
                    ignored_body_depth = ignored_body_depth.saturating_sub(1);
                } else if name == b"body" {
                    main_body = false;
                } else if main_body {
                    match name {
                        b"p" | b"subtitle" | b"v" => {
                            if !in_title && table.as_ref().is_none_or(|table| !table.in_cell) {
                                flush_paragraph(
                                    &mut blocks,
                                    &mut paragraph,
                                    &mut rendered_text_bytes,
                                )?;
                            }
                        }
                        b"title" => {
                            let title = clean_fb2_text(&title_text);
                            if !title.is_empty() {
                                blocks.push(HtmlBlock::Heading {
                                    level: section_depth.clamp(1, 6) as u8,
                                    text: title,
                                });
                            }
                            in_title = false;
                            title_text.clear();
                        }
                        b"section" => section_depth = section_depth.saturating_sub(1),
                        b"th" | b"td" => {
                            if let Some(table) = table.as_mut() {
                                table.current_row.push(clean_fb2_text(&cell));
                                table.in_cell = false;
                            }
                            cell.clear();
                        }
                        b"tr" => {
                            if let Some(table) = table.as_mut()
                                && !table.current_row.is_empty()
                            {
                                table.rows.push(std::mem::take(&mut table.current_row));
                            }
                        }
                        b"table" => {
                            if let Some(table) = table.take() {
                                let data = finish_table(table);
                                if !data.headers.is_empty() {
                                    blocks.push(HtmlBlock::Table(data));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "FictionBook document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    flush_paragraph(&mut blocks, &mut paragraph, &mut rendered_text_bytes)?;
    Ok((blocks, warnings))
}

fn extract_metadata(xml: &str, max_events: usize) -> Result<Metadata> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut metadata = Metadata::default();
    let mut current = None::<&'static str>;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "FictionBook metadata exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => match local_name(element.name().as_ref()) {
                b"book-title" => current = Some("title"),
                b"first-name" | b"middle-name" | b"last-name" => current = Some("author"),
                b"body" => break,
                _ => {}
            },
            Event::Text(text) => {
                let value = String::from_utf8_lossy(text.as_ref());
                match current {
                    Some("title") => metadata.title.push_str(&value),
                    Some("author") => {
                        if !metadata.author.is_empty() {
                            metadata.author.push(' ');
                        }
                        metadata.author.push_str(&value);
                    }
                    _ => {}
                }
            }
            Event::CData(text) => {
                let value = String::from_utf8_lossy(text.as_ref());
                match current {
                    Some("title") => metadata.title.push_str(&value),
                    Some("author") => {
                        if !metadata.author.is_empty() {
                            metadata.author.push(' ');
                        }
                        metadata.author.push_str(&value);
                    }
                    _ => {}
                }
            }
            Event::End(element) => {
                if matches!(
                    local_name(element.name().as_ref()),
                    b"book-title" | b"first-name" | b"middle-name" | b"last-name"
                ) {
                    current = None;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    metadata.title = clean_fb2_text(&metadata.title);
    metadata.author = clean_fb2_text(&metadata.author);
    Ok(metadata)
}

#[allow(clippy::too_many_arguments)]
fn append_image(
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    images: &HashMap<String, InlineHtmlImage>,
    reference: &str,
    paragraph: &mut Option<String>,
    cell: &mut String,
    in_cell: bool,
    text_bytes: &mut usize,
) -> Result<()> {
    let key = reference.trim().trim_start_matches('#');
    if let Some(image) = images.get(key) {
        if in_cell {
            cell.push_str("[Embedded image]");
        } else {
            flush_paragraph(blocks, paragraph, text_bytes)?;
            blocks.push(HtmlBlock::Image {
                href: image.href.clone(),
                pixel_width: image.pixel_width,
                pixel_height: image.pixel_height,
                alt: "Embedded FictionBook image".into(),
            });
        }
    } else {
        push_warning_once(
            warnings,
            "FictionBook image reference did not resolve to an embedded PNG/JPEG",
        );
        if in_cell {
            cell.push_str("[Image omitted]");
        }
    }
    Ok(())
}

fn flush_paragraph(
    blocks: &mut Vec<HtmlBlock>,
    paragraph: &mut Option<String>,
    text_bytes: &mut usize,
) -> Result<()> {
    let Some(text) = paragraph.take() else {
        return Ok(());
    };
    let text = clean_fb2_text(&text);
    if text.is_empty() {
        return Ok(());
    }
    *text_bytes = text_bytes.saturating_add(text.len());
    if *text_bytes > MAX_FB2_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "FictionBook rendered text exceeds {MAX_FB2_TEXT_BYTES} bytes"
        )));
    }
    blocks.push(HtmlBlock::Paragraph { text });
    Ok(())
}

fn finish_table(table: TableBuilder) -> TableData {
    let mut rows = table.rows;
    let mut headers = rows.first().cloned().unwrap_or_default();
    if !rows.is_empty() {
        rows.remove(0);
    }
    let columns = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .max(1);
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

fn clean_fb2_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn attribute(element: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    element
        .attributes()
        .with_checks(false)
        .flatten()
        .find_map(|attribute| {
            let key = local_name(attribute.key.as_ref());
            (key == name || attribute.key.as_ref() == name)
                .then(|| String::from_utf8_lossy(attribute.value.as_ref()).into_owned())
        })
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_fictionbook_root_and_metadata() {
        let xml = r#"<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0"><description><title-info><book-title>Book</book-title><author><first-name>A</first-name><last-name>B</last-name></author></title-info></description><body><section><title><p>Chapter</p></title><p>Text</p></section></body></FictionBook>"#;
        assert!(looks_like_prefix(xml.as_bytes()));
        let metadata = extract_metadata(xml, 1000).unwrap();
        assert_eq!(metadata.title, "Book");
        assert_eq!(metadata.author, "A B");
    }
}
