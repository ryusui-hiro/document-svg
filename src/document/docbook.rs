//! Bounded DocBook 4/5 article and book preview.
//!
//! The converter keeps DocBook content inert: it lays out titles, paragraphs,
//! lists, source blocks, simple CALS-like tables, and validated local
//! PNG/JPEG media. XInclude, entities, processing instructions, links,
//! MathML, scripts, and publisher-specific extensions are never evaluated.

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

const MAX_DOCBOOK_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_DOCBOOK_XML_DEPTH: usize = 256;
const MAX_DOCBOOK_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_DOCBOOK_IMAGE_REFERENCES: usize = 10_000;
const MAX_DOCBOOK_TABLE_CELLS: usize = 200_000;

#[derive(Default)]
struct DocBookTable {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    in_cell: bool,
    in_header: bool,
    header_rows: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ActiveKind {
    Title(u8),
    Paragraph,
    ListItem(String),
    Code,
    Caption,
    Term,
}

struct ActiveText {
    kind: ActiveKind,
    text: String,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let lower = text.to_ascii_lowercase();
    let has_root = lower.contains("<book")
        || lower.contains("<article")
        || lower.contains("<chapter")
        || lower.contains("<section")
        || lower.contains("<topic");
    let known_namespace = lower.contains("http://docbook.org/ns/docbook")
        || lower.contains("https://docbook.org/ns/docbook")
        || lower.contains("oasis-open.org/docbook");
    let known_doctype = lower.contains("<!doctype") && lower.contains("docbook");
    has_root && (known_namespace || known_doctype)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DOCBOOK_INPUT_BYTES),
        "DocBook input",
    )?;
    let xml = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("DocBook input is not UTF-8: {error}")))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = fs::canonicalize(parent)?;
    let (sources, source_limit_exceeded) = collect_image_sources(&xml, options.max_xml_events)?;
    let (images, mut warnings) =
        load_local_image_sources(&base_dir, sources, source_limit_exceeded)?;
    let (blocks, parser_warnings) = parse_blocks(&xml, &images, options.max_xml_events)?;
    warnings.extend(parser_warnings);
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "DocBook document contains no renderable content".into(),
        ));
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
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
                "DocBook XML exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) | Event::Empty(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if matches!(name, b"imagedata" | b"graphic" | b"inlinegraphic") {
                    let source = attribute(&element, b"fileref")
                        .or_else(|| attribute(&element, b"href"))
                        .or_else(|| attribute(&element, b"entityref"));
                    if let Some(source) = source.filter(|source| !source.trim().is_empty()) {
                        if sources.len() >= MAX_DOCBOOK_IMAGE_REFERENCES {
                            exceeded = true;
                        } else {
                            sources.push(source);
                        }
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
    let mut active = None::<ActiveText>;
    let mut table = None::<DocBookTable>;
    let mut figure_caption = None::<String>;
    let mut text_bytes = 0usize;
    let mut event_count = 0usize;
    let mut list_stack = Vec::<bool>::new();
    let mut ordered_index = Vec::<usize>::new();
    let mut image_count = 0usize;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "DocBook XML exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let name = local_name(element.name().as_ref()).to_vec();
                let name_str = String::from_utf8_lossy(&name).into_owned();
                if stack.len() >= MAX_DOCBOOK_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "DocBook XML nesting exceeds {MAX_DOCBOOK_XML_DEPTH}"
                    )));
                }
                match name.as_slice() {
                    b"title" => {
                        let level = title_level(&stack);
                        active = Some(ActiveText {
                            kind: ActiveKind::Title(level),
                            text: String::new(),
                        });
                    }
                    b"para" | b"simpara" | b"formalpara" | b"remark" | b"abstract"
                    | b"blockquote" | b"note" | b"tip" | b"warning" | b"important" | b"caution"
                    | b"danger" => {
                        if active.is_none() && table.is_none() {
                            active = Some(ActiveText {
                                kind: ActiveKind::Paragraph,
                                text: String::new(),
                            });
                        }
                    }
                    b"programlisting" | b"screen" | b"literallayout" | b"synopsis" => {
                        if active.is_none() {
                            active = Some(ActiveText {
                                kind: ActiveKind::Code,
                                text: String::new(),
                            });
                        }
                    }
                    b"caption" => {
                        active = Some(ActiveText {
                            kind: ActiveKind::Caption,
                            text: String::new(),
                        });
                    }
                    b"term" => {
                        active = Some(ActiveText {
                            kind: ActiveKind::Term,
                            text: String::new(),
                        });
                    }
                    b"itemizedlist" => list_stack.push(false),
                    b"orderedlist" => {
                        list_stack.push(true);
                        ordered_index.push(1);
                    }
                    b"listitem" => {
                        if active.is_none() {
                            let ordered = list_stack.last().copied().unwrap_or(false);
                            let bullet = if ordered {
                                let index = ordered_index.last_mut().expect("ordered list counter");
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
                    b"table" | b"informaltable" => table = Some(DocBookTable::default()),
                    b"thead" => {
                        if let Some(table) = table.as_mut() {
                            table.in_header = true;
                        }
                    }
                    b"tbody" | b"tfoot" => {
                        if let Some(table) = table.as_mut() {
                            table.in_header = false;
                        }
                    }
                    b"row" | b"tr" => {
                        if let Some(table) = table.as_mut() {
                            table.row.clear();
                        }
                    }
                    b"entry" | b"td" | b"th" => {
                        if let Some(table) = table.as_mut() {
                            table.cell.clear();
                            table.in_cell = true;
                            if name.as_slice() == b"th" {
                                table.in_header = true;
                            }
                        }
                    }
                    b"ulink" | b"link" | b"xref" | b"biblioref" => push_warning_once(
                        &mut warnings,
                        "DocBook links and cross-references are shown as text; targets were not loaded",
                    ),
                    b"equation" | b"inlineequation" | b"informalequation" | b"mathphrase" => {
                        push_warning_once(
                            &mut warnings,
                            "DocBook MathML and equation markup was shown as text without evaluation",
                        );
                    }
                    b"include" => push_warning_once(
                        &mut warnings,
                        "DocBook XInclude or include content was not loaded",
                    ),
                    _ => {}
                }
                stack.push(name_str);
            }
            Event::Empty(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                if matches!(name, b"imagedata" | b"graphic" | b"inlinegraphic") {
                    image_count = image_count.saturating_add(1);
                    if image_count <= MAX_DOCBOOK_IMAGE_REFERENCES {
                        let source = attribute(&element, b"fileref")
                            .or_else(|| attribute(&element, b"href"))
                            .or_else(|| attribute(&element, b"entityref"));
                        append_image(
                            &mut blocks,
                            &mut warnings,
                            images,
                            source.as_deref(),
                            figure_caption.take(),
                        );
                    } else {
                        push_warning_once(
                            &mut warnings,
                            "DocBook image references exceeded the supported limit; remaining images were omitted",
                        );
                    }
                }
            }
            Event::Text(text) => {
                let value = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid DocBook text: {error}"))
                })?;
                let value = quick_xml::escape::unescape(&value).map_err(|error| {
                    Error::InvalidInput(format!("invalid DocBook text: {error}"))
                })?;
                append_text(&value, &mut active, &mut table, &mut text_bytes)?;
            }
            Event::CData(text) => {
                let value = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid DocBook CDATA: {error}"))
                })?;
                append_text(&value, &mut active, &mut table, &mut text_bytes)?;
            }
            Event::GeneralRef(reference) => {
                let value = reference.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid DocBook reference: {error}"))
                })?;
                append_text(
                    &format!("&{value};"),
                    &mut active,
                    &mut table,
                    &mut text_bytes,
                )?;
            }
            Event::End(element) => {
                let qualified_name = element.name();
                let name = local_name(qualified_name.as_ref());
                match name {
                    b"title" => {
                        if let Some(ActiveText {
                            kind: ActiveKind::Title(level),
                            text,
                        }) = active.take()
                        {
                            let text = clean_text(&text);
                            if !text.is_empty() {
                                blocks.push(HtmlBlock::Heading { level, text });
                            }
                        }
                    }
                    b"para" | b"simpara" | b"formalpara" | b"remark" | b"abstract"
                    | b"blockquote" | b"note" | b"tip" | b"warning" | b"important" | b"caution"
                    | b"danger" => flush_active_paragraph(&mut blocks, &mut active, &stack),
                    b"listitem" => flush_active_paragraph(&mut blocks, &mut active, &stack),
                    b"programlisting" | b"screen" | b"literallayout" | b"synopsis" => {
                        if let Some(ActiveText {
                            kind: ActiveKind::Code,
                            text,
                        }) = active.take()
                            && !text.trim().is_empty()
                        {
                            blocks.push(HtmlBlock::CodeBlock { text });
                        }
                    }
                    b"caption" => {
                        if let Some(ActiveText {
                            kind: ActiveKind::Caption,
                            text,
                        }) = active.take()
                        {
                            let text = clean_text(&text);
                            if !text.is_empty() {
                                figure_caption = Some(text);
                            }
                        }
                    }
                    b"term" => {
                        if let Some(ActiveText {
                            kind: ActiveKind::Term,
                            text,
                        }) = active.take()
                        {
                            let text = clean_text(&text);
                            if !text.is_empty() {
                                blocks.push(HtmlBlock::Heading { level: 4, text });
                            }
                        }
                    }
                    b"entry" | b"td" | b"th" => {
                        if let Some(table) = table.as_mut()
                            && table.in_cell
                        {
                            table.row.push(clean_text(&table.cell));
                            table.cell.clear();
                            table.in_cell = false;
                        }
                    }
                    b"row" | b"tr" => {
                        if let Some(table) = table.as_mut()
                            && !table.row.is_empty()
                        {
                            if table.in_header {
                                table.header_rows = table.header_rows.saturating_add(1);
                            }
                            table.rows.push(std::mem::take(&mut table.row));
                            if table.rows.iter().map(Vec::len).sum::<usize>()
                                > MAX_DOCBOOK_TABLE_CELLS
                            {
                                return Err(Error::LimitExceeded(format!(
                                    "DocBook table exceeds {MAX_DOCBOOK_TABLE_CELLS} cells"
                                )));
                            }
                        }
                    }
                    b"table" | b"informaltable" => {
                        if let Some(table) = table.take() {
                            let table = finish_table(table);
                            if !table.headers.is_empty() || !table.rows.is_empty() {
                                blocks.push(HtmlBlock::Table(table));
                            }
                        }
                    }
                    b"figure" | b"informalfigure" => {
                        if let Some(caption) = figure_caption.take() {
                            blocks.push(HtmlBlock::Paragraph {
                                text: format!("Figure: {caption}"),
                            });
                        }
                    }
                    b"itemizedlist" | b"orderedlist" => {
                        list_stack.pop();
                        if name == b"orderedlist" {
                            ordered_index.pop();
                        }
                    }
                    _ => {}
                }
                stack.pop();
            }
            Event::DocType(_) => {
                // quick-xml reports the declaration without resolving it; keep
                // the document inert and make the omission visible to callers.
                push_warning_once(
                    &mut warnings,
                    "DocBook DTD declaration was ignored; external entities were not loaded",
                );
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if text_bytes > MAX_DOCBOOK_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DocBook text exceeds {MAX_DOCBOOK_TEXT_BYTES} bytes"
        )));
    }
    Ok((blocks, warnings))
}

fn append_text(
    value: &str,
    active: &mut Option<ActiveText>,
    table: &mut Option<DocBookTable>,
    text_bytes: &mut usize,
) -> Result<()> {
    *text_bytes = text_bytes.saturating_add(value.len());
    if *text_bytes > MAX_DOCBOOK_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DocBook text exceeds {MAX_DOCBOOK_TEXT_BYTES} bytes"
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

fn flush_active_paragraph(
    blocks: &mut Vec<HtmlBlock>,
    active: &mut Option<ActiveText>,
    _stack: &[String],
) {
    let Some(kind) = active.as_ref().map(|value| &value.kind) else {
        return;
    };
    if !matches!(kind, ActiveKind::Paragraph | ActiveKind::ListItem(_)) {
        return;
    }
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
    caption: Option<String>,
) {
    if let Some(caption_text) = caption.as_deref() {
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Figure: {caption_text}"),
        });
    }
    let Some(source) = source else {
        push_warning_once(warnings, "DocBook image without fileref was omitted");
        return;
    };
    if let Some(image) = images.get(source) {
        blocks.push(HtmlBlock::Image {
            href: image.href.clone(),
            pixel_width: image.pixel_width,
            pixel_height: image.pixel_height,
            alt: caption.unwrap_or_else(|| "DocBook image".into()),
        });
    } else {
        push_warning_once(
            warnings,
            "DocBook image was omitted because it was not a validated local PNG/JPEG resource",
        );
    }
}

fn finish_table(table: DocBookTable) -> TableData {
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
                "part"
                    | "chapter"
                    | "appendix"
                    | "preface"
                    | "section"
                    | "sect1"
                    | "sect2"
                    | "sect3"
                    | "sect4"
                    | "sect5"
                    | "simplesect"
                    | "topic"
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
    fn recognizes_docbook_namespace_and_doctype_without_broad_text_sniffing() {
        assert!(looks_like_prefix(
            br#"<article xmlns="http://docbook.org/ns/docbook"><title>Guide</title></article>"#
        ));
        assert!(looks_like_prefix(
            br#"<!DOCTYPE book PUBLIC "-//OASIS//DTD DocBook XML V4.5//EN" "docbookx.dtd"><book/>"#
        ));
        assert!(!looks_like_prefix(
            br#"<article><para>This mentions DocBook as prose.</para></article>"#
        ));
    }
}
