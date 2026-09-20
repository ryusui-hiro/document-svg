//! Bounded RFC 5322/MIME e-mail message preview.
//!
//! Headers and the first safe HTML or plain-text body are typeset as a document.
//! RFC 3676 flowed plain text is unwrapped while retaining quote depth.
//! Bounded PNG/JPEG MIME parts referenced by local Content-ID can be embedded;
//! other attachments and remote resources are never fetched.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;

use mail_parser::{Address, MessageParser, MimeHeaders};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{
    HtmlBlock, parse_html_blocks_with_inline_images_budgeted, render_blocks_to_pages,
};
use crate::document::mime_images::{MimeImageResources, collect_mime_images};
use crate::error::{Error, Result};
use crate::ir::Page;

pub(crate) const MAX_EML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EML_LINE_BYTES: usize = 1024 * 1024;
const MAX_EML_LINES: usize = 1_000_000;
const MAX_EML_HEADER_FIELDS: usize = 100_000;
const MAX_EML_PARTS: usize = 20_000;
const MAX_EML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_EML_NORMALIZED_HTML_BYTES: usize = 64 * 1024 * 1024;

struct EmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    source_title: String,
    warnings: &'a [String],
    source_format: &'static str,
    page_offset: usize,
    page_count: usize,
}

impl PageConsumer for EmlPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = page.number.saturating_add(self.page_offset);
        page.source_format = self.source_format.into();
        if page.title.is_empty() {
            page.title = self.source_title.clone();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)?;
        self.page_count = self.page_count.saturating_add(1);
        Ok(())
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_EML_BYTES);
    let mut bytes = Vec::new();
    Read::take(File::open(path)?, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "EML input exceeds maximum limit of {max_bytes} bytes"
        )));
    }
    let (warnings, _) = render_message_bytes(&bytes, options, "eml", 0, sink)?;
    Ok(warnings)
}

pub(crate) fn render_message_bytes(
    bytes: &[u8],
    options: &ConvertOptions,
    source_format: &'static str,
    page_offset: usize,
    sink: &mut dyn PageConsumer,
) -> Result<(Vec<String>, usize)> {
    preflight_lines(bytes)?;
    let parser = MessageParser::default()
        .with_minimal_headers()
        .default_header_text();
    let message = parser.parse(&bytes).ok_or_else(|| {
        Error::InvalidInput("mail message contains no parseable message headers".into())
    })?;
    if message.parts.len() > MAX_EML_PARTS {
        return Err(Error::LimitExceeded(format!(
            "mail message contains {} MIME parts; maximum is {MAX_EML_PARTS}",
            message.parts.len()
        )));
    }

    let subject = message
        .subject()
        .map(clean_text)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "(no subject)".into());
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: subject.clone(),
    }];
    push_header(&mut blocks, "From", message.from());
    push_header(&mut blocks, "To", message.to());
    push_header(&mut blocks, "Cc", message.cc());
    if let Some(date) = message.date() {
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Date: {}", date.to_rfc3339()),
        });
    }
    blocks.push(HtmlBlock::HorizontalRule);

    let html_body_indices = actual_html_body_positions(&message).collect::<Vec<_>>();
    let html_body_index = html_body_indices.first().copied();
    let html_body_part_id = html_body_index
        .and_then(|index| message.html_body.get(index))
        .copied();
    let MimeImageResources {
        images: cid_images,
        part_ids: cid_part_ids,
        part_count: cid_part_count,
        mut warnings,
        ..
    } = if let Some(html_body_part_id) = html_body_part_id {
        collect_mime_images(&message, Some(html_body_part_id))
    } else {
        MimeImageResources {
            images: HashMap::new(),
            part_ids: HashMap::new(),
            part_count: 0,
            warnings: Vec::new(),
            base_uri: None,
        }
    };
    let mut used_cid_images = HashSet::new();
    let html_body = html_body_index
        .and_then(|index| message.body_html(index))
        .map(|value| clean_text(&value));
    let mut displayed_body = false;
    if html_body.is_some() && message.text_body_count() > 0 {
        warnings.push(
            "HTML e-mail alternative is shown; plain-text alternatives are not included".into(),
        );
    }
    if let Some(html) = html_body.as_deref() {
        if html.len() > MAX_EML_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "decoded mail HTML body exceeds {MAX_EML_TEXT_BYTES} bytes"
            )));
        }
        let (html_blocks, html_warnings, used_images) =
            parse_html_blocks_with_inline_images_budgeted(
                html,
                options.max_xml_events,
                MAX_EML_NORMALIZED_HTML_BYTES,
                &cid_images,
            )?;
        displayed_body = !html_blocks.is_empty();
        blocks.extend(html_blocks);
        used_cid_images = used_images;
        for warning in html_warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        if html.to_ascii_lowercase().contains("<style")
            && !warnings
                .iter()
                .any(|warning| warning.contains("CSS styling"))
        {
            warnings.push("e-mail CSS styling is not applied".into());
        }
    } else if let Some(body) = message.body_text(0).map(|value| clean_text(&value)) {
        if body.len() > MAX_EML_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "decoded mail body exceeds {MAX_EML_TEXT_BYTES} bytes"
            )));
        }
        displayed_body = !body.trim().is_empty();
        if let Some(delsp) = flowed_text_delsp(&message) {
            append_flowed_body_blocks(&mut blocks, &body, delsp);
            warnings.push("RFC 3676 flowed plain text was unwrapped for preview".into());
        } else {
            append_body_blocks(&mut blocks, &body);
        }
    }
    if !displayed_body {
        warnings.push("mail message has no supported HTML or text body".into());
    }
    if html_body_indices.len() > 1 || (html_body.is_none() && message.text_body_count() > 1) {
        warnings.push("only the first available mail HTML or text body part is displayed".into());
    }
    let used_cid_part_count = used_cid_images
        .iter()
        .filter_map(|key| cid_part_ids.get(key))
        .copied()
        .collect::<HashSet<_>>()
        .len();
    let omitted_resource_parts = message
        .attachment_count()
        .saturating_sub(used_cid_part_count)
        .max(cid_part_count.saturating_sub(used_cid_part_count));
    if omitted_resource_parts > 0 {
        warnings.push(format!(
            "{omitted_resource_parts} mail attachment/resource part(s) were omitted"
        ));
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Attachments/resources: {omitted_resource_parts} omitted"),
        });
    }
    if message.parts.iter().any(|part| part.is_encoding_problem) {
        warnings.push("one or more mail MIME parts contain transfer-encoding errors".into());
    }

    let mut page_sink = EmlPageSink {
        inner: sink,
        source_title: subject,
        warnings: &warnings,
        source_format,
        page_offset,
        page_count: 0,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    let page_count = page_sink.page_count;
    drop(page_sink);
    Ok((warnings, page_count))
}

fn actual_html_body_positions<'message, 'data>(
    message: &'message mail_parser::Message<'data>,
) -> impl Iterator<Item = usize> + 'message {
    message
        .html_body
        .iter()
        .enumerate()
        .filter_map(|(position, part_id)| {
            message
                .parts
                .get(*part_id as usize)
                .is_some_and(|part| part.is_content_type("text", "html"))
                .then_some(position)
        })
}

fn push_header(blocks: &mut Vec<HtmlBlock>, label: &str, address: Option<&Address<'_>>) {
    if let Some(address) = address {
        let value = address
            .iter()
            .map(|entry| match (entry.name(), entry.address()) {
                (Some(name), Some(address)) => format!("{name} <{address}>"),
                (None, Some(address)) => address.to_owned(),
                (Some(name), None) => name.to_owned(),
                (None, None) => String::new(),
            })
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        if !value.is_empty() {
            blocks.push(HtmlBlock::Paragraph {
                text: format!("{label}: {value}"),
            });
        }
    }
}

fn append_body_blocks(blocks: &mut Vec<HtmlBlock>, body: &str) {
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

fn flowed_text_delsp(message: &mail_parser::Message<'_>) -> Option<bool> {
    let part = message
        .text_body
        .first()
        .and_then(|part_id| usize::try_from(*part_id).ok())
        .and_then(|part_id| message.parts.get(part_id))?;
    let content_type = part.content_type()?;
    if !content_type.c_type.eq_ignore_ascii_case("text")
        || !content_type
            .c_subtype
            .as_deref()
            .is_some_and(|subtype| subtype.eq_ignore_ascii_case("plain"))
        || !content_type
            .attribute("format")
            .is_some_and(|format| format.eq_ignore_ascii_case("flowed"))
    {
        return None;
    }
    Some(
        content_type
            .attribute("delsp")
            .is_some_and(|value| value.eq_ignore_ascii_case("yes")),
    )
}

fn append_flowed_body_blocks(blocks: &mut Vec<HtmlBlock>, body: &str, delsp: bool) {
    let mut current = String::new();
    let mut current_quote_depth = 0usize;
    let mut pending_flow = false;
    for raw_line in body.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let mut content = line;
        let mut quote_depth = 0usize;
        while let Some(rest) = content.strip_prefix('>') {
            quote_depth += 1;
            content = rest;
        }
        if content.starts_with(' ') {
            content = &content[1..];
        }
        let signature_separator = content == "-- ";
        let flowed = !signature_separator && content.ends_with(' ');
        let content = if flowed {
            content.strip_suffix(' ').unwrap_or(content)
        } else if signature_separator {
            "--"
        } else {
            content
        };

        if !current.is_empty() && current_quote_depth != quote_depth {
            flush_flowed_paragraph(blocks, &mut current);
            pending_flow = false;
        }
        if signature_separator {
            flush_flowed_paragraph(blocks, &mut current);
            pending_flow = false;
        }
        if content.is_empty() && !flowed {
            flush_flowed_paragraph(blocks, &mut current);
            pending_flow = false;
            continue;
        }
        if current.is_empty() {
            current_quote_depth = quote_depth;
            for _ in 0..quote_depth {
                current.push('>');
            }
            if quote_depth > 0 {
                current.push(' ');
            }
            current.push_str(content);
        } else if pending_flow {
            if !delsp {
                current.push(' ');
            }
            current.push_str(content);
        } else {
            flush_flowed_paragraph(blocks, &mut current);
            current_quote_depth = quote_depth;
            for _ in 0..quote_depth {
                current.push('>');
            }
            if quote_depth > 0 {
                current.push(' ');
            }
            current.push_str(content);
        }
        pending_flow = flowed;
        if !flowed {
            flush_flowed_paragraph(blocks, &mut current);
        }
    }
    flush_flowed_paragraph(blocks, &mut current);
}

fn flush_flowed_paragraph(blocks: &mut Vec<HtmlBlock>, current: &mut String) {
    let text = current.trim().to_owned();
    if !text.is_empty() {
        blocks.push(HtmlBlock::Paragraph { text });
    }
    current.clear();
}

fn clean_text(text: &str) -> String {
    let mut cleaned = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                cleaned.push('\n');
            }
            '\n' | '\t' => cleaned.push(character),
            character if !character.is_control() => cleaned.push(character),
            _ => {}
        }
    }
    cleaned
}

pub(crate) fn preflight_lines(bytes: &[u8]) -> Result<()> {
    let mut line_count = 0usize;
    let mut in_headers = true;
    let mut header_count = 0usize;
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        line_count += 1;
        if line_count > MAX_EML_LINES {
            return Err(Error::LimitExceeded(format!(
                "mail message exceeds {MAX_EML_LINES} lines"
            )));
        }
        if raw_line.len() > MAX_EML_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "mail message line exceeds {MAX_EML_LINE_BYTES} bytes"
            )));
        }
        if in_headers {
            let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
            if line.is_empty() {
                in_headers = false;
            } else if !line.first().is_some_and(u8::is_ascii_whitespace) && line.contains(&b':') {
                header_count += 1;
                if header_count > MAX_EML_HEADER_FIELDS {
                    return Err(Error::LimitExceeded(format!(
                        "mail message exceeds {MAX_EML_HEADER_FIELDS} top-level header fields"
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraphs(blocks: &[HtmlBlock]) -> Vec<&str> {
        blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Paragraph { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn normalizes_crlf_to_single_line_breaks() {
        assert_eq!(
            clean_text("one\r\ntwo\rthree\nfour"),
            "one\ntwo\nthree\nfour"
        );
    }

    #[test]
    fn unwraps_flowed_text_with_delsp_quote_and_signature_rules() {
        let body = "soft break \r\ncontinues\r\n\r\n From sender\r\n> quoted \r\n> line\r\n> -- \r\nSignature\r\n";
        let body = clean_text(body);
        let mut delsp_no = Vec::new();
        append_flowed_body_blocks(&mut delsp_no, &body, false);
        assert_eq!(
            paragraphs(&delsp_no),
            [
                "soft break continues",
                "From sender",
                "> quoted line",
                "> --",
                "Signature"
            ]
        );

        let mut delsp_yes = Vec::new();
        append_flowed_body_blocks(&mut delsp_yes, "word  \nwrap\n", true);
        assert_eq!(paragraphs(&delsp_yes), ["word wrap"]);
    }
}
