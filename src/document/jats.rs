//! Bounded JATS (Journal Article Tag Suite) article preview.
//!
//! JATS is XML used for journal articles and technical reports. This reader
//! renders article metadata, abstract, sections, paragraphs, figure captions,
//! and simple tables. `graphic`/`inline-graphic` references are confined to the
//! input directory and use the shared PNG/JPEG validation budget; XML, MathML,
//! external links, scripts, and publisher-specific extensions remain inert.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{
    HtmlBlock, InlineHtmlImage, load_local_image_sources, render_blocks_to_pages_with_warnings,
};
use crate::error::{Error, Result};
use crate::ooxml::{attribute, local_name};
use crate::table::{TableAlign, TableData};

const MAX_JATS_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_JATS_XML_DEPTH: usize = 256;
const MAX_JATS_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_JATS_TABLE_CELLS: usize = 200_000;
const MAX_JATS_IMAGE_REFERENCES: usize = 10_000;

#[derive(Default)]
struct JatsTable {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_cell: bool,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let lower = text.to_ascii_lowercase();
    lower.contains("<article")
        && (lower.contains("jats")
            || lower.contains("niso.org")
            || lower.contains("journal-meta")
            || lower.contains("article-meta"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_JATS_INPUT_BYTES),
        "JATS input",
    )?;
    let xml = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("JATS input is not UTF-8: {error}")))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = fs::canonicalize(parent)?;
    let (sources, source_limit_exceeded) = collect_graphic_sources(&xml, options.max_xml_events)?;
    let (images, mut warnings) =
        load_local_image_sources(&base_dir, sources, source_limit_exceeded)?;
    let (blocks, parser_warnings) = parse_blocks(&xml, &images, options.max_xml_events)?;
    warnings.extend(parser_warnings);
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "JATS article contains no renderable content".into(),
        ));
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn collect_graphic_sources(xml: &str, max_events: usize) -> Result<(Vec<String>, bool)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut sources = Vec::new();
    let mut exceeded = false;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "JATS XML exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) | Event::Empty(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if (name == b"graphic" || name == b"inline-graphic")
                    && let Some(href) = attribute(&element, b"href")
                    && !href.trim().is_empty()
                {
                    if sources.len() >= MAX_JATS_IMAGE_REFERENCES {
                        exceeded = true;
                    } else {
                        sources.push(href);
                    }
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "JATS document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((sources, exceeded))
}

fn parse_blocks(
    xml: &str,
    images: &HashMap<String, InlineHtmlImage>,
    max_events: usize,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut section_depth = 0usize;
    let mut heading = None::<String>;
    let mut paragraph = None::<String>;
    let mut caption = None::<String>;
    let mut label = None::<String>;
    let mut table = None::<JatsTable>;
    let mut rendered_text_bytes = 0usize;
    let mut event_count = 0usize;
    let mut title_info = false;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "JATS XML exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                let local = String::from_utf8_lossy(name).into_owned();
                match name {
                    b"title-group" => title_info = true,
                    b"article-title" if title_info => heading = Some(String::new()),
                    b"abstract" => {
                        push_warning_once(
                            &mut warnings,
                            "JATS abstract and article text layout is approximated",
                        );
                    }
                    b"sec" => section_depth = section_depth.saturating_add(1),
                    b"title" if !title_info => heading = Some(String::new()),
                    b"p" | b"subtitle" | b"named-content" | b"disp-quote" => {
                        if caption.is_none() && label.is_none() {
                            paragraph = Some(String::new());
                        }
                    }
                    b"caption" => caption = Some(String::new()),
                    b"label" => label = Some(String::new()),
                    b"table" => table = Some(JatsTable::default()),
                    b"tr" if table.is_some() => {
                        if let Some(table) = table.as_mut() {
                            table.row.clear();
                        }
                    }
                    b"th" | b"td" if table.is_some() => {
                        if let Some(table) = table.as_mut() {
                            table.cell.clear();
                            table.in_cell = true;
                        }
                    }
                    _ => {}
                }
                if stack.len() >= MAX_JATS_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "JATS XML nesting exceeds {MAX_JATS_XML_DEPTH}"
                    )));
                }
                stack.push(local);
            }
            Event::Empty(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if name == b"graphic" || name == b"inline-graphic" {
                    if let Some(href) = attribute(&element, b"href") {
                        append_graphic(&mut blocks, &mut warnings, images, &href, &mut paragraph)?;
                    } else {
                        push_warning_once(
                            &mut warnings,
                            "JATS graphic without xlink:href was omitted",
                        );
                    }
                } else if name == b"break" || name == b"hr" {
                    flush_paragraph(&mut blocks, &mut paragraph, &mut rendered_text_bytes)?;
                    blocks.push(HtmlBlock::HorizontalRule);
                }
            }
            Event::Text(text) => {
                let value = text
                    .decode()
                    .map_err(|error| Error::InvalidInput(format!("invalid JATS text: {error}")))?;
                let value = quick_xml::escape::unescape(&value).map_err(|error| {
                    Error::InvalidInput(format!("invalid JATS XML text: {error}"))
                })?;
                append_jats_text(
                    &value,
                    &mut heading,
                    &mut paragraph,
                    &mut caption,
                    &mut label,
                    &mut table,
                );
            }
            Event::CData(text) => {
                let value = text
                    .decode()
                    .map_err(|error| Error::InvalidInput(format!("invalid JATS CDATA: {error}")))?;
                append_jats_text(
                    &value,
                    &mut heading,
                    &mut paragraph,
                    &mut caption,
                    &mut label,
                    &mut table,
                );
            }
            Event::GeneralRef(reference) => {
                let value = reference.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid JATS reference: {error}"))
                })?;
                append_jats_text(
                    &format!("&{value};"),
                    &mut heading,
                    &mut paragraph,
                    &mut caption,
                    &mut label,
                    &mut table,
                );
            }
            Event::End(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                match name {
                    b"article-title" | b"title" if heading.is_some() => {
                        if let Some(text) = heading.take() {
                            let text = clean_jats_text(&text);
                            if !text.is_empty() {
                                blocks.push(HtmlBlock::Heading {
                                    level: if name == b"article-title" {
                                        1
                                    } else {
                                        section_depth.clamp(1, 6) as u8
                                    },
                                    text,
                                });
                            }
                        }
                    }
                    b"p" | b"subtitle" | b"named-content" | b"disp-quote" => {
                        if let Some(text) = paragraph.take() {
                            let text = clean_jats_text(&text);
                            if !text.is_empty() {
                                rendered_text_bytes =
                                    rendered_text_bytes.saturating_add(text.len());
                                if rendered_text_bytes > MAX_JATS_TEXT_BYTES {
                                    return Err(Error::LimitExceeded(format!(
                                        "JATS rendered text exceeds {MAX_JATS_TEXT_BYTES} bytes"
                                    )));
                                }
                                blocks.push(HtmlBlock::Paragraph { text });
                            }
                        }
                    }
                    b"caption" => {
                        if let Some(text) = caption.take() {
                            let text = clean_jats_text(&text);
                            if !text.is_empty() {
                                blocks.push(HtmlBlock::Paragraph {
                                    text: format!("Figure: {text}"),
                                });
                            }
                        }
                    }
                    b"label" => {
                        label = None;
                    }
                    b"th" | b"td" if table.is_some() => {
                        if let Some(table) = table.as_mut() {
                            table.row.push(clean_jats_text(&table.cell));
                            table.cell.clear();
                            table.in_cell = false;
                            if table.row.len() > MAX_JATS_TABLE_CELLS {
                                return Err(Error::LimitExceeded(
                                    "JATS table cell limit exceeded".into(),
                                ));
                            }
                        }
                    }
                    b"tr" if table.is_some() => {
                        if let Some(table) = table.as_mut()
                            && !table.row.is_empty()
                        {
                            table.rows.push(std::mem::take(&mut table.row));
                        }
                    }
                    b"table" => {
                        if let Some(table) = table.take() {
                            let data = finish_jats_table(table);
                            if !data.headers.is_empty() {
                                blocks.push(HtmlBlock::Table(data));
                            }
                        }
                    }
                    b"sec" => section_depth = section_depth.saturating_sub(1),
                    b"title-group" => title_info = false,
                    _ => {}
                }
                stack.pop();
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "JATS document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((blocks, warnings))
}

fn append_jats_text(
    value: &str,
    heading: &mut Option<String>,
    paragraph: &mut Option<String>,
    caption: &mut Option<String>,
    label: &mut Option<String>,
    table: &mut Option<JatsTable>,
) {
    if let Some(table) = table.as_mut()
        && table.in_cell
    {
        table.cell.push_str(value);
        return;
    }
    if let Some(heading) = heading.as_mut() {
        heading.push_str(value);
    } else if let Some(label) = label.as_mut() {
        label.push_str(value);
    } else if let Some(caption) = caption.as_mut() {
        caption.push_str(value);
    } else if let Some(paragraph) = paragraph.as_mut() {
        paragraph.push_str(value);
    }
}

fn append_graphic(
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    images: &HashMap<String, InlineHtmlImage>,
    href: &str,
    paragraph: &mut Option<String>,
) -> Result<()> {
    let Some(image) = images.get(href) else {
        push_warning_once(
            warnings,
            "JATS graphic was omitted because it was not a validated local PNG/JPEG resource",
        );
        return Ok(());
    };
    if let Some(text) = paragraph.take()
        && !text.trim().is_empty()
    {
        blocks.push(HtmlBlock::Paragraph {
            text: clean_jats_text(&text),
        });
    }
    blocks.push(HtmlBlock::Image {
        href: image.href.clone(),
        pixel_width: image.pixel_width,
        pixel_height: image.pixel_height,
        alt: "JATS graphic".into(),
    });
    Ok(())
}

fn flush_paragraph(
    blocks: &mut Vec<HtmlBlock>,
    paragraph: &mut Option<String>,
    _bytes: &mut usize,
) -> Result<()> {
    if let Some(text) = paragraph.take()
        && !text.trim().is_empty()
    {
        blocks.push(HtmlBlock::Paragraph {
            text: clean_jats_text(&text),
        });
    }
    Ok(())
}

fn finish_jats_table(table: JatsTable) -> TableData {
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

fn clean_jats_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}
