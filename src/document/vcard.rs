//! Bounded vCard 2.1/3.0/4.0 contact-card preview.
//!
//! Content lines are unfolded before property value decoding, then common
//! contact properties are typeset as pages. Embedded media and external
//! resources are omitted and never fetched.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::Page;

const MAX_VCARD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_VCARD_PHYSICAL_LINES: usize = 1_000_000;
const MAX_VCARD_LOGICAL_LINES: usize = 500_000;
const MAX_VCARD_PROPERTIES_TOTAL: usize = 250_000;
const MAX_VCARD_LINE_BYTES: usize = 1024 * 1024;
const MAX_VCARD_COMPONENTS: usize = 100_000;
const MAX_VCARD_PROPERTIES_PER_CARD: usize = 20_000;
const MAX_VCARD_PARAMS_PER_PROPERTY: usize = 100;
const MAX_VCARD_TEXT_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
struct VcardProperty {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

#[derive(Clone, Debug)]
struct Vcard {
    version: String,
    properties: Vec<VcardProperty>,
    has_unsupported_properties: bool,
    has_media_properties: bool,
}

struct VcardPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    page_offset: usize,
    page_count: usize,
    title: String,
    warnings: &'a [String],
}

impl PageConsumer for VcardPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.number = page.number.saturating_add(self.page_offset);
        page.source_format = "vcard".into();
        if page.title.is_empty() {
            page.title = self.title.clone();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)?;
        self.page_count = self.page_count.saturating_add(1);
        Ok(())
    }
}

pub(crate) fn looks_like_vcard_prefix(bytes: &[u8]) -> bool {
    let mut text = String::from_utf8_lossy(bytes);
    if text.starts_with('\u{feff}') {
        text = std::borrow::Cow::Owned(text.trim_start_matches('\u{feff}').to_owned());
    }
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.eq_ignore_ascii_case("BEGIN:VCARD"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_VCARD_BYTES);
    let mut bytes = Vec::new();
    Read::take(File::open(path)?, max_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "vCard input exceeds maximum limit of {max_bytes} bytes"
        )));
    }

    let (cards, mut warnings) = parse_cards(&bytes, options.max_pages)?;
    if cards.is_empty() {
        return Err(Error::InvalidInput(
            "vCard input contains no BEGIN:VCARD / END:VCARD cards".into(),
        ));
    }

    let mut total_pages = 0usize;
    for (index, card) in cards.iter().enumerate() {
        let (title, blocks, card_warnings) = render_card(card, index + 1);
        let remaining_pages = options.max_pages.saturating_sub(total_pages);
        let mut card_options = options.clone();
        card_options.max_pages = remaining_pages;
        let mut page_sink = VcardPageSink {
            inner: sink,
            page_offset: total_pages,
            page_count: 0,
            title,
            warnings: &card_warnings,
        };
        render_blocks_to_pages(&blocks, &mut page_sink, &card_options)?;
        if page_sink.page_count == 0 {
            return Err(Error::InvalidInput(
                "vCard card produced no SVG pages".into(),
            ));
        }
        total_pages = total_pages
            .checked_add(page_sink.page_count)
            .ok_or_else(|| Error::LimitExceeded("vCard page count overflowed".into()))?;
        for warning in card_warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    Ok(warnings)
}

fn parse_cards(bytes: &[u8], max_pages: usize) -> Result<(Vec<Vcard>, Vec<String>)> {
    let legacy_folding = vcard_21_flags(bytes);
    let lines = unfold_lines(bytes, &legacy_folding)?;
    let mut cards = Vec::new();
    let mut active: Option<Vcard> = None;
    let mut total_properties = 0usize;
    let mut total_text_bytes = 0usize;
    let mut warnings = Vec::new();

    for (line_index, line) in lines.iter().enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if line.eq_ignore_ascii_case(b"BEGIN:VCARD") {
            if active.is_some() {
                return Err(Error::InvalidInput("vCard cards cannot be nested".into()));
            }
            if cards.len() >= max_pages.min(MAX_VCARD_COMPONENTS) {
                return Err(Error::LimitExceeded(format!(
                    "vCard file exceeds {} cards/pages",
                    max_pages.min(MAX_VCARD_COMPONENTS)
                )));
            }
            active = Some(Vcard {
                version: String::new(),
                properties: Vec::new(),
                has_unsupported_properties: false,
                has_media_properties: false,
            });
            continue;
        }
        if line.eq_ignore_ascii_case(b"END:VCARD") {
            let Some(card) = active.take() else {
                return Err(Error::InvalidInput(
                    "vCard END:VCARD has no matching BEGIN:VCARD".into(),
                ));
            };
            if !matches!(card.version.as_str(), "2.1" | "3.0" | "4.0") {
                return Err(Error::Unsupported(format!(
                    "vCard version {:?} is unsupported; versions 2.1, 3.0, and 4.0 are supported",
                    card.version
                )));
            }
            if !card.properties.iter().any(|property| property.name == "FN") {
                warnings.push(
                    "vCard is missing the required FN property; a display name is inferred from N when available".into(),
                );
            }
            if card.version == "4.0"
                && card
                    .properties
                    .first()
                    .is_none_or(|property| property.name != "VERSION" || property.value != "4.0")
            {
                warnings
                    .push("vCard 4.0 VERSION is not the first property after BEGIN:VCARD".into());
            }
            cards.push(card);
            continue;
        }

        let Some(card) = active.as_mut() else {
            return Err(Error::InvalidInput(format!(
                "vCard content line {} is outside a card",
                line_index + 1
            )));
        };
        total_properties = total_properties.saturating_add(1);
        if total_properties > MAX_VCARD_PROPERTIES_TOTAL {
            return Err(Error::LimitExceeded(format!(
                "vCard input exceeds {MAX_VCARD_PROPERTIES_TOTAL} properties"
            )));
        }
        if card.properties.len() >= MAX_VCARD_PROPERTIES_PER_CARD {
            return Err(Error::LimitExceeded(format!(
                "vCard card exceeds {MAX_VCARD_PROPERTIES_PER_CARD} properties"
            )));
        }
        let (property, property_warnings) = parse_property(line, line_index + 1)?;
        for warning in property_warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        total_text_bytes = total_text_bytes
            .checked_add(property.value.len())
            .ok_or_else(|| Error::LimitExceeded("vCard text size overflowed".into()))?;
        if total_text_bytes > MAX_VCARD_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "vCard property text exceeds {MAX_VCARD_TEXT_BYTES} bytes"
            )));
        }
        if property.name == "VERSION" {
            if !card.version.is_empty() {
                return Err(Error::InvalidInput(
                    "vCard has more than one VERSION property".into(),
                ));
            }
            card.version = property.value.clone();
        }
        if matches!(
            property.name.as_str(),
            "PHOTO" | "LOGO" | "SOUND" | "KEY" | "AGENT"
        ) {
            card.has_media_properties = true;
        }
        if !is_rendered_property(&property.name) {
            card.has_unsupported_properties = true;
        }
        card.properties.push(property);
    }

    if active.is_some() {
        return Err(Error::InvalidInput("vCard is missing END:VCARD".into()));
    }
    Ok((cards, warnings))
}

fn vcard_21_flags(bytes: &[u8]) -> Vec<bool> {
    let mut flags = Vec::new();
    let mut active_card = None;
    let mut in_qp_continuation = false;
    for (index, raw_line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        let mut line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if index == 0 {
            line = line.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(line);
        }
        if in_qp_continuation {
            in_qp_continuation = line.last() == Some(&b'=');
            continue;
        }
        if line.eq_ignore_ascii_case(b"BEGIN:VCARD") {
            flags.push(false);
            active_card = Some(flags.len() - 1);
        } else if let Some(card_index) = active_card {
            if let Some(colon) = line.iter().position(|byte| *byte == b':')
                && line[..colon].eq_ignore_ascii_case(b"VERSION")
            {
                flags[card_index] = line[colon + 1..].eq_ignore_ascii_case(b"2.1");
            }
            if line.eq_ignore_ascii_case(b"END:VCARD") {
                active_card = None;
            }
        }
        in_qp_continuation = line.last() == Some(&b'=') && is_quoted_printable_content_line(line);
    }
    flags
}

fn unfold_lines(bytes: &[u8], legacy_folding: &[bool]) -> Result<Vec<Vec<u8>>> {
    let mut physical_count = 0usize;
    let mut logical_lines = Vec::new();
    let mut current = Vec::new();
    let mut card_index = 0usize;
    let mut is_legacy_v21 = false;
    for raw_line in bytes.split(|byte| *byte == b'\n') {
        physical_count = physical_count.saturating_add(1);
        if physical_count > MAX_VCARD_PHYSICAL_LINES {
            return Err(Error::LimitExceeded(format!(
                "vCard input exceeds {MAX_VCARD_PHYSICAL_LINES} physical lines"
            )));
        }
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        if line.len() > MAX_VCARD_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "vCard physical line exceeds {MAX_VCARD_LINE_BYTES} bytes"
            )));
        }
        if current.last() == Some(&b'=') && is_quoted_printable_content_line(&current) {
            current.pop();
            let unfolded_len = current
                .len()
                .checked_add(line.len())
                .ok_or_else(|| Error::LimitExceeded("vCard line size overflowed".into()))?;
            if unfolded_len > MAX_VCARD_LINE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "unfolded vCard line exceeds {MAX_VCARD_LINE_BYTES} bytes"
                )));
            }
            current.extend_from_slice(line);
            continue;
        }
        if line
            .first()
            .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            if current.is_empty() {
                return Err(Error::InvalidInput(
                    "vCard folded line has no preceding content line".into(),
                ));
            }
            let unfolded_len = current
                .len()
                .checked_add(line.len().saturating_sub(1))
                .ok_or_else(|| Error::LimitExceeded("vCard line size overflowed".into()))?;
            if unfolded_len > MAX_VCARD_LINE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "unfolded vCard line exceeds {MAX_VCARD_LINE_BYTES} bytes"
                )));
            }
            if is_legacy_v21 {
                current.extend_from_slice(line);
            } else {
                current.extend_from_slice(&line[1..]);
            }
        } else {
            if !current.is_empty() {
                logical_lines.push(std::mem::take(&mut current));
                if logical_lines.len() > MAX_VCARD_LOGICAL_LINES {
                    return Err(Error::LimitExceeded(format!(
                        "vCard input exceeds {MAX_VCARD_LOGICAL_LINES} logical lines"
                    )));
                }
            }
            if line.eq_ignore_ascii_case(b"BEGIN:VCARD") {
                is_legacy_v21 = legacy_folding.get(card_index).copied().unwrap_or(false);
            } else if line.eq_ignore_ascii_case(b"END:VCARD") {
                card_index = card_index.saturating_add(1);
                is_legacy_v21 = false;
            }
            current.extend_from_slice(line);
        }
    }
    if !current.is_empty() {
        logical_lines.push(current);
    }
    if logical_lines.len() > MAX_VCARD_LOGICAL_LINES {
        return Err(Error::LimitExceeded(format!(
            "vCard input exceeds {MAX_VCARD_LOGICAL_LINES} logical lines"
        )));
    }
    if logical_lines
        .first()
        .is_some_and(|line| line.starts_with(&[0xef, 0xbb, 0xbf]))
    {
        logical_lines[0].drain(..3);
    }
    Ok(logical_lines)
}

fn is_quoted_printable_content_line(line: &[u8]) -> bool {
    let Some(colon) = line.iter().position(|byte| *byte == b':') else {
        return false;
    };
    String::from_utf8_lossy(&line[..colon])
        .to_ascii_uppercase()
        .split(';')
        .skip(1)
        .any(|param| param.trim() == "ENCODING=QUOTED-PRINTABLE")
}

fn parse_property(line: &[u8], line_number: usize) -> Result<(VcardProperty, Vec<String>)> {
    let Some(colon) = find_unquoted_colon_bytes(line) else {
        return Err(Error::InvalidInput(format!(
            "vCard content line {line_number} has no value separator"
        )));
    };
    let head = std::str::from_utf8(&line[..colon]).map_err(|error| {
        Error::InvalidInput(format!(
            "vCard content line {line_number} property header is not valid UTF-8: {error}"
        ))
    })?;
    let raw_value = &line[colon + 1..];
    let head_parts = split_unquoted(head, ';');
    let full_name = head_parts.first().copied().unwrap_or_default().trim();
    let name = full_name
        .rsplit_once('.')
        .map(|(_, name)| name)
        .unwrap_or(full_name)
        .to_ascii_uppercase();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::InvalidInput(format!(
            "vCard content line {line_number} has an invalid property name"
        )));
    }
    if head_parts.len().saturating_sub(1) > MAX_VCARD_PARAMS_PER_PROPERTY {
        return Err(Error::LimitExceeded(format!(
            "vCard content line {line_number} exceeds {MAX_VCARD_PARAMS_PER_PROPERTY} parameters"
        )));
    }
    let mut params = Vec::new();
    for param in head_parts.into_iter().skip(1) {
        let (key, value) = param.split_once('=').unwrap_or((param, ""));
        let key = key.trim().to_ascii_uppercase();
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(Error::InvalidInput(format!(
                "vCard content line {line_number} has an invalid parameter name"
            )));
        }
        params.push((key, unquote(value.trim())));
    }
    let (value, warnings) = decode_property_value(raw_value, &params, &name, line_number)?;
    Ok((
        VcardProperty {
            name,
            params,
            value,
        },
        warnings,
    ))
}

fn find_unquoted_colon_bytes(line: &[u8]) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (index, byte) in line.iter().copied().enumerate() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' && quoted {
            escaped = true;
        } else if byte == b'"' {
            quoted = !quoted;
        } else if byte == b':' && !quoted {
            return Some(index);
        }
    }
    None
}

fn decode_property_value(
    raw_value: &[u8],
    params: &[(String, String)],
    property_name: &str,
    line_number: usize,
) -> Result<(String, Vec<String>)> {
    let encoding = params
        .iter()
        .find(|(key, _)| key == "ENCODING")
        .map(|(_, value)| value.as_str());
    let (bytes, mut warnings) = if encoding.is_some_and(|value| {
        value.eq_ignore_ascii_case("QUOTED-PRINTABLE") || value.eq_ignore_ascii_case("QP")
    }) {
        (
            decode_quoted_printable(raw_value, property_name, line_number)?,
            Vec::new(),
        )
    } else {
        (raw_value.to_vec(), Vec::new())
    };
    let charset = params
        .iter()
        .find(|(key, _)| key == "CHARSET")
        .map(|(_, value)| value.as_bytes());
    if let Some(charset) = charset {
        if let Some(encoding) = encoding_rs::Encoding::for_label(charset) {
            let (decoded, had_errors) = encoding.decode_without_bom_handling(&bytes);
            if had_errors {
                warnings.push(format!(
                    "vCard content line {line_number} contains invalid bytes for its declared charset"
                ));
            }
            return Ok((decoded.into_owned(), warnings));
        }
        warnings.push(format!(
            "vCard content line {line_number} uses an unsupported charset; Windows-1252 fallback was used"
        ));
        return Ok((
            encoding_rs::WINDOWS_1252
                .decode_without_bom_handling(&bytes)
                .0
                .into_owned(),
            warnings,
        ));
    }
    match String::from_utf8(bytes) {
        Ok(value) => Ok((value, warnings)),
        Err(error) => {
            warnings.push(format!(
                "vCard content line {line_number} is not UTF-8 and has no supported CHARSET; Windows-1252 fallback was used"
            ));
            Ok((
                encoding_rs::WINDOWS_1252
                    .decode_without_bom_handling(error.as_bytes())
                    .0
                    .into_owned(),
                warnings,
            ))
        }
    }
}

fn decode_quoted_printable(
    input: &[u8],
    property_name: &str,
    line_number: usize,
) -> Result<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0usize;
    while index < input.len() {
        if input[index] != b'=' {
            output.push(input[index]);
            index += 1;
            continue;
        }
        let Some(pair) = input.get(index + 1..index + 3) else {
            return Err(Error::InvalidInput(format!(
                "vCard content line {line_number} ends with an incomplete quoted-printable escape"
            )));
        };
        let high = (pair[0] as char).to_digit(16);
        let low = (pair[1] as char).to_digit(16);
        let (Some(high), Some(low)) = (high, low) else {
            return Err(Error::InvalidInput(format!(
                "vCard content line {line_number} has an invalid quoted-printable escape"
            )));
        };
        let decoded = ((high << 4) | low) as u8;
        if decoded == b';' && matches!(property_name, "N" | "ADR" | "ORG") {
            output.extend_from_slice(b"\\;");
        } else {
            output.push(decoded);
        }
        index += 3;
    }
    Ok(output)
}

fn split_unquoted(text: &str, delimiter: char) -> Vec<&str> {
    let mut quoted = false;
    let mut escaped = false;
    let mut starts = vec![0];
    for (index, character) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quoted {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == delimiter && !quoted {
            starts.push(index + delimiter.len_utf8());
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts
                .get(index + 1)
                .map(|next| next - delimiter.len_utf8())
                .unwrap_or(text.len());
            &text[*start..end]
        })
        .collect()
}

fn unquote(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].replace("\\\"", "\"")
    } else {
        value.to_owned()
    }
}

fn unescape_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            match chars.next() {
                Some('n' | 'N') => output.push('\n'),
                Some('\\') => output.push('\\'),
                Some(',') => output.push(','),
                Some(';') => output.push(';'),
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => output.push('\\'),
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn render_card(card: &Vcard, index: usize) -> (String, Vec<HtmlBlock>, Vec<String>) {
    let mut warnings = Vec::new();
    let title = card
        .properties
        .iter()
        .find(|property| property.name == "FN")
        .map(|property| clean_text(&unescape_text(&property.value)))
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            card.properties
                .iter()
                .find(|property| property.name == "N")
                .map(|property| format_structured_name(&property.value))
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| format!("Contact {index}"));
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: title.clone(),
    }];

    for property in &card.properties {
        let type_label = property_type_label(property);
        let label_suffix = type_label
            .as_ref()
            .map(|value| format!(" ({value})"))
            .unwrap_or_default();
        match property.name.as_str() {
            "VERSION" | "FN" | "N" | "PHOTO" | "LOGO" | "SOUND" | "KEY" | "AGENT" => {}
            "ORG" => {
                let org = split_unescaped(&property.value, ';')
                    .iter()
                    .map(|part| unescape_text(part).trim().to_owned())
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" / ");
                push_labeled_paragraph(&mut blocks, "Organization", &org);
            }
            "TITLE" => {
                push_labeled_paragraph(&mut blocks, "Title", &unescape_text(&property.value))
            }
            "ROLE" => push_labeled_paragraph(&mut blocks, "Role", &unescape_text(&property.value)),
            "EMAIL" => push_labeled_paragraph(
                &mut blocks,
                &format!("Email{label_suffix}"),
                &unescape_text(&property.value),
            ),
            "TEL" => push_labeled_paragraph(
                &mut blocks,
                &format!("Phone{label_suffix}"),
                &unescape_text(&property.value),
            ),
            "ADR" => {
                let fields = split_unescaped(&property.value, ';');
                let address = fields
                    .iter()
                    .map(|field| unescape_text(field).trim().to_owned())
                    .filter(|field| !field.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                push_labeled_paragraph(&mut blocks, &format!("Address{label_suffix}"), &address);
            }
            "URL" | "SOURCE" => {
                push_labeled_paragraph(&mut blocks, &property.name, &unescape_text(&property.value))
            }
            "BDAY" => {
                push_labeled_paragraph(&mut blocks, "Birthday", &unescape_text(&property.value))
            }
            "ANNIVERSARY" => {
                push_labeled_paragraph(&mut blocks, "Anniversary", &unescape_text(&property.value))
            }
            "NOTE" => {
                let note = clean_text(&unescape_text(&property.value));
                for paragraph in note
                    .split('\n')
                    .map(str::trim)
                    .filter(|paragraph| !paragraph.is_empty())
                {
                    blocks.push(HtmlBlock::Paragraph {
                        text: paragraph.to_owned(),
                    });
                }
            }
            "CATEGORIES" | "NICKNAME" | "KIND" | "GEO" | "TZ" | "LANG" | "IMPP" | "RELATED"
            | "MEMBER" | "UID" | "REV" => {
                push_labeled_paragraph(&mut blocks, &property.name, &unescape_text(&property.value))
            }
            _ => {}
        }
    }

    if card.has_media_properties {
        warnings.push(
            "vCard photo, logo, sound, key, and agent content was omitted; no URI resources are fetched".into(),
        );
    }
    if card.has_unsupported_properties {
        warnings
            .push("one or more unsupported or non-rendered vCard properties were omitted".into());
    }
    (title, blocks, warnings)
}

fn split_unescaped(value: &str, delimiter: char) -> Vec<&str> {
    let mut starts = vec![0];
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == delimiter {
            starts.push(index + delimiter.len_utf8());
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let end = starts
                .get(index + 1)
                .map(|next| next - delimiter.len_utf8())
                .unwrap_or(value.len());
            &value[*start..end]
        })
        .collect()
}

fn format_structured_name(value: &str) -> String {
    let fields = split_unescaped(value, ';');
    let ordered = [3usize, 1, 2, 0, 4]; // Prefix, given, additional, family, suffix.
    ordered
        .iter()
        .filter_map(|index| fields.get(*index))
        .map(|field| unescape_text(field).trim().to_owned())
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn property_type_label(property: &VcardProperty) -> Option<String> {
    let mut labels = Vec::new();
    for (key, value) in &property.params {
        if key == "TYPE" {
            labels.extend(
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_ascii_lowercase),
            );
        } else if value.is_empty()
            && matches!(
                key.as_str(),
                "HOME"
                    | "WORK"
                    | "CELL"
                    | "VOICE"
                    | "FAX"
                    | "PAGER"
                    | "INTERNET"
                    | "POSTAL"
                    | "PARCEL"
                    | "DOM"
                    | "ISDN"
                    | "PCS"
                    | "TEXT"
                    | "VIDEO"
                    | "BBS"
                    | "MODEM"
                    | "CAR"
                    | "MSG"
            )
        {
            labels.push(key.to_ascii_lowercase());
        }
    }
    (!labels.is_empty()).then(|| labels.join(", "))
}

fn push_labeled_paragraph(blocks: &mut Vec<HtmlBlock>, label: &str, value: &str) {
    let value = clean_text(value);
    if !value.trim().is_empty() {
        blocks.push(HtmlBlock::Paragraph {
            text: format!("{label}: {}", value.trim()),
        });
    }
}

fn is_rendered_property(name: &str) -> bool {
    matches!(
        name,
        "VERSION"
            | "FN"
            | "N"
            | "NICKNAME"
            | "PHOTO"
            | "BDAY"
            | "ANNIVERSARY"
            | "ADR"
            | "TEL"
            | "EMAIL"
            | "IMPP"
            | "LANG"
            | "TZ"
            | "GEO"
            | "TITLE"
            | "ROLE"
            | "LOGO"
            | "ORG"
            | "MEMBER"
            | "RELATED"
            | "CATEGORIES"
            | "NOTE"
            | "REV"
            | "SOUND"
            | "UID"
            | "URL"
            | "KEY"
            | "SOURCE"
            | "KIND"
            | "AGENT"
    )
}

fn clean_text(text: &str) -> String {
    text.chars()
        .map(|character| if character == '\r' { '\n' } else { character })
        .filter(|character| matches!(character, '\n' | '\t') || !character.is_control())
        .collect()
}
