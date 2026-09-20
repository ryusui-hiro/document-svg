//! Bounded XMind mind-map package preview.
//!
//! XMind packages are ZIP containers with either content.xml or content.json.
//! This adapter renders root and attached topic titles as a safe outline.

use std::io::{Cursor, Read};
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;
use serde_json::Value;
use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};
use crate::ooxml::local_name;

const MAX_XMIND_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XMIND_ENTRIES: usize = 100_000;
const MAX_XMIND_PART_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XMIND_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_XMIND_XML_EVENTS: usize = 1_000_000;
const MAX_XMIND_XML_DEPTH: usize = 256;
const MAX_XMIND_TOPICS: usize = 200_000;
const MAX_XMIND_TEXT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn looks_like_xmind_archive(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut archive) = ZipArchive::new(file) else {
        return false;
    };
    (0..archive.len()).take(MAX_XMIND_ENTRIES).any(|index| {
        archive
            .by_index(index)
            .ok()
            .map(|entry| matches!(entry.name(), "content.xml" | "content.json"))
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
        options.max_input_bytes.min(MAX_XMIND_INPUT_BYTES),
        "XMind input",
    )?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("invalid XMind ZIP package: {error}")))?;
    if archive.len() > MAX_XMIND_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "XMind package contains more than {MAX_XMIND_ENTRIES} entries"
        )));
    }
    let mut expanded = 0u64;
    let mut warnings = vec![
        "XMind relationships, notes, attachments, hyperlinks, and canvas coordinates are omitted"
            .to_owned(),
    ];
    let blocks = if let Some(index) = find_entry(&mut archive, "content.xml")? {
        let bytes = read_entry(&mut archive, index)?;
        expanded = expanded.saturating_add(bytes.len() as u64);
        if expanded > MAX_XMIND_EXPANDED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XMind expanded parts exceed {MAX_XMIND_EXPANDED_BYTES} bytes"
            )));
        }
        parse_xml(&bytes, options.max_xml_events)?
    } else if let Some(index) = find_entry(&mut archive, "content.json")? {
        let bytes = read_entry(&mut archive, index)?;
        expanded = expanded.saturating_add(bytes.len() as u64);
        if expanded > MAX_XMIND_EXPANDED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XMind expanded parts exceed {MAX_XMIND_EXPANDED_BYTES} bytes"
            )));
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| Error::InvalidInput(format!("invalid XMind content.json: {error}")))?;
        parse_json(&value)?
    } else {
        return Err(Error::InvalidInput(
            "XMind package contains neither content.xml nor content.json".into(),
        ));
    };
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "XMind content contains no topic titles".into(),
        ));
    }
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn parse_xml(bytes: &[u8], max_events: usize) -> Result<Vec<HtmlBlock>> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<Vec<u8>>::new();
    let mut title = None::<(u8, String)>;
    let mut blocks = Vec::new();
    let mut topics = 0usize;
    let mut text_bytes = 0usize;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events.min(MAX_XMIND_XML_EVENTS) {
            return Err(Error::LimitExceeded(format!(
                "XMind content.xml exceeds {} parser events",
                max_events.min(MAX_XMIND_XML_EVENTS)
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref()).to_vec();
                if stack.len() >= MAX_XMIND_XML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "XMind XML nesting exceeds {MAX_XMIND_XML_DEPTH}"
                    )));
                }
                if name.as_slice() == b"topic" {
                    topics = topics.saturating_add(1);
                    if topics > MAX_XMIND_TOPICS {
                        return Err(Error::LimitExceeded(format!(
                            "XMind exceeds {MAX_XMIND_TOPICS} topics"
                        )));
                    }
                }
                if name.as_slice() == b"title"
                    && stack.iter().any(|item| item.as_slice() == b"topic")
                {
                    let level = stack
                        .iter()
                        .filter(|item| item.as_slice() == b"topic")
                        .count()
                        .clamp(1, 6) as u8;
                    title = Some((level, String::new()));
                }
                stack.push(name);
            }
            Event::Empty(element) => {
                let qualified = element.name();
                if local_name(qualified.as_ref()) == b"topic" {
                    topics = topics.saturating_add(1);
                }
            }
            Event::Text(text) => {
                if let Some((_, value)) = title.as_mut() {
                    let decoded = text.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid XMind title text: {error}"))
                    })?;
                    text_bytes = text_bytes.saturating_add(decoded.len());
                    if text_bytes > MAX_XMIND_TEXT_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "XMind rendered text exceeds {MAX_XMIND_TEXT_BYTES} bytes"
                        )));
                    }
                    value.push_str(&decoded);
                }
            }
            Event::CData(text) => {
                if let Some((_, value)) = title.as_mut() {
                    let decoded = text.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid XMind title CDATA: {error}"))
                    })?;
                    text_bytes = text_bytes.saturating_add(decoded.len());
                    if text_bytes > MAX_XMIND_TEXT_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "XMind rendered text exceeds {MAX_XMIND_TEXT_BYTES} bytes"
                        )));
                    }
                    value.push_str(&decoded);
                }
            }
            Event::End(element) => {
                let qualified = element.name();
                let name = local_name(qualified.as_ref());
                if name == b"title"
                    && let Some((level, value)) = title.take()
                {
                    let value = clean_text(&value);
                    if !value.is_empty() {
                        blocks.push(HtmlBlock::Heading { level, text: value });
                    }
                }
                stack.pop();
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "XMind document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(blocks)
}

fn parse_json(value: &Value) -> Result<Vec<HtmlBlock>> {
    let mut blocks = Vec::new();
    let root = value
        .get("rootTopic")
        .or_else(|| value.get("root_topic"))
        .unwrap_or(value);
    visit_json_topic(root, 1, &mut blocks, 0)?;
    Ok(blocks)
}

fn visit_json_topic(
    value: &Value,
    level: u8,
    blocks: &mut Vec<HtmlBlock>,
    count: usize,
) -> Result<usize> {
    if count >= MAX_XMIND_TOPICS {
        return Err(Error::LimitExceeded(format!(
            "XMind exceeds {MAX_XMIND_TOPICS} topics"
        )));
    }
    let mut count = count + 1;
    if let Some(title) = value.get("title").and_then(Value::as_str).map(clean_text)
        && !title.is_empty()
    {
        blocks.push(HtmlBlock::Heading {
            level: level.clamp(1, 6),
            text: title,
        });
    }
    if let Some(attached) = value
        .get("children")
        .and_then(|children| children.get("attached"))
        .and_then(Value::as_array)
    {
        for child in attached {
            count = visit_json_topic(child, level.saturating_add(1), blocks, count)?;
        }
    }
    Ok(count)
}

fn find_entry(archive: &mut ZipArchive<Cursor<Vec<u8>>>, name: &str) -> Result<Option<usize>> {
    for index in 0..archive.len() {
        if archive.by_index(index)?.name() == name {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn read_entry(archive: &mut ZipArchive<Cursor<Vec<u8>>>, index: usize) -> Result<Vec<u8>> {
    let mut entry = archive.by_index(index)?;
    if entry.is_dir() || entry.size() > MAX_XMIND_PART_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XMind content part exceeds {MAX_XMIND_PART_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut entry)
        .take(MAX_XMIND_PART_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_XMIND_PART_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XMind content part exceeds {MAX_XMIND_PART_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

fn clean_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
