//! Bounded Microsoft Outlook `.msg` message preview.
//!
//! The CFB container is opened without extracting attachment payloads. Common
//! message properties and a safe HTML/plain-text body are rendered; embedded
//! content is never executed or fetched.

use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::Path;

use cfb::CompoundFile;
use encoding_rs::{Encoding, WINDOWS_1252};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::Page;

const MAX_MSG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MSG_ENTRIES: usize = 50_000;
const MAX_MSG_PATH_BYTES: usize = 1024;
const MAX_MSG_PATHS_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_MSG_TEXT_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn looks_like_msg_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() < 512 || metadata.len() > MAX_MSG_BYTES {
        return false;
    }
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if Read::take(file, MAX_MSG_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_MSG_BYTES
    {
        return false;
    }
    if bytes.len() < 8 || bytes[..8] != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return false;
    }
    let Ok(compound) = CompoundFile::open(Cursor::new(bytes.as_slice())) else {
        return false;
    };
    compound.exists("/__properties_version1.0")
}

struct MsgPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    title: String,
    warnings: &'a [String],
}

impl PageConsumer for MsgPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "msg".into();
        if page.title.is_empty() {
            page.title = self.title.clone();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_MSG_BYTES);
    let mut bytes = Vec::new();
    Read::take(File::open(path)?, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "Outlook MSG input exceeds maximum limit of {max_bytes} bytes"
        )));
    }
    if bytes.len() < 8 || bytes[..8] != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return Err(Error::InvalidInput(
            "Outlook MSG input is not a Compound File Binary document".into(),
        ));
    }

    let mut compound = CompoundFile::open(Cursor::new(bytes.as_slice())).map_err(|error| {
        Error::InvalidInput(format!("Outlook MSG compound file is invalid: {error}"))
    })?;
    validate_entries(&compound, bytes.len())?;
    if !compound.exists("/__properties_version1.0") {
        return Err(Error::InvalidInput(
            "Compound document does not contain the Outlook message property stream".into(),
        ));
    }

    let mut text_bytes = 0usize;
    let codepages = read_codepages(&mut compound, &mut text_bytes)?;
    let mut warnings = Vec::new();
    let metadata_codepage = codepages.message.or(codepages.internet);
    let body_codepage = codepages.internet.or(codepages.message);
    let subject = read_text_property(
        &mut compound,
        "0037",
        metadata_codepage,
        &mut text_bytes,
        &mut warnings,
    )?
    .unwrap_or_default()
    .trim()
    .to_owned();
    let title = if subject.is_empty() {
        "Outlook message".to_owned()
    } else {
        subject.clone()
    };
    let sender_name = read_text_property(
        &mut compound,
        "0C1A",
        metadata_codepage,
        &mut text_bytes,
        &mut warnings,
    )?;
    let sender_address = read_text_property(
        &mut compound,
        "0C1F",
        metadata_codepage,
        &mut text_bytes,
        &mut warnings,
    )?;
    let to = read_text_property(
        &mut compound,
        "0E04",
        metadata_codepage,
        &mut text_bytes,
        &mut warnings,
    )?;
    let cc = read_text_property(
        &mut compound,
        "0E03",
        metadata_codepage,
        &mut text_bytes,
        &mut warnings,
    )?;
    let bcc = read_text_property(
        &mut compound,
        "0E02",
        metadata_codepage,
        &mut text_bytes,
        &mut warnings,
    )?;
    let body = read_text_property(
        &mut compound,
        "1000",
        body_codepage,
        &mut text_bytes,
        &mut warnings,
    )?;
    let html = read_binary_property(
        &mut compound,
        "1013",
        codepages.internet,
        &mut text_bytes,
        &mut warnings,
    )?;

    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: title.clone(),
    }];
    let from = format_sender(sender_name.as_deref(), sender_address.as_deref());
    push_header(&mut blocks, "From", from.as_deref());
    push_header(&mut blocks, "To", to.as_deref());
    push_header(&mut blocks, "Cc", cc.as_deref());
    push_header(&mut blocks, "Bcc", bcc.as_deref());
    blocks.push(HtmlBlock::HorizontalRule);

    if let Some(html) = html.filter(|html| !html.trim().is_empty()) {
        if contains_ascii_case_insensitive(html.as_bytes(), b"<script")
            || contains_ascii_case_insensitive(html.as_bytes(), b"<style")
        {
            warnings.push("active script and style content in Outlook HTML was omitted".into());
        }
        let (html_blocks, html_warnings, _) =
            crate::document::html::parse_html_blocks_with_inline_images(
                &html,
                options.max_xml_events,
                MAX_MSG_TEXT_BYTES,
                &std::collections::HashMap::new(),
            )?;
        blocks.extend(html_blocks);
        warnings.extend(html_warnings);
        if body.as_ref().is_some_and(|body| !body.trim().is_empty()) {
            warnings.push(
                "Outlook plain-text alternative was omitted because an HTML body was present"
                    .into(),
            );
        }
    } else if let Some(body) = body.filter(|body| !body.trim().is_empty()) {
        append_plain_text(&mut blocks, &body);
    } else {
        warnings.push("Outlook message has no supported text body".into());
    }

    let attachment_count = count_attachments(&compound);
    if attachment_count > 0 {
        warnings.push(format!(
            "{attachment_count} Outlook attachment(s), including inline images, were omitted"
        ));
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Attachments: {attachment_count} omitted"),
        });
    }
    if compound.exists("/__substg1.0_007D001F")
        || compound.exists("/__substg1.0_007D001E")
        || compound.exists("/__substg1.0_007D0102")
    {
        warnings.push(
            "Outlook transport headers beyond sender and recipient fields were omitted".into(),
        );
    }

    let mut page_sink = MsgPageSink {
        inner: sink,
        title,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn validate_entries(compound: &CompoundFile<Cursor<&[u8]>>, file_bytes: usize) -> Result<()> {
    let mut entries = 0usize;
    let mut total_path_bytes = 0usize;
    for entry in compound.walk() {
        entries = entries.saturating_add(1);
        if entries > MAX_MSG_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "Outlook MSG contains more than {MAX_MSG_ENTRIES} compound entries"
            )));
        }
        let path_bytes = entry.path().to_string_lossy().len();
        if path_bytes > MAX_MSG_PATH_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Outlook MSG compound path exceeds {MAX_MSG_PATH_BYTES} bytes"
            )));
        }
        total_path_bytes = total_path_bytes.saturating_add(path_bytes);
        if total_path_bytes > MAX_MSG_PATHS_TOTAL_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Outlook MSG paths exceed {MAX_MSG_PATHS_TOTAL_BYTES} bytes"
            )));
        }
        if entry.is_stream() && entry.len() > file_bytes as u64 {
            return Err(Error::InvalidInput(
                "Outlook MSG stream declares more bytes than its container".into(),
            ));
        }
    }
    Ok(())
}

fn count_attachments(compound: &CompoundFile<Cursor<&[u8]>>) -> usize {
    compound
        .walk()
        .filter(|entry| {
            entry.is_storage()
                && entry
                    .name()
                    .to_ascii_lowercase()
                    .starts_with("__attach_version1.0_#")
        })
        .count()
}

fn read_text_property(
    compound: &mut CompoundFile<Cursor<&[u8]>>,
    property_id: &str,
    codepage: Option<u32>,
    total_bytes: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<Option<String>> {
    for suffix in ["001F", "001E"] {
        let path = format!("/__substg1.0_{property_id}{suffix}");
        if let Some(bytes) = read_named_stream(compound, &path, MAX_MSG_TEXT_BYTES, total_bytes)? {
            let value = if suffix == "001F" {
                if bytes.len() % 2 != 0 {
                    return Err(Error::InvalidInput(format!(
                        "Outlook Unicode property {property_id} has an odd byte length"
                    )));
                }
                let units = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .collect::<Vec<_>>();
                String::from_utf16_lossy(&units)
            } else {
                let encoding = codepage.and_then(encoding_for_codepage).unwrap_or_else(|| {
                    push_warning_once(
                        warnings,
                        "Outlook ANSI text used a missing or unsupported code page; Windows-1252 fallback was used",
                    );
                    WINDOWS_1252
                });
                let (decoded, had_errors) = encoding.decode_without_bom_handling(&bytes);
                if had_errors {
                    push_warning_once(
                        warnings,
                        "Outlook ANSI text contained invalid byte sequences for its declared code page",
                    );
                }
                decoded.into_owned()
            };
            return Ok(Some(clean_text(&value)));
        }
    }
    Ok(None)
}

fn read_binary_property(
    compound: &mut CompoundFile<Cursor<&[u8]>>,
    property_id: &str,
    codepage: Option<u32>,
    total_bytes: &mut usize,
    warnings: &mut Vec<String>,
) -> Result<Option<String>> {
    let path = format!("/__substg1.0_{property_id}0102");
    let Some(bytes) = read_named_stream(compound, &path, MAX_MSG_TEXT_BYTES, total_bytes)? else {
        return Ok(None);
    };
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let encoding = codepage.and_then(encoding_for_codepage);
    let (html, had_errors) = if let Some(encoding) = encoding {
        encoding.decode_without_bom_handling(bytes)
    } else if let Ok(html) = std::str::from_utf8(bytes) {
        (html.into(), false)
    } else {
        push_warning_once(
            warnings,
            "Outlook HTML body had no supported code page; UTF-8/Windows-1252 fallback was used",
        );
        WINDOWS_1252.decode_without_bom_handling(bytes)
    };
    if had_errors {
        push_warning_once(
            warnings,
            "Outlook HTML body contained invalid byte sequences for its declared code page",
        );
    }
    Ok(Some(html.into_owned()))
}

#[derive(Default)]
struct Codepages {
    internet: Option<u32>,
    message: Option<u32>,
}

fn read_codepages(
    compound: &mut CompoundFile<Cursor<&[u8]>>,
    total_bytes: &mut usize,
) -> Result<Codepages> {
    let Some(bytes) = read_named_stream(
        compound,
        "/__properties_version1.0",
        1024 * 1024,
        total_bytes,
    )?
    else {
        return Ok(Codepages::default());
    };
    if bytes.len() < 32 || (bytes.len() - 32) % 16 != 0 {
        return Err(Error::InvalidInput(
            "Outlook MSG property stream has an invalid header or entry length".into(),
        ));
    }
    let mut codepages = Codepages::default();
    for entry in bytes[32..].chunks_exact(16) {
        let property_type = u16::from_le_bytes([entry[0], entry[1]]);
        if property_type != 0x0003 {
            continue;
        }
        let property_id = u16::from_le_bytes([entry[2], entry[3]]);
        let value = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]);
        match property_id {
            0x3FDE => codepages.internet = Some(value),
            0x3FFD => codepages.message = Some(value),
            _ => {}
        }
    }
    Ok(codepages)
}

fn encoding_for_codepage(codepage: u32) -> Option<&'static Encoding> {
    let label = match codepage {
        65001 => "utf-8".to_owned(),
        932 => "shift_jis".to_owned(),
        936 => "gbk".to_owned(),
        949 => "euc-kr".to_owned(),
        950 => "big5".to_owned(),
        874 | 1250..=1258 => format!("windows-{codepage}"),
        _ => return None,
    };
    Encoding::for_label(label.as_bytes())
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn read_named_stream(
    compound: &mut CompoundFile<Cursor<&[u8]>>,
    path: &str,
    max_bytes: usize,
    total_bytes: &mut usize,
) -> Result<Option<Vec<u8>>> {
    if !compound.exists(path) {
        return Ok(None);
    }
    let mut stream = compound
        .open_stream(path)
        .map_err(|error| Error::InvalidInput(format!("cannot read Outlook MSG stream: {error}")))?;
    let mut bytes = Vec::new();
    Read::take(&mut stream, max_bytes.saturating_add(1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "Outlook MSG property stream exceeds {max_bytes} bytes"
        )));
    }
    *total_bytes = total_bytes
        .checked_add(bytes.len())
        .ok_or_else(|| Error::LimitExceeded("Outlook MSG text size overflowed".into()))?;
    if *total_bytes > MAX_MSG_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Outlook MSG text properties exceed {MAX_MSG_TEXT_BYTES} bytes"
        )));
    }
    Ok(Some(bytes))
}

fn format_sender(name: Option<&str>, address: Option<&str>) -> Option<String> {
    match (
        name.map(str::trim).filter(|value| !value.is_empty()),
        address.map(str::trim).filter(|value| !value.is_empty()),
    ) {
        (Some(name), Some(address)) => Some(format!("{name} <{address}>")),
        (Some(name), None) => Some(name.to_owned()),
        (None, Some(address)) => Some(address.to_owned()),
        (None, None) => None,
    }
}

fn push_header(blocks: &mut Vec<HtmlBlock>, label: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        blocks.push(HtmlBlock::Paragraph {
            text: format!("{label}: {value}"),
        });
    }
}

fn append_plain_text(blocks: &mut Vec<HtmlBlock>, body: &str) {
    for paragraph in body.split("\n\n") {
        let text = paragraph
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_owned();
        if !text.is_empty() {
            blocks.push(HtmlBlock::Paragraph { text });
        }
    }
}

fn clean_text(text: &str) -> String {
    text.chars()
        .map(|character| if character == '\r' { '\n' } else { character })
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
}
