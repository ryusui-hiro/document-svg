//! Bounded DITA topic and map previews.
//!
//! DITA is an XML vocabulary for modular technical documentation. This
//! adapter renders topic titles, short descriptions, sections, paragraphs,
//! lists, code blocks, simple tables, and local PNG/JPEG images. Topic maps
//! resolve only local `topicref href` targets; no key resolution, XInclude,
//! DTD/entity expansion, URL fetch, or code execution is performed.

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
use crate::local_resource::resolve_relative_file;
use crate::ooxml::{attribute, local_name};
use crate::table::{TableAlign, TableData};

const MAX_DITA_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DITA_TOPIC_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DITA_XML_DEPTH: usize = 256;
const MAX_DITA_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_DITA_IMAGE_REFERENCES: usize = 10_000;
const MAX_DITA_TOPIC_REFS: usize = 10_000;
const MAX_DITA_TABLE_CELLS: usize = 200_000;

#[derive(Default)]
struct DitaTable {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_cell: bool,
    in_header: bool,
    header_rows: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ActiveKind {
    Heading(u8),
    Paragraph,
    ListItem(String),
    Code,
}

struct ActiveText {
    kind: ActiveKind,
    text: String,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let lower = text.to_ascii_lowercase();
    let root = lower.contains("<topic")
        || lower.contains("<concept")
        || lower.contains("<task")
        || lower.contains("<reference")
        || lower.contains("<map");
    let known_namespace = lower.contains("dita.oasis-open.org")
        || lower.contains("dita.org")
        || lower.contains("oasis-open.org/dita");
    let known_doctype = lower.contains("<!doctype") && lower.contains("dita");
    root && (known_namespace || known_doctype)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DITA_INPUT_BYTES),
        "DITA input",
    )?;
    let xml = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("DITA input is not UTF-8: {error}")))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = fs::canonicalize(parent)?;
    let root_name = root_name(&xml, options.max_xml_events)?;
    let is_map = root_name.as_deref() == Some("map");
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    if is_map {
        let (map_title, references) = collect_map_refs(&xml, options.max_xml_events)?;
        if let Some(title) = map_title {
            blocks.push(HtmlBlock::Heading {
                level: 1,
                text: title,
            });
        }
        if references.is_empty() {
            push_warning_once(&mut warnings, "DITA map contains no local topicref targets");
        }
        let mut total_topic_bytes = 0u64;
        for reference in references {
            let Some(topic_path) = resolve_relative_file(&base_dir, &reference) else {
                push_warning_once(
                    &mut warnings,
                    "DITA topicref targets outside the map directory or uses an unsupported URI scheme",
                );
                continue;
            };
            let metadata = match fs::metadata(&topic_path) {
                Ok(metadata) if metadata.is_file() => metadata,
                _ => {
                    push_warning_once(&mut warnings, "missing DITA topicref targets were omitted");
                    continue;
                }
            };
            if metadata.len() > MAX_DITA_TOPIC_BYTES {
                push_warning_once(
                    &mut warnings,
                    "DITA topicref targets exceeding the per-topic byte limit were omitted",
                );
                continue;
            }
            total_topic_bytes = total_topic_bytes.saturating_add(metadata.len());
            if total_topic_bytes > options.max_input_bytes.min(MAX_DITA_INPUT_BYTES) {
                return Err(Error::LimitExceeded(
                    "DITA map topic inputs exceed the cumulative input limit".into(),
                ));
            }
            let topic_bytes = read_limited_file(&topic_path, MAX_DITA_TOPIC_BYTES, "DITA topic")?;
            let topic_xml = String::from_utf8(topic_bytes).map_err(|error| {
                Error::InvalidInput(format!("DITA topic is not UTF-8: {error}"))
            })?;
            let topic_base = topic_path.parent().unwrap_or_else(|| Path::new("."));
            let (sources, exceeded) = collect_image_sources(&topic_xml, options.max_xml_events)?;
            let (images, image_warnings) = load_local_image_sources(topic_base, sources, exceeded)?;
            warnings.extend(image_warnings);
            let (topic_blocks, topic_warnings) =
                parse_topic(&topic_xml, &images, options.max_xml_events)?;
            blocks.extend(topic_blocks);
            warnings.extend(topic_warnings);
        }
    } else {
        let (sources, exceeded) = collect_image_sources(&xml, options.max_xml_events)?;
        let (images, image_warnings) = load_local_image_sources(&base_dir, sources, exceeded)?;
        warnings.extend(image_warnings);
        let (topic_blocks, topic_warnings) = parse_topic(&xml, &images, options.max_xml_events)?;
        blocks.extend(topic_blocks);
        warnings.extend(topic_warnings);
    }
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "DITA document contains no renderable content".into(),
        ));
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn root_name(xml: &str, max_events: usize) -> Result<Option<String>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    for _ in 0..max_events {
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) | Event::Empty(element) => {
                let qualified = element.name();
                return Ok(Some(
                    String::from_utf8_lossy(local_name(qualified.as_ref())).into_owned(),
                ));
            }
            Event::DocType(_) => {}
            Event::Eof => return Ok(None),
            _ => {}
        }
        buffer.clear();
    }
    Err(Error::LimitExceeded(format!(
        "DITA XML exceeds {max_events} parser events"
    )))
}

fn collect_map_refs(xml: &str, max_events: usize) -> Result<(Option<String>, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut title = None;
    let mut in_title = false;
    let mut title_text = String::new();
    let mut references = Vec::new();
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "DITA map exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if name == b"title" && title.is_none() {
                    in_title = true;
                    title_text.clear();
                }
                if name == b"topicref"
                    && references.len() < MAX_DITA_TOPIC_REFS
                    && let Some(href) = attribute(&element, b"href")
                    && !href.trim().is_empty()
                {
                    let target = href.split('#').next().unwrap_or_default();
                    if !target.is_empty() {
                        references.push(target.to_owned());
                    }
                }
            }
            Event::Empty(element) => {
                let qualified = element.name();
                if local_name(qualified.as_ref()) == b"topicref"
                    && references.len() < MAX_DITA_TOPIC_REFS
                    && let Some(href) = attribute(&element, b"href")
                    && !href.trim().is_empty()
                {
                    let target = href.split('#').next().unwrap_or_default();
                    if !target.is_empty() {
                        references.push(target.to_owned());
                    }
                }
            }
            Event::Text(text) if in_title => {
                title_text.push_str(&text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid DITA map title: {error}"))
                })?);
            }
            Event::End(element) => {
                let qualified = element.name();
                if local_name(qualified.as_ref()) == b"title" && in_title {
                    let clean = clean_text(&title_text);
                    if !clean.is_empty() {
                        title = Some(clean);
                    }
                    in_title = false;
                }
            }
            Event::DocType(_) => {}
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((title, references))
}

fn collect_image_sources(xml: &str, max_events: usize) -> Result<(Vec<String>, bool)> {
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
                "DITA XML exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) | Event::Empty(element) => {
                let qualified = element.name();
                if local_name(qualified.as_ref()) == b"image"
                    && let Some(href) = attribute(&element, b"href")
                    && !href.trim().is_empty()
                {
                    if sources.len() >= MAX_DITA_IMAGE_REFERENCES {
                        exceeded = true;
                    } else {
                        sources.push(href);
                    }
                }
            }
            Event::DocType(_) => {}
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((sources, exceeded))
}

fn parse_topic(
    xml: &str,
    images: &HashMap<String, InlineHtmlImage>,
    max_events: usize,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<String>::new();
    let mut active = None::<ActiveText>;
    let mut table = None::<DitaTable>;
    let mut list_stack = Vec::<bool>::new();
    let mut ordered_index = Vec::<usize>::new();
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut text_bytes = 0usize;
    let mut image_count = 0usize;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "DITA topic exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref()).to_vec();
                let name_string = String::from_utf8_lossy(&name).into_owned();
                if stack.len() >= MAX_DITA_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "DITA XML nesting exceeds {MAX_DITA_XML_DEPTH}"
                    )));
                }
                match name.as_slice() {
                    b"title" | b"navtitle" => {
                        active = Some(ActiveText {
                            kind: ActiveKind::Heading(title_level(&stack)),
                            text: String::new(),
                        });
                    }
                    b"p" | b"shortdesc" | b"lq" | b"note" | b"hazardstatement" | b"stepsection" => {
                        if active.is_none() && table.is_none() {
                            active = Some(ActiveText {
                                kind: ActiveKind::Paragraph,
                                text: String::new(),
                            });
                        }
                    }
                    b"codeblock" | b"pre" | b"lines" | b"msgblock" => {
                        if active.is_none() {
                            active = Some(ActiveText {
                                kind: ActiveKind::Code,
                                text: String::new(),
                            });
                        }
                    }
                    b"ul" | b"sl" => list_stack.push(false),
                    b"ol" => {
                        list_stack.push(true);
                        ordered_index.push(1);
                    }
                    b"li" | b"step" => {
                        if active.is_none() {
                            let ordered = list_stack.last().copied().unwrap_or(false);
                            let bullet = if ordered {
                                let index =
                                    ordered_index.last_mut().expect("DITA ordered list counter");
                                let bullet = format!("{index}.");
                                *index = index.saturating_add(1);
                                bullet
                            } else {
                                "•".into()
                            };
                            active = Some(ActiveText {
                                kind: ActiveKind::ListItem(bullet),
                                text: String::new(),
                            });
                        }
                    }
                    b"simpletable" | b"table" => table = Some(DitaTable::default()),
                    b"sthead" | b"thead" => {
                        if let Some(table) = table.as_mut() {
                            table.in_header = true;
                        }
                    }
                    b"stbody" | b"tbody" | b"tfoot" => {
                        if let Some(table) = table.as_mut() {
                            table.in_header = false;
                        }
                    }
                    b"strow" | b"row" | b"tr" => {
                        if let Some(table) = table.as_mut() {
                            table.row.clear();
                        }
                    }
                    b"stentry" | b"entry" | b"td" | b"th" => {
                        if let Some(table) = table.as_mut() {
                            table.cell.clear();
                            table.in_cell = true;
                            if name.as_slice() == b"th" {
                                table.in_header = true;
                            }
                        }
                    }
                    b"image" => {
                        image_count = image_count.saturating_add(1);
                        if image_count <= MAX_DITA_IMAGE_REFERENCES {
                            let source = attribute(&element, b"href");
                            append_image(&mut blocks, &mut warnings, images, source.as_deref());
                        } else {
                            push_warning_once(
                                &mut warnings,
                                "DITA image references exceeded the supported limit; remaining images were omitted",
                            );
                        }
                    }
                    b"xref" | b"link" | b"object" | b"foreign" => push_warning_once(
                        &mut warnings,
                        "DITA links and foreign content remain inert; targets were not loaded",
                    ),
                    b"include" => {
                        push_warning_once(&mut warnings, "DITA include content was not loaded")
                    }
                    _ => {}
                }
                stack.push(name_string);
            }
            Event::Empty(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if name == b"image" {
                    image_count = image_count.saturating_add(1);
                    if image_count <= MAX_DITA_IMAGE_REFERENCES {
                        let source = attribute(&element, b"href");
                        append_image(&mut blocks, &mut warnings, images, source.as_deref());
                    } else {
                        push_warning_once(
                            &mut warnings,
                            "DITA image references exceeded the supported limit; remaining images were omitted",
                        );
                    }
                }
            }
            Event::Text(text) => {
                let value = text
                    .decode()
                    .map_err(|error| Error::InvalidInput(format!("invalid DITA text: {error}")))?;
                let value = quick_xml::escape::unescape(&value)
                    .map_err(|error| Error::InvalidInput(format!("invalid DITA text: {error}")))?;
                append_text(&value, &mut active, &mut table, &mut text_bytes)?;
            }
            Event::CData(text) => {
                let value = text
                    .decode()
                    .map_err(|error| Error::InvalidInput(format!("invalid DITA CDATA: {error}")))?;
                append_text(&value, &mut active, &mut table, &mut text_bytes)?;
            }
            Event::GeneralRef(reference) => {
                let value = reference.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid DITA reference: {error}"))
                })?;
                append_text(
                    &format!("&{value};"),
                    &mut active,
                    &mut table,
                    &mut text_bytes,
                )?;
            }
            Event::End(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                match name {
                    b"title" | b"navtitle" => {
                        if let Some(ActiveText {
                            kind: ActiveKind::Heading(level),
                            text,
                        }) = active.take()
                            && !clean_text(&text).is_empty()
                        {
                            blocks.push(HtmlBlock::Heading {
                                level,
                                text: clean_text(&text),
                            });
                        }
                    }
                    b"p" | b"shortdesc" | b"lq" | b"note" | b"hazardstatement" | b"stepsection"
                    | b"li" | b"step" => flush_text_block(&mut blocks, &mut active),
                    b"codeblock" | b"pre" | b"lines" | b"msgblock" => {
                        if let Some(ActiveText {
                            kind: ActiveKind::Code,
                            text,
                        }) = active.take()
                            && !text.trim().is_empty()
                        {
                            blocks.push(HtmlBlock::CodeBlock { text });
                        }
                    }
                    b"stentry" | b"entry" | b"td" | b"th" => {
                        if let Some(table) = table.as_mut()
                            && table.in_cell
                        {
                            table.row.push(clean_text(&table.cell));
                            table.cell.clear();
                            table.in_cell = false;
                        }
                    }
                    b"strow" | b"row" | b"tr" => {
                        if let Some(table) = table.as_mut()
                            && !table.row.is_empty()
                        {
                            if table.in_header {
                                table.header_rows = table.header_rows.saturating_add(1);
                            }
                            table.rows.push(std::mem::take(&mut table.row));
                            if table.rows.iter().map(Vec::len).sum::<usize>() > MAX_DITA_TABLE_CELLS
                            {
                                return Err(Error::LimitExceeded(format!(
                                    "DITA table exceeds {MAX_DITA_TABLE_CELLS} cells"
                                )));
                            }
                        }
                    }
                    b"sthead" | b"thead" => {
                        if let Some(table) = table.as_mut() {
                            table.in_header = false;
                        }
                    }
                    b"simpletable" | b"table" => {
                        if let Some(table) = table.take() {
                            let table = finish_table(table);
                            if !table.headers.is_empty() || !table.rows.is_empty() {
                                blocks.push(HtmlBlock::Table(table));
                            }
                        }
                    }
                    b"ul" | b"sl" | b"ol" => {
                        list_stack.pop();
                        if name == b"ol" {
                            ordered_index.pop();
                        }
                    }
                    _ => {}
                }
                stack.pop();
            }
            Event::DocType(_) => push_warning_once(
                &mut warnings,
                "DITA DTD declaration was ignored; external entities were not loaded",
            ),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((blocks, warnings))
}

fn append_text(
    value: &str,
    active: &mut Option<ActiveText>,
    table: &mut Option<DitaTable>,
    text_bytes: &mut usize,
) -> Result<()> {
    *text_bytes = text_bytes.saturating_add(value.len());
    if *text_bytes > MAX_DITA_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DITA text exceeds {MAX_DITA_TEXT_BYTES} bytes"
        )));
    }
    if let Some(table) = table.as_mut()
        && table.in_cell
    {
        table.cell.push_str(value);
    } else if let Some(active) = active.as_mut() {
        active.text.push_str(value);
    }
    Ok(())
}

fn flush_text_block(blocks: &mut Vec<HtmlBlock>, active: &mut Option<ActiveText>) {
    let Some(ActiveText { kind, text }) = active.take() else {
        return;
    };
    let text = clean_text(&text);
    if text.is_empty() {
        return;
    }
    match kind {
        ActiveKind::ListItem(bullet) => blocks.push(HtmlBlock::ListItem { bullet, text }),
        ActiveKind::Paragraph => blocks.push(HtmlBlock::Paragraph { text }),
        _ => {}
    }
}

fn append_image(
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    images: &HashMap<String, InlineHtmlImage>,
    source: Option<&str>,
) {
    let Some(source) = source else {
        push_warning_once(warnings, "DITA image without href was omitted");
        return;
    };
    if let Some(image) = images.get(source) {
        blocks.push(HtmlBlock::Image {
            href: image.href.clone(),
            pixel_width: image.pixel_width,
            pixel_height: image.pixel_height,
            alt: "DITA image".into(),
        });
    } else {
        push_warning_once(
            warnings,
            "DITA image was omitted because it was not a validated local PNG/JPEG resource",
        );
    }
}

fn finish_table(table: DitaTable) -> TableData {
    let mut rows = table.rows;
    let header_count = table.header_rows.min(rows.len());
    let mut headers = if header_count > 0 {
        rows.drain(..header_count).flatten().collect()
    } else {
        rows.first().cloned().unwrap_or_default()
    };
    if header_count == 0 && !rows.is_empty() {
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

fn title_level(stack: &[String]) -> u8 {
    let depth = stack
        .iter()
        .filter(|name| {
            matches!(
                name.as_str(),
                "topic" | "concept" | "task" | "reference" | "section" | "example" | "stepsection"
            )
        })
        .count();
    depth.clamp(1, 6) as u8
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;

    #[test]
    fn recognizes_dita_namespace_and_doctype_without_broad_text_sniffing() {
        assert!(looks_like_prefix(
            br#"<topic id="t" xmlns="http://dita.oasis-open.org/architecture/1.3/"><title>Guide</title></topic>"#
        ));
        assert!(looks_like_prefix(
            br#"<!DOCTYPE concept PUBLIC "-//OASIS//DTD DITA Concept//EN" "concept.dtd"><concept/>"#
        ));
        assert!(!looks_like_prefix(
            br#"<topic><p>This mentions DITA as prose.</p></topic>"#
        ));
    }
}
