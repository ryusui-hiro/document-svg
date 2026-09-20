//! Bounded MHTML (`.mht`/`.mhtml`) web-archive preview.
//!
//! The MIME root HTML part is parsed by the existing safe HTML subset and
//! typeset as SVG pages. Only validated, bounded PNG/JPEG resources referenced
//! by Content-ID or resolved Content-Location matches in the same or enclosing
//! multipart/related
//! are embedded; remote resources, scripts, stylesheets, and other attachments are omitted.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use mail_parser::{Message, MessageParser, MimeHeaders, PartType};

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::eml::{self, MAX_EML_BYTES};
use crate::document::html::{
    HtmlBlock, first_html_base_href, parse_html_blocks_with_inline_images_resolved_budgeted,
    render_blocks_to_pages,
};
use crate::document::mime_images::{
    MimeImageResources, collect_mime_images_with_uri_resolution, resolve_mime_uri,
};
use crate::error::{Error, Result};
use crate::ir::Page;

const MAX_MHTML_HTML_BYTES: usize = 32 * 1024 * 1024;
const MAX_MHTML_NORMALIZED_HTML_BYTES: usize = 64 * 1024 * 1024;
const MAX_MHTML_PARTS: usize = 20_000;

struct MhtmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    title: String,
    warnings: &'a [String],
}

#[derive(Clone, Copy)]
enum MhtmlBodyPart {
    Html { part_id: u32 },
    Text { part_id: u32 },
}

impl PageConsumer for MhtmlPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "mhtml".into();
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
    let max_bytes = options.max_input_bytes.min(MAX_EML_BYTES);
    let mut bytes = Vec::new();
    Read::take(File::open(path)?, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "MHTML input exceeds maximum limit of {max_bytes} bytes"
        )));
    }
    eml::preflight_lines(&bytes)?;
    let parser = MessageParser::default()
        .with_minimal_headers()
        .default_header_text();
    let message = parser.parse(&bytes).ok_or_else(|| {
        Error::InvalidInput("MHTML archive contains no parseable MIME message".into())
    })?;
    if message.parts.len() > MAX_MHTML_PARTS {
        return Err(Error::LimitExceeded(format!(
            "MHTML archive contains {} MIME parts; maximum is {MAX_MHTML_PARTS}",
            message.parts.len()
        )));
    }

    let title = message
        .subject()
        .map(clean_text)
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "MHTML web archive".into());
    let (selected_body, related_found, mut warnings) = select_mhtml_body(&message)?;
    let html_part_id = match selected_body {
        Some(MhtmlBodyPart::Html { part_id, .. }) => Some(part_id),
        _ => None,
    };
    let MimeImageResources {
        images: cid_images,
        part_ids: cid_part_ids,
        part_count: cid_part_count,
        warnings: image_warnings,
        base_uri: mime_base_uri,
    } = html_part_id
        .map(|html_part_id| collect_mime_images_with_uri_resolution(&message, Some(html_part_id)))
        .unwrap_or_default();
    warnings.extend(image_warnings);
    let mut used_cid_images = std::collections::HashSet::new();
    let mut blocks = match selected_body {
        Some(MhtmlBodyPart::Html { part_id }) => {
            let html = message
                .parts
                .get(part_id as usize)
                .and_then(|part| part.text_contents())
                .unwrap_or_default();
            let html = clean_text(html);
            if html.len() > MAX_MHTML_HTML_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "MHTML HTML part exceeds {MAX_MHTML_HTML_BYTES} bytes"
                )));
            }
            let mime_base_uri = mime_base_uri.unwrap_or_else(|| "thismessage:/".to_owned());
            let html_base_uri = if let Some(base_href) = first_html_base_href(
                &html,
                options.max_xml_events,
                MAX_MHTML_NORMALIZED_HTML_BYTES,
            )? {
                match resolve_mime_uri(&mime_base_uri, &base_href) {
                    Some(resolved) => resolved,
                    None => {
                        push_mhtml_warning_once(
                            &mut warnings,
                            "invalid or overlong HTML base URI was ignored",
                        );
                        mime_base_uri
                    }
                }
            } else {
                mime_base_uri
            };
            let (blocks, html_warnings, used_images) =
                parse_html_blocks_with_inline_images_resolved_budgeted(
                    &html,
                    options.max_xml_events,
                    MAX_MHTML_NORMALIZED_HTML_BYTES,
                    &cid_images,
                    &html_base_uri,
                )?;
            warnings.extend(html_warnings);
            used_cid_images = used_images;
            if html.to_ascii_lowercase().contains("<style") {
                warnings.push("MHTML CSS styling is not applied".into());
            }
            blocks
        }
        Some(MhtmlBodyPart::Text { part_id }) => {
            let text = clean_text(
                message
                    .parts
                    .get(part_id as usize)
                    .and_then(|part| part.text_contents())
                    .unwrap_or_default(),
            );
            if text.len() > MAX_MHTML_HTML_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "MHTML text part exceeds {MAX_MHTML_HTML_BYTES} bytes"
                )));
            }
            text.split("\n\n")
                .map(str::trim)
                .filter(|paragraph| !paragraph.is_empty())
                .map(|paragraph| HtmlBlock::Paragraph {
                    text: paragraph.to_owned(),
                })
                .collect()
        }
        None => Vec::new(),
    };
    if blocks.is_empty() {
        warnings.push(if related_found {
            "MHTML related root contains no supported HTML/text body".into()
        } else {
            "MHTML archive contains no supported HTML/text body".into()
        });
    }
    let used_cid_part_count = used_cid_images
        .iter()
        .filter_map(|key| cid_part_ids.get(key))
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .len();
    let omitted_resource_parts = message
        .attachment_count()
        .saturating_sub(used_cid_part_count)
        .max(cid_part_count.saturating_sub(used_cid_part_count));
    if omitted_resource_parts > 0 {
        warnings.push(format!(
            "{omitted_resource_parts} MHTML resource/attachment part(s) were omitted"
        ));
    }
    if message.parts.iter().any(|part| part.is_encoding_problem) {
        warnings.push("one or more MHTML MIME parts contain transfer-encoding errors".into());
    }
    if !title.is_empty() {
        blocks.insert(
            0,
            HtmlBlock::Heading {
                level: 1,
                text: title.clone(),
            },
        );
    }
    let mut page_sink = MhtmlPageSink {
        inner: sink,
        title,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn select_mhtml_body(message: &Message<'_>) -> Result<(Option<MhtmlBodyPart>, bool, Vec<String>)> {
    let mut related_found = false;
    let mut warnings = Vec::new();

    for related_part in &message.parts {
        let Some(content_type) = related_part.content_type() else {
            continue;
        };
        if !content_type.ctype().eq_ignore_ascii_case("multipart")
            || !content_type
                .subtype()
                .is_some_and(|subtype| subtype.eq_ignore_ascii_case("related"))
        {
            continue;
        }
        related_found = true;
        let children = related_part.sub_parts().unwrap_or_default();
        if children.is_empty() {
            warnings.push("MHTML multipart/related root has no body parts".into());
            continue;
        }

        let root_id = if let Some(start) = mime_parameter(content_type, "start") {
            let start = normalize_mhtml_content_id(start);
            let matches = children
                .iter()
                .filter(|child_id| {
                    message
                        .parts
                        .get(**child_id as usize)
                        .and_then(|part| part.content_id())
                        .is_some_and(|content_id| normalize_mhtml_content_id(content_id) == start)
                })
                .copied()
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [root_id] => *root_id,
                [] => {
                    return Err(Error::InvalidInput(format!(
                        "MHTML multipart/related start parameter '{start}' does not match a direct body part"
                    )));
                }
                _ => {
                    return Err(Error::InvalidInput(format!(
                        "MHTML multipart/related start parameter '{start}' is ambiguous"
                    )));
                }
            }
        } else {
            children[0]
        };

        let root_part = message.parts.get(root_id as usize).ok_or_else(|| {
            Error::InvalidInput("MHTML multipart/related root part index is invalid".into())
        })?;
        if let Some(expected_type) = mime_parameter(content_type, "type") {
            let actual_type = root_part.content_type().map(|part_type| {
                format!(
                    "{}/{}",
                    part_type.ctype(),
                    part_type.subtype().unwrap_or_default()
                )
            });
            if !actual_type
                .as_deref()
                .is_some_and(|actual| actual.eq_ignore_ascii_case(expected_type.trim()))
            {
                push_mhtml_warning_once(
                    &mut warnings,
                    "MHTML multipart/related type parameter does not match its root part",
                );
            }
        } else {
            push_mhtml_warning_once(
                &mut warnings,
                "MHTML multipart/related is missing its required type parameter",
            );
        }

        let descendants = mhtml_descendants(message, root_id);
        if let Some(body_part) = first_supported_body_in(message, &descendants) {
            return Ok((Some(body_part), true, warnings));
        }
        push_mhtml_warning_once(
            &mut warnings,
            "MHTML multipart/related root contains no supported HTML/text body",
        );
        return Ok((None, true, warnings));
    }

    if !related_found {
        if let Some(part_id) = message.html_body.first().copied() {
            warnings.push(
                "MHTML message has no multipart/related root; the first HTML body is used as a compatibility fallback".into(),
            );
            return Ok((Some(MhtmlBodyPart::Html { part_id }), false, warnings));
        }
        if let Some(part_id) = message.text_body.first().copied() {
            warnings.push(
                "MHTML message has no multipart/related root; the first text body is used as a compatibility fallback".into(),
            );
            return Ok((Some(MhtmlBodyPart::Text { part_id }), false, warnings));
        }
    }
    Ok((None, related_found, warnings))
}

fn first_supported_body_in(
    message: &Message<'_>,
    descendants: &std::collections::HashSet<usize>,
) -> Option<MhtmlBodyPart> {
    if let Some((part_id, _)) = message
        .parts
        .iter()
        .enumerate()
        .find(|(part_id, part)| descendants.contains(part_id) && part.is_text_html())
    {
        return Some(MhtmlBodyPart::Html {
            part_id: part_id as u32,
        });
    }
    message
        .parts
        .iter()
        .enumerate()
        .find(|(part_id, part)| {
            descendants.contains(part_id) && part.is_text() && !part.is_text_html()
        })
        .map(|(part_id, _)| MhtmlBodyPart::Text {
            part_id: part_id as u32,
        })
}

fn mhtml_descendants(message: &Message<'_>, root_id: u32) -> std::collections::HashSet<usize> {
    let mut descendants = std::collections::HashSet::new();
    let mut pending = vec![root_id as usize];
    while let Some(part_id) = pending.pop() {
        if !descendants.insert(part_id) {
            continue;
        }
        if let Some(part) = message.parts.get(part_id)
            && let PartType::Multipart(children) = &part.body
        {
            pending.extend(children.iter().map(|child_id| *child_id as usize));
        }
    }
    descendants
}

fn mime_parameter<'a>(
    content_type: &'a mail_parser::ContentType<'_>,
    name: &str,
) -> Option<&'a str> {
    content_type
        .attributes()?
        .iter()
        .find(|attribute| attribute.name.eq_ignore_ascii_case(name))
        .map(|attribute| attribute.value.as_ref())
}

fn normalize_mhtml_content_id(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_owned()
}

fn push_mhtml_warning_once(warnings: &mut Vec<String>, warning: impl Into<String>) {
    let warning = warning.into();
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

pub(crate) fn image_dimensions(bytes: &[u8], mime: &str) -> Option<(u32, u32)> {
    crate::document::mime_images::image_dimensions(bytes, mime)
}

fn clean_text(text: &str) -> String {
    text.chars()
        .map(|character| if character == '\r' { '\n' } else { character })
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}
