//! HTML document parser, typography typesetter, and paginated vector SVG renderer.
//!
//! Parses HTML markup (headings, paragraphs, blockquotes, lists, tables, pre/code),
//! performs flow-based line wrapping and page pagination, and renders standard vector SVG pages.

#![allow(clippy::collapsible_if)]

use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun};
use crate::ooxml::{attribute, local_name, sniff_image_mime};

const PAGE_WIDTH: f64 = 595.0; // A4 standard width (pt)
const PAGE_HEIGHT: f64 = 842.0; // A4 standard height (pt)
const MARGIN_LEFT: f64 = 54.0;
const MARGIN_TOP: f64 = 54.0;
const MARGIN_RIGHT: f64 = 54.0;
const MARGIN_BOTTOM: f64 = 54.0;
const CONTENT_WIDTH: f64 = PAGE_WIDTH - MARGIN_LEFT - MARGIN_RIGHT;
const CONTENT_HEIGHT: f64 = PAGE_HEIGHT - MARGIN_TOP - MARGIN_BOTTOM;
const DEFAULT_MAX_HTML_EVENTS: usize = 5_000_000;
const MAX_NORMALIZED_HTML_BYTES: usize = 512 * 1024 * 1024;
const MAX_HTML_IMAGE_REFERENCES: usize = 10_000;
const MAX_HTML_IMAGE_ELEMENTS: usize = 10_000;
const MAX_HTML_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HTML_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_HTML_TOTAL_DATA_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_HTML_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_HTML_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;

struct HtmlWarningSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for HtmlWarningSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Clone, Debug)]
pub enum HtmlBlock {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    ListItem {
        bullet: String,
        text: String,
    },
    StyledText {
        kind: HtmlTextBlockKind,
        runs: Vec<HtmlTextRun>,
    },
    CodeBlock {
        text: String,
    },
    Image {
        href: String,
        pixel_width: u32,
        pixel_height: u32,
        alt: String,
    },
    Table(crate::table::TableData),
    PageBreak,
    HorizontalRule,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HtmlTextStyle {
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub color: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HtmlTextRun {
    pub text: String,
    pub style: HtmlTextStyle,
}

#[derive(Clone, Debug)]
pub enum HtmlTextBlockKind {
    Heading(u8),
    Paragraph,
    ListItem { bullet: String },
}

#[derive(Clone, Debug)]
pub(crate) struct InlineHtmlImage {
    pub href: String,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

fn html_attribute(start: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .find_map(|item| {
            if !local_name(item.key.as_ref()).eq_ignore_ascii_case(name) {
                return None;
            }
            let fallback = String::from_utf8_lossy(item.value.as_ref()).into_owned();
            Some(
                item.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .map(|value| value.into_owned())
                    .unwrap_or(fallback),
            )
        })
}

/// Select an image URL from the HTML image attributes. Browsers choose a
/// `srcset` candidate from viewport density and layout; the SVG preview has no
/// viewport negotiation, so it deterministically uses the first candidate when
/// `src` is absent and reports that approximation to callers.
fn html_image_source(element: &quick_xml::events::BytesStart<'_>) -> Option<(String, bool)> {
    if let Some(source) = html_attribute(element, b"src")
        && !source.trim().is_empty()
    {
        return Some((source, false));
    }
    let srcset = html_attribute(element, b"srcset")?;
    for candidate in srcset.split(',') {
        let source = candidate.split_whitespace().next().unwrap_or_default();
        if !source.is_empty() {
            return Some((source.to_owned(), true));
        }
    }
    None
}

#[derive(Default)]
struct HtmlImageBudget {
    loaded_bytes: usize,
    data_uri_bytes: usize,
    pixels: u64,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    Read::take(&mut file, options.max_input_bytes.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "HTML input exceeds maximum bytes ({})",
            options.max_input_bytes
        )));
    }
    let text = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("HTML file is not valid UTF-8: {e}")))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base_dir = fs::canonicalize(parent)?;
    let normalized_limit = usize::try_from(options.max_input_bytes)
        .unwrap_or(usize::MAX)
        .min(MAX_NORMALIZED_HTML_BYTES);
    let (sources, source_limit_exceeded) = collect_html_image_sources_from_html_with_limit(
        &text,
        options.max_xml_events,
        normalized_limit,
        MAX_HTML_IMAGE_REFERENCES,
    )?;
    let (inline_images, mut warnings) =
        load_local_image_sources(&base_dir, sources, source_limit_exceeded)?;
    if !inline_images.is_empty() {
        push_html_warning_once(
            &mut warnings,
            "HTML image layout, CSS sizing, and inline wrapping are approximated as centered flow blocks",
        );
    }
    let (blocks, parser_warnings, _) = parse_html_blocks_with_inline_images(
        &text,
        options.max_xml_events,
        normalized_limit,
        &inline_images,
    )?;
    for warning in parser_warnings {
        push_html_warning_once(&mut warnings, &warning);
    }
    let mut warning_sink = HtmlWarningSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut warning_sink, options)?;
    Ok(warnings)
}

pub(crate) fn load_local_image_sources(
    base_dir: &Path,
    sources: Vec<String>,
    source_limit_exceeded: bool,
) -> Result<(HashMap<String, InlineHtmlImage>, Vec<String>)> {
    let mut warnings = Vec::new();
    if source_limit_exceeded {
        push_html_warning_once(
            &mut warnings,
            "image references exceeded the supported limit; remaining images were omitted",
        );
    }
    let mut budget = HtmlImageBudget::default();
    let mut inline_images = HashMap::new();
    let mut seen_sources = HashSet::new();
    for source in sources {
        if !seen_sources.insert(source.clone()) {
            if let Some(image) = inline_images.get(&source) {
                reserve_html_image_instance(image, &mut budget, &mut warnings);
            }
            continue;
        }
        if let Some(image) = load_local_html_image(base_dir, &source, &mut budget, &mut warnings)? {
            inline_images.insert(source, image);
        }
    }
    Ok((inline_images, warnings))
}

fn load_local_html_image(
    base_dir: &Path,
    source: &str,
    budget: &mut HtmlImageBudget,
    warnings: &mut Vec<String>,
) -> Result<Option<InlineHtmlImage>> {
    let Some(path) = local_html_image_path(base_dir, source) else {
        let warning = if source.contains("://") || source.starts_with("//") {
            "external image resources are not fetched"
        } else {
            "image paths outside the input directory or with unsupported URI schemes were omitted"
        };
        push_html_warning_once(warnings, warning);
        return Ok(None);
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => {
            push_html_warning_once(warnings, "non-file image resources were omitted");
            return Ok(None);
        }
        Err(_) => {
            push_html_warning_once(warnings, "missing local image resources were omitted");
            return Ok(None);
        }
    };
    if metadata.len() > MAX_HTML_IMAGE_BYTES {
        push_html_warning_once(
            warnings,
            "local image resources exceeding the per-image byte limit were omitted",
        );
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let mut bytes = Vec::new();
    Read::take(&mut file, MAX_HTML_IMAGE_BYTES.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_HTML_IMAGE_BYTES {
        push_html_warning_once(
            warnings,
            "local image resources expanded beyond the per-image byte limit and were omitted",
        );
        return Ok(None);
    }
    let Some(mime) =
        sniff_image_mime(&bytes).filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
    else {
        push_html_warning_once(
            warnings,
            "unsupported local image types were omitted; only PNG and JPEG are embedded",
        );
        return Ok(None);
    };
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_html_warning_once(warnings, "invalid local PNG/JPEG images were omitted");
        return Ok(None);
    };
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || pixels > MAX_HTML_IMAGE_PIXELS {
        push_html_warning_once(
            warnings,
            "HTML images exceeding the per-image pixel limit were omitted",
        );
        return Ok(None);
    }
    let next_loaded_bytes = budget.loaded_bytes.saturating_add(bytes.len());
    if next_loaded_bytes > MAX_HTML_TOTAL_IMAGE_BYTES {
        push_html_warning_once(
            warnings,
            "HTML images exceeding the total loaded image byte limit were omitted",
        );
        return Ok(None);
    }
    let prefix = format!("data:{mime};base64,");
    let uri_bytes = prefix
        .len()
        .saturating_add(bytes.len().div_ceil(3).saturating_mul(4));
    if !reserve_html_image_dimensions(width, height, uri_bytes, budget, warnings) {
        return Ok(None);
    }
    budget.loaded_bytes = next_loaded_bytes;
    Ok(Some(InlineHtmlImage {
        href: format!("{prefix}{}", BASE64_STANDARD.encode(&bytes)),
        pixel_width: width,
        pixel_height: height,
    }))
}

fn reserve_html_image_instance(
    image: &InlineHtmlImage,
    budget: &mut HtmlImageBudget,
    warnings: &mut Vec<String>,
) -> bool {
    reserve_html_image_dimensions(
        image.pixel_width,
        image.pixel_height,
        image.href.len(),
        budget,
        warnings,
    )
}

fn reserve_html_image_dimensions(
    width: u32,
    height: u32,
    uri_bytes: usize,
    budget: &mut HtmlImageBudget,
    warnings: &mut Vec<String>,
) -> bool {
    let next_pixels = budget
        .pixels
        .saturating_add(u64::from(width) * u64::from(height));
    if next_pixels > MAX_HTML_TOTAL_IMAGE_PIXELS {
        push_html_warning_once(
            warnings,
            "HTML images exceeding the total decoded pixel limit were omitted",
        );
        return false;
    }
    let next_uri_bytes = budget.data_uri_bytes.saturating_add(uri_bytes);
    if next_uri_bytes > MAX_HTML_TOTAL_DATA_URI_BYTES {
        push_html_warning_once(
            warnings,
            "HTML images exceeding the total data URI byte limit were omitted",
        );
        return false;
    }
    budget.pixels = next_pixels;
    budget.data_uri_bytes = next_uri_bytes;
    true
}

fn local_html_image_path(base_dir: &Path, source: &str) -> Option<PathBuf> {
    crate::local_resource::resolve_relative_file(base_dir, source)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn push_html_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn is_html_void_tag(name: &[u8]) -> bool {
    matches!(
        name,
        b"area"
            | b"base"
            | b"br"
            | b"col"
            | b"embed"
            | b"hr"
            | b"img"
            | b"input"
            | b"link"
            | b"meta"
            | b"param"
            | b"source"
            | b"track"
            | b"wbr"
    )
}

pub(crate) fn escape_bare_ampersands_limited(html: &str, max_bytes: usize) -> Result<String> {
    if html.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "HTML input exceeds normalized size limit of {max_bytes} bytes"
        )));
    }
    let mut out = String::with_capacity(html.len().min(max_bytes));
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'&' {
            let mut j = i + 1;
            let mut is_entity = false;
            if j < bytes.len() && bytes[j] == b'#' {
                j += 1;
                if j < bytes.len() && (bytes[j] == b'x' || bytes[j] == b'X') {
                    j += 1;
                    let hex_start = j;
                    while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                        j += 1;
                    }
                    if j > hex_start && j < bytes.len() && bytes[j] == b';' {
                        is_entity = true;
                    }
                } else {
                    let dec_start = j;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > dec_start && j < bytes.len() && bytes[j] == b';' {
                        is_entity = true;
                    }
                }
            } else {
                let name_start = j;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                if j > name_start && j < bytes.len() && bytes[j] == b';' {
                    is_entity = true;
                }
            }

            if is_entity {
                push_html_bounded(&mut out, "&", max_bytes)?;
            } else {
                push_html_bounded(&mut out, "&amp;", max_bytes)?;
            }
            i += 1;
        } else {
            let Some(character) = html[i..].chars().next() else {
                break;
            };
            let mut encoded = [0u8; 4];
            push_html_bounded(&mut out, character.encode_utf8(&mut encoded), max_bytes)?;
            i += character.len_utf8();
        }
    }
    Ok(out)
}

fn push_html_bounded(output: &mut String, value: &str, max_bytes: usize) -> Result<()> {
    if output.len().saturating_add(value.len()) > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "HTML entity normalization exceeds {max_bytes} bytes"
        )));
    }
    output.push_str(value);
    Ok(())
}

pub(crate) fn normalize_html_void_tags_limited(html: &str, max_bytes: usize) -> Result<String> {
    if html.len() > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "HTML input exceeds normalized size limit of {max_bytes} bytes"
        )));
    }
    let bytes = html.as_bytes();
    let mut output = Vec::with_capacity(html.len().min(max_bytes));
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != b'<' || cursor + 1 >= bytes.len() {
            push_html_bytes_bounded(&mut output, &bytes[cursor..cursor + 1], max_bytes)?;
            cursor += 1;
            continue;
        }
        if bytes[cursor..].starts_with(b"<!--") {
            let end = find_bytes(&bytes[cursor + 4..], b"-->")
                .map(|offset| cursor + 4 + offset + 2)
                .ok_or_else(|| Error::InvalidInput("HTML comment is not terminated".into()))?;
            push_html_bytes_bounded(&mut output, &bytes[cursor..=end], max_bytes)?;
            cursor = end + 1;
            continue;
        }
        if bytes[cursor..].starts_with(b"<![CDATA[") {
            let end = find_bytes(&bytes[cursor + 9..], b"]]>")
                .map(|offset| cursor + 9 + offset + 2)
                .ok_or_else(|| Error::InvalidInput("HTML CDATA block is not terminated".into()))?;
            push_html_bytes_bounded(&mut output, &bytes[cursor..=end], max_bytes)?;
            cursor = end + 1;
            continue;
        }
        if matches!(bytes[cursor + 1], b'/' | b'!' | b'?') {
            let Some(end) = find_html_tag_end(bytes, cursor + 2) else {
                push_html_bytes_bounded(&mut output, &bytes[cursor..], max_bytes)?;
                break;
            };
            push_html_bytes_bounded(&mut output, &bytes[cursor..=end], max_bytes)?;
            cursor = end + 1;
            continue;
        }
        let name_start = cursor + 1;
        let mut name_end = name_start;
        while name_end < bytes.len()
            && (bytes[name_end].is_ascii_alphanumeric()
                || matches!(bytes[name_end], b':' | b'-' | b'_'))
        {
            name_end += 1;
        }
        if name_end == name_start {
            push_html_bytes_bounded(&mut output, &bytes[cursor..cursor + 1], max_bytes)?;
            cursor += 1;
            continue;
        }
        let Some(end) = find_html_tag_end(bytes, name_end) else {
            push_html_bytes_bounded(&mut output, &bytes[cursor..], max_bytes)?;
            break;
        };
        let name = &bytes[name_start..name_end];
        let is_void = is_html_void_tag(&name.to_ascii_lowercase());
        let before_close = &bytes[name_end..end];
        let already_self_closed = before_close
            .iter()
            .rev()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(&b'/');
        push_html_bytes_bounded(&mut output, &bytes[cursor..end], max_bytes)?;
        if is_void && !already_self_closed {
            push_html_bytes_bounded(&mut output, b"/", max_bytes)?;
        }
        push_html_bytes_bounded(&mut output, b">", max_bytes)?;
        cursor = end + 1;
    }
    String::from_utf8(output)
        .map_err(|_| Error::InvalidInput("HTML normalization produced invalid UTF-8".into()))
}

fn push_html_bytes_bounded(output: &mut Vec<u8>, value: &[u8], max_bytes: usize) -> Result<()> {
    if output.len().saturating_add(value.len()) > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "HTML tag normalization exceeds {max_bytes} bytes"
        )));
    }
    output.extend_from_slice(value);
    Ok(())
}

fn find_html_tag_end(bytes: &[u8], mut cursor: usize) -> Option<usize> {
    let mut quote = None;
    while cursor < bytes.len() {
        match (quote, bytes[cursor]) {
            (Some(open), byte) if byte == open => quote = None,
            (None, b'\'' | b'"') => quote = Some(bytes[cursor]),
            (None, b'>') => return Some(cursor),
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn normalize_cid_reference(source: &str) -> Option<String> {
    let cid = source
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("cid:"))
        .then(|| &source[4..])?;
    let cid = cid.trim().trim_matches(['<', '>']);
    let bytes = cid.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_nibble(bytes[index + 1]), hex_nibble(bytes[index + 2]))
        {
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .ok()
        .filter(|value| !value.is_empty())
}

fn append_html_text(target: &mut String, text: &str, in_pre: bool) {
    if in_pre {
        target.push_str(text);
        return;
    }
    let starts_with_ws = text.starts_with(|c: char| c.is_whitespace());
    let ends_with_ws = text.ends_with(|c: char| c.is_whitespace());
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        if (starts_with_ws || ends_with_ws)
            && !target.is_empty()
            && !target.ends_with(' ')
            && !target.ends_with('\n')
        {
            target.push(' ');
        }
        return;
    }
    if starts_with_ws && !target.is_empty() && !target.ends_with(' ') && !target.ends_with('\n') {
        target.push(' ');
    }
    for (idx, word) in words.iter().enumerate() {
        if idx > 0 && !target.ends_with(' ') && !target.ends_with('\n') {
            target.push(' ');
        }
        target.push_str(word);
    }
    if ends_with_ws && !target.ends_with(' ') && !target.ends_with('\n') {
        target.push(' ');
    }
}

pub fn parse_html_blocks(html: &str) -> Result<Vec<HtmlBlock>> {
    parse_html_blocks_with_limit(html, DEFAULT_MAX_HTML_EVENTS)
}

pub(crate) fn parse_html_blocks_with_limit(
    html: &str,
    max_events: usize,
) -> Result<Vec<HtmlBlock>> {
    parse_html_blocks_with_limits(html, max_events, MAX_NORMALIZED_HTML_BYTES)
}

pub(crate) fn parse_html_blocks_with_limits(
    html: &str,
    max_events: usize,
    max_normalized_bytes: usize,
) -> Result<Vec<HtmlBlock>> {
    parse_html_blocks_with_inline_images(html, max_events, max_normalized_bytes, &HashMap::new())
        .map(|(blocks, _, _)| blocks)
}

pub(crate) fn collect_html_image_sources_with_limit(
    html: &str,
    max_events: usize,
    max_sources: usize,
) -> Result<(Vec<String>, bool)> {
    let mut reader = Reader::from_str(html);
    let mut buffer = Vec::new();
    let mut sources = Vec::new();
    let mut events = 0usize;
    let mut exceeded_limit = false;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "HTML image source scan exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(ref element) | Event::Empty(ref element)
                if local_name(element.name().as_ref()).eq_ignore_ascii_case(b"img") =>
            {
                if let Some((source, _)) = html_image_source(element)
                    && !source.trim().is_empty()
                {
                    if sources.len() < max_sources {
                        sources.push(source);
                    } else {
                        exceeded_limit = true;
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok((sources, exceeded_limit))
}

pub(crate) fn first_html_base_href(
    html: &str,
    max_events: usize,
    max_normalized_bytes: usize,
) -> Result<Option<String>> {
    let normalized = normalize_html_void_tags_limited(html, max_normalized_bytes)?;
    let normalized = escape_bare_ampersands_limited(&normalized, max_normalized_bytes)?;
    let mut reader = Reader::from_str(&normalized);
    let mut buffer = Vec::new();
    let mut before_body = true;
    let mut head_closed = false;
    let mut template_depth = 0usize;
    let mut events = 0usize;
    loop {
        events = events.saturating_add(1);
        if events > max_events {
            return Err(Error::LimitExceeded(format!(
                "HTML base URI scan exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buffer)? {
            Event::Start(ref element) | Event::Empty(ref element) => {
                let qname = element.name();
                let name = local_name(qname.as_ref());
                if name.eq_ignore_ascii_case(b"body") {
                    before_body = false;
                } else if name.eq_ignore_ascii_case(b"head") {
                    head_closed = false;
                } else if name.eq_ignore_ascii_case(b"template") {
                    template_depth = template_depth.saturating_add(1);
                } else if before_body
                    && !head_closed
                    && template_depth == 0
                    && name.eq_ignore_ascii_case(b"base")
                {
                    if let Some(href) = html_attribute(element, b"href") {
                        if !href.trim().is_empty() {
                            return Ok(Some(href.trim().to_owned()));
                        }
                    }
                }
            }
            Event::End(ref element)
                if local_name(element.name().as_ref()).eq_ignore_ascii_case(b"body") =>
            {
                before_body = false;
            }
            Event::End(ref element)
                if local_name(element.name().as_ref()).eq_ignore_ascii_case(b"head") =>
            {
                head_closed = true;
            }
            Event::End(ref element)
                if local_name(element.name().as_ref()).eq_ignore_ascii_case(b"template") =>
            {
                template_depth = template_depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    Ok(None)
}

fn collect_html_image_sources_from_html_with_limit(
    html: &str,
    max_events: usize,
    max_normalized_bytes: usize,
    max_sources: usize,
) -> Result<(Vec<String>, bool)> {
    let normalized = normalize_html_void_tags_limited(html, max_normalized_bytes)?;
    let normalized = escape_bare_ampersands_limited(&normalized, max_normalized_bytes)?;
    collect_html_image_sources_with_limit(&normalized, max_events, max_sources)
}

pub(crate) fn parse_html_blocks_with_inline_images(
    html: &str,
    max_events: usize,
    max_normalized_bytes: usize,
    inline_images: &HashMap<String, InlineHtmlImage>,
) -> Result<(Vec<HtmlBlock>, Vec<String>, HashSet<String>)> {
    parse_html_blocks_with_inline_images_impl(
        html,
        max_events,
        max_normalized_bytes,
        inline_images,
        false,
        None,
    )
}

pub(crate) fn parse_html_blocks_with_inline_images_budgeted(
    html: &str,
    max_events: usize,
    max_normalized_bytes: usize,
    inline_images: &HashMap<String, InlineHtmlImage>,
) -> Result<(Vec<HtmlBlock>, Vec<String>, HashSet<String>)> {
    parse_html_blocks_with_inline_images_impl(
        html,
        max_events,
        max_normalized_bytes,
        inline_images,
        true,
        None,
    )
}

pub(crate) fn parse_html_blocks_with_inline_images_resolved_budgeted(
    html: &str,
    max_events: usize,
    max_normalized_bytes: usize,
    inline_images: &HashMap<String, InlineHtmlImage>,
    base_uri: &str,
) -> Result<(Vec<HtmlBlock>, Vec<String>, HashSet<String>)> {
    parse_html_blocks_with_inline_images_impl(
        html,
        max_events,
        max_normalized_bytes,
        inline_images,
        true,
        Some(base_uri),
    )
}

fn parse_html_blocks_with_inline_images_impl(
    html: &str,
    max_events: usize,
    max_normalized_bytes: usize,
    inline_images: &HashMap<String, InlineHtmlImage>,
    budget_inline_instances: bool,
    inline_image_base_uri: Option<&str>,
) -> Result<(Vec<HtmlBlock>, Vec<String>, HashSet<String>)> {
    let html = normalize_html_void_tags_limited(html, max_normalized_bytes)?;
    let clean_html = escape_bare_ampersands_limited(&html, max_normalized_bytes)?;
    let mut reader = Reader::from_str(&clean_html);
    let mut buf = Vec::new();

    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    let mut used_inline_images = HashSet::new();
    let mut inline_image_budget = HtmlImageBudget::default();
    let mut image_element_count = 0usize;
    let mut current_tag: Vec<Vec<u8>> = Vec::new();
    let mut current_text = String::new();
    let mut list_stack: Vec<usize> = Vec::new();
    let mut ignore_depth = 0usize;

    let mut in_table = false;
    let mut in_th = false;
    let mut in_td = false;
    let mut row_had_th = false;
    let mut current_colspan = 1usize;
    let mut current_cell_align: Option<crate::table::TableAlign> = None;
    let mut table_col_alignments: Vec<Option<crate::table::TableAlign>> = Vec::new();
    let mut table_headers: Vec<String> = Vec::new();
    let mut table_rows: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_cell = String::new();

    let flush_text = |blocks: &mut Vec<HtmlBlock>, current_text: &mut String| {
        let trimmed = current_text.trim();
        if !trimmed.is_empty() {
            blocks.push(HtmlBlock::Paragraph {
                text: trimmed.to_string(),
            });
        }
        current_text.clear();
    };

    let flush_li =
        |blocks: &mut Vec<HtmlBlock>, current_text: &mut String, list_stack: &mut Vec<usize>| {
            let trimmed = current_text.trim();
            if !trimmed.is_empty() {
                let depth = list_stack.len().saturating_sub(1);
                let indent = "  ".repeat(depth);
                let bullet = if let Some(counter) = list_stack.last_mut() {
                    if *counter > 0 {
                        let b = format!("{indent}{}. ", *counter);
                        *counter += 1;
                        b
                    } else {
                        format!("{indent}• ")
                    }
                } else {
                    "• ".to_string()
                };
                blocks.push(HtmlBlock::ListItem {
                    bullet,
                    text: trimmed.to_string(),
                });
            }
            current_text.clear();
        };

    let mut event_count = 0usize;
    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "HTML document exceeds {max_events} parser events"
            )));
        }
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(event) => match event {
                Event::Start(ref e) => {
                    let qname = e.name();
                    let name = local_name(qname.as_ref()).to_vec();

                    if matches!(
                        name.as_slice(),
                        b"head" | b"style" | b"script" | b"noscript" | b"template" | b"svg"
                    ) {
                        ignore_depth += 1;
                    }

                    if ignore_depth == 0 {
                        let in_li = current_tag.iter().any(|t| t.as_slice() == b"li");
                        if name == b"ol" || name == b"ul" {
                            if in_li {
                                flush_li(&mut blocks, &mut current_text, &mut list_stack);
                            } else {
                                flush_text(&mut blocks, &mut current_text);
                            }
                            if name == b"ol" {
                                let start = attribute(e, b"start")
                                    .and_then(|s| s.parse::<usize>().ok())
                                    .unwrap_or(1);
                                list_stack.push(start);
                            } else {
                                list_stack.push(0);
                            }
                        } else if name == b"li" {
                            if in_li {
                                flush_li(&mut blocks, &mut current_text, &mut list_stack);
                            } else {
                                flush_text(&mut blocks, &mut current_text);
                            }
                        } else if matches!(
                            name.as_slice(),
                            b"h1"
                                | b"h2"
                                | b"h3"
                                | b"h4"
                                | b"h5"
                                | b"h6"
                                | b"p"
                                | b"pre"
                                | b"blockquote"
                                | b"table"
                                | b"div"
                                | b"section"
                                | b"article"
                                | b"main"
                                | b"header"
                                | b"footer"
                                | b"aside"
                                | b"nav"
                                | b"figure"
                                | b"dl"
                                | b"dt"
                                | b"dd"
                        ) {
                            flush_text(&mut blocks, &mut current_text);
                        } else if name == b"table" {
                            in_table = true;
                            table_headers.clear();
                            table_rows.clear();
                            table_col_alignments.clear();
                            current_row.clear();
                            current_cell.clear();
                        } else if name == b"tr" {
                            current_row.clear();
                            row_had_th = false;
                        } else if name == b"th" || name == b"td" {
                            if name == b"th" {
                                in_th = true;
                                row_had_th = true;
                            } else {
                                in_td = true;
                            }
                            current_cell.clear();
                            current_colspan = attribute(e, b"colspan")
                                .and_then(|s| s.parse::<usize>().ok())
                                .unwrap_or(1)
                                .max(1);
                            current_cell_align = if let Some(a) = attribute(e, b"align") {
                                match a.to_ascii_lowercase().as_str() {
                                    "right" => Some(crate::table::TableAlign::Right),
                                    "center" => Some(crate::table::TableAlign::Center),
                                    "left" => Some(crate::table::TableAlign::Left),
                                    _ => None,
                                }
                            } else if let Some(s) = attribute(e, b"style") {
                                let s_lower = s.to_ascii_lowercase();
                                if s_lower.contains("text-align: right")
                                    || s_lower.contains("text-align:right")
                                {
                                    Some(crate::table::TableAlign::Right)
                                } else if s_lower.contains("text-align: center")
                                    || s_lower.contains("text-align:center")
                                {
                                    Some(crate::table::TableAlign::Center)
                                } else if s_lower.contains("text-align: left")
                                    || s_lower.contains("text-align:left")
                                {
                                    Some(crate::table::TableAlign::Left)
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                        } else if name == b"br" {
                            if in_th || in_td {
                                current_cell.push('\n');
                            } else {
                                current_text.push('\n');
                            }
                        } else if name == b"hr" {
                            flush_text(&mut blocks, &mut current_text);
                            blocks.push(HtmlBlock::HorizontalRule);
                        }
                    }

                    if !is_html_void_tag(&name) {
                        current_tag.push(name);
                    }
                }
                Event::End(ref e) => {
                    let qname = e.name();
                    let name = local_name(qname.as_ref());

                    if matches!(
                        name,
                        b"head" | b"style" | b"script" | b"noscript" | b"template" | b"svg"
                    ) {
                        ignore_depth = ignore_depth.saturating_sub(1);
                    }

                    if ignore_depth == 0 {
                        let text = current_text.trim().to_string();

                        match name {
                            b"h1" => {
                                blocks.push(HtmlBlock::Heading { level: 1, text });
                                current_text.clear();
                            }
                            b"h2" => {
                                blocks.push(HtmlBlock::Heading { level: 2, text });
                                current_text.clear();
                            }
                            b"h3" => {
                                blocks.push(HtmlBlock::Heading { level: 3, text });
                                current_text.clear();
                            }
                            b"h4" | b"h5" | b"h6" => {
                                blocks.push(HtmlBlock::Heading { level: 4, text });
                                current_text.clear();
                            }
                            b"p" | b"blockquote" => {
                                if !text.is_empty() {
                                    blocks.push(HtmlBlock::Paragraph { text });
                                }
                                current_text.clear();
                            }
                            b"div" | b"section" | b"article" | b"main" | b"header" | b"footer"
                            | b"aside" | b"nav" | b"figure" | b"figcaption" | b"details"
                            | b"summary" | b"dl" => {
                                flush_text(&mut blocks, &mut current_text);
                            }
                            b"dt" => {
                                if !text.is_empty() {
                                    blocks.push(HtmlBlock::Heading { level: 4, text });
                                }
                                current_text.clear();
                            }
                            b"dd" => {
                                if !text.is_empty() {
                                    blocks.push(HtmlBlock::ListItem {
                                        bullet: "  • ".to_string(),
                                        text,
                                    });
                                }
                                current_text.clear();
                            }
                            b"li" => {
                                flush_li(&mut blocks, &mut current_text, &mut list_stack);
                            }
                            b"ul" | b"ol" => {
                                list_stack.pop();
                            }
                            b"pre" => {
                                if !text.is_empty() {
                                    blocks.push(HtmlBlock::CodeBlock { text });
                                }
                                current_text.clear();
                            }
                            b"th" | b"td" => {
                                let col_idx = current_row.len();
                                if let Some(align) = current_cell_align {
                                    if col_idx >= table_col_alignments.len() {
                                        table_col_alignments
                                            .resize(col_idx + current_colspan, None);
                                    }
                                    table_col_alignments[col_idx] = Some(align);
                                }
                                current_row.push(current_cell.trim().to_string());
                                for _ in 1..current_colspan {
                                    current_row.push(String::new());
                                }
                                current_cell.clear();
                                current_colspan = 1;
                                current_cell_align = None;
                                in_th = false;
                                in_td = false;
                            }
                            b"tr" => {
                                if !current_row.is_empty() {
                                    if row_had_th && table_headers.is_empty() {
                                        table_headers = std::mem::take(&mut current_row);
                                    } else {
                                        table_rows.push(std::mem::take(&mut current_row));
                                    }
                                }
                                current_row.clear();
                                row_had_th = false;
                            }
                            b"table" => {
                                in_table = false;
                                if !table_headers.is_empty() || !table_rows.is_empty() {
                                    if table_headers.is_empty() && !table_rows.is_empty() {
                                        table_headers = table_rows.remove(0);
                                    }
                                    let col_count = table_headers
                                        .len()
                                        .max(table_rows.iter().map(|r| r.len()).max().unwrap_or(0))
                                        .max(1);

                                    let mut alignments =
                                        vec![crate::table::TableAlign::Left; col_count];
                                    for (c, align) in
                                        alignments.iter_mut().enumerate().take(col_count)
                                    {
                                        if let Some(Some(explicit)) = table_col_alignments.get(c) {
                                            *align = *explicit;
                                        } else {
                                            let is_numeric = !table_rows.is_empty()
                                                && table_rows.iter().all(|r| {
                                                    if let Some(val) = r.get(c) {
                                                        crate::table::is_numeric_cell(val)
                                                    } else {
                                                        true
                                                    }
                                                });
                                            if is_numeric {
                                                *align = crate::table::TableAlign::Right;
                                            }
                                        }
                                    }

                                    blocks.push(HtmlBlock::Table(crate::table::TableData {
                                        headers: table_headers.clone(),
                                        rows: std::mem::take(&mut table_rows),
                                        alignments,
                                        raw_source: String::new(),
                                    }));
                                    table_headers.clear();
                                    table_col_alignments.clear();
                                }
                            }
                            _ => {}
                        }
                    }

                    if let Some(pos) = current_tag.iter().rposition(|t| t.as_slice() == name) {
                        current_tag.remove(pos);
                    }
                }
                Event::Empty(ref e) => {
                    let qname = e.name();
                    let name = local_name(qname.as_ref());
                    if ignore_depth == 0 {
                        if name == b"hr" {
                            flush_text(&mut blocks, &mut current_text);
                            blocks.push(HtmlBlock::HorizontalRule);
                        } else if name == b"br" {
                            if in_th || in_td {
                                current_cell.push('\n');
                            } else {
                                current_text.push('\n');
                            }
                        } else if name.eq_ignore_ascii_case(b"img") {
                            image_element_count = image_element_count.saturating_add(1);
                            if image_element_count > MAX_HTML_IMAGE_ELEMENTS {
                                push_html_warning_once(
                                    &mut warnings,
                                    "HTML image elements exceeded the supported limit; remaining images were omitted",
                                );
                            } else {
                                let (source, from_srcset) =
                                    html_image_source(e).unwrap_or_default();
                                let alt = html_attribute(e, b"alt").unwrap_or_default();
                                if from_srcset {
                                    push_html_warning_once(
                                        &mut warnings,
                                        "HTML srcset uses its first candidate; responsive source selection is approximated",
                                    );
                                }
                                let image_key = normalize_cid_reference(&source)
                                    .or_else(|| {
                                        inline_images.contains_key(&source).then(|| source.clone())
                                    })
                                    .or_else(|| {
                                        inline_image_base_uri
                                            .and_then(|base| {
                                                crate::document::mime_images::resolve_mime_uri(
                                                    base, &source,
                                                )
                                            })
                                            .filter(|resolved| inline_images.contains_key(resolved))
                                    });
                                if let Some((key, image)) = image_key
                                    .as_ref()
                                    .and_then(|key| inline_images.get_key_value(key))
                                {
                                    if in_th || in_td {
                                        if !alt.is_empty() {
                                            current_cell.push_str(&format!("[Image: {alt}]"));
                                        }
                                        push_html_warning_once(
                                            &mut warnings,
                                            "embedded HTML images inside table cells were reduced to alt text",
                                        );
                                    } else if in_table {
                                        push_html_warning_once(
                                            &mut warnings,
                                            "embedded HTML images inside tables were omitted",
                                        );
                                    } else {
                                        let within_budget = !budget_inline_instances
                                            || reserve_html_image_instance(
                                                image,
                                                &mut inline_image_budget,
                                                &mut warnings,
                                            );
                                        if within_budget {
                                            flush_text(&mut blocks, &mut current_text);
                                            blocks.push(HtmlBlock::Image {
                                                href: image.href.clone(),
                                                pixel_width: image.pixel_width,
                                                pixel_height: image.pixel_height,
                                                alt: if alt.is_empty() {
                                                    "Embedded HTML image".into()
                                                } else {
                                                    alt
                                                },
                                            });
                                            used_inline_images.insert(key.clone());
                                        } else if !alt.is_empty() {
                                            current_text.push_str(&format!("[Image: {alt}]"));
                                        }
                                    }
                                } else {
                                    push_html_warning_once(
                                        &mut warnings,
                                        "HTML image source was omitted because it was not a validated embedded image resource",
                                    );
                                    if !alt.is_empty() {
                                        if in_th || in_td {
                                            current_cell.push_str(&format!("[Image: {alt}]"));
                                        } else if !in_table {
                                            flush_text(&mut blocks, &mut current_text);
                                            blocks.push(HtmlBlock::Paragraph {
                                                text: format!("[Image omitted: {alt}]"),
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Event::Text(ref e) => {
                    if ignore_depth == 0
                        && let Ok(raw_str) = std::str::from_utf8(e.as_ref())
                    {
                        let t = decode_html_entities(raw_str);
                        let in_pre = current_tag.iter().any(|t| t.as_slice() == b"pre");
                        if in_th || in_td {
                            append_html_text(&mut current_cell, &t, in_pre);
                        } else if !in_table {
                            append_html_text(&mut current_text, &t, in_pre);
                        }
                    }
                }
                Event::GeneralRef(ref e) => {
                    if ignore_depth == 0
                        && let Ok(raw_str) = std::str::from_utf8(e.as_ref())
                    {
                        let clean_name = raw_str.trim_start_matches('&').trim_end_matches(';');
                        let decoded = decode_named_or_numeric_entity(clean_name)
                            .unwrap_or_else(|| format!("&{clean_name};"));
                        if in_th || in_td {
                            current_cell.push_str(&decoded);
                        } else if !in_table {
                            current_text.push_str(&decoded);
                        }
                    }
                }
                Event::CData(ref e) => {
                    if ignore_depth == 0
                        && let Ok(raw_str) = std::str::from_utf8(e.as_ref())
                    {
                        if in_th || in_td {
                            current_cell.push_str(raw_str);
                        } else if !in_table {
                            current_text.push_str(raw_str);
                        }
                    }
                }
                Event::Eof => break,
                _ => {}
            },
            Err(error) => {
                return Err(Error::InvalidInput(format!(
                    "malformed HTML document: {error}"
                )));
            }
        }
        buf.clear();
    }

    flush_text(&mut blocks, &mut current_text);

    Ok((blocks, warnings, used_inline_images))
}

pub(crate) fn render_blocks_to_pages(
    blocks: &[HtmlBlock],
    sink: &mut dyn PageConsumer,
    options: &ConvertOptions,
) -> Result<()> {
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "HTML contains no renderable content".into(),
        ));
    }

    let mut current_page_num = 1usize;
    let mut current_page = create_document_page(current_page_num);
    let mut current_y = MARGIN_TOP;

    let flush_page =
        |page: &mut Page, page_num: &mut usize, sink: &mut dyn PageConsumer| -> Result<()> {
            sink.consume(page.clone())?;
            *page_num += 1;
            *page = create_document_page(*page_num);
            Ok(())
        };

    for block in blocks {
        match block {
            HtmlBlock::PageBreak => {
                if current_page.nodes.len() > 1 {
                    if current_page_num >= options.max_pages {
                        return Err(Error::LimitExceeded(
                            "HTML page breaks exceeded maximum pages".into(),
                        ));
                    }
                    sink.consume(current_page)?;
                    current_page_num += 1;
                    current_page = create_document_page(current_page_num);
                    current_y = MARGIN_TOP;
                }
            }
            HtmlBlock::Heading { level, text } => {
                let (font_size, line_height, space_before, space_after) = match level {
                    1 => (22.0, 28.0, 24.0, 12.0),
                    2 => (17.0, 22.0, 20.0, 10.0),
                    3 => (14.0, 18.0, 16.0, 8.0),
                    _ => (12.5, 16.0, 12.0, 6.0),
                };

                // Keep-with-next: ensure room for heading plus subsequent content
                if current_y + space_before + line_height + space_after + 32.0
                    > MARGIN_TOP + CONTENT_HEIGHT
                    && current_y > MARGIN_TOP
                {
                    flush_page(&mut current_page, &mut current_page_num, sink)?;
                    current_y = MARGIN_TOP;
                } else {
                    current_y += space_before;
                }

                let wrapped_lines = wrap_text(text, CONTENT_WIDTH, font_size * 0.65);
                for line in wrapped_lines {
                    if current_y + line_height > MARGIN_TOP + CONTENT_HEIGHT {
                        flush_page(&mut current_page, &mut current_page_num, sink)?;
                        current_y = MARGIN_TOP;
                    }
                    current_page.nodes.push(Node::Text {
                        id: format!("heading_{}_{}", current_page_num, current_page.nodes.len()),
                        x: MARGIN_LEFT,
                        y: current_y + font_size * 0.85,
                        runs: vec![TextRun {
                            text: line,
                            font_family: "sans-serif".into(),
                            font_size,
                            bold: true,
                            italic: false,
                            fill: Paint::solid("#0f172a"),
                            baseline_shift: 0.0,
                            glyph_x_offsets: Vec::new(),
                            target_advance: None,
                        }],
                        anchor: TextAnchor::Start,
                        transform: IDENTITY,
                        opacity: 1.0,
                        stroke: Stroke::default(),
                        clip_id: None,
                        meta: SourceMeta {
                            semantic_role: format!("heading-{level}"),
                            ..Default::default()
                        },
                    });
                    current_y += line_height;
                }
                current_y += space_after;
            }
            HtmlBlock::Paragraph { text } => {
                let font_size = 11.0;
                let line_height = 16.0;
                let space_after = 10.0;

                let wrapped_lines = wrap_text(text, CONTENT_WIDTH, font_size * 0.58);
                for line in wrapped_lines {
                    if current_y + line_height > MARGIN_TOP + CONTENT_HEIGHT {
                        flush_page(&mut current_page, &mut current_page_num, sink)?;
                        current_y = MARGIN_TOP;
                    }
                    current_page.nodes.push(Node::Text {
                        id: format!("p_{}_{}", current_page_num, current_page.nodes.len()),
                        x: MARGIN_LEFT,
                        y: current_y + font_size * 0.85,
                        runs: vec![TextRun {
                            text: line,
                            font_family: "sans-serif".into(),
                            font_size,
                            bold: false,
                            italic: false,
                            fill: Paint::solid("#334155"),
                            baseline_shift: 0.0,
                            glyph_x_offsets: Vec::new(),
                            target_advance: None,
                        }],
                        anchor: TextAnchor::Start,
                        transform: IDENTITY,
                        opacity: 1.0,
                        stroke: Stroke::default(),
                        clip_id: None,
                        meta: SourceMeta {
                            semantic_role: "paragraph".into(),
                            ..Default::default()
                        },
                    });
                    current_y += line_height;
                }
                current_y += space_after;
            }
            HtmlBlock::ListItem { bullet, text } => {
                let font_size = 11.0;
                let line_height = 15.0;
                let indent = 20.0;

                if current_y + line_height > MARGIN_TOP + CONTENT_HEIGHT {
                    flush_page(&mut current_page, &mut current_page_num, sink)?;
                    current_y = MARGIN_TOP;
                }

                let wrapped_lines = wrap_text(text, CONTENT_WIDTH - indent, font_size * 0.58);
                for (i, line) in wrapped_lines.iter().enumerate() {
                    if current_y + line_height > MARGIN_TOP + CONTENT_HEIGHT {
                        flush_page(&mut current_page, &mut current_page_num, sink)?;
                        current_y = MARGIN_TOP;
                    }
                    let line_text = if i == 0 {
                        format!("{bullet}{line}")
                    } else {
                        line.clone()
                    };
                    let x_offset = if i == 0 {
                        MARGIN_LEFT
                    } else {
                        MARGIN_LEFT + indent
                    };

                    current_page.nodes.push(Node::Text {
                        id: format!("li_{}_{}", current_page_num, current_page.nodes.len()),
                        x: x_offset,
                        y: current_y + font_size * 0.85,
                        runs: vec![TextRun {
                            text: line_text,
                            font_family: "sans-serif".into(),
                            font_size,
                            bold: false,
                            italic: false,
                            fill: Paint::solid("#334155"),
                            baseline_shift: 0.0,
                            glyph_x_offsets: Vec::new(),
                            target_advance: None,
                        }],
                        anchor: TextAnchor::Start,
                        transform: IDENTITY,
                        opacity: 1.0,
                        stroke: Stroke::default(),
                        clip_id: None,
                        meta: SourceMeta {
                            semantic_role: "list-item".into(),
                            ..Default::default()
                        },
                    });
                    current_y += line_height;
                }
                current_y += 4.0;
            }
            HtmlBlock::StyledText { kind, runs } => {
                let (
                    base_font_size,
                    base_bold,
                    base_color,
                    line_height,
                    space_before,
                    space_after,
                    indent,
                    semantic_role,
                ) = match kind {
                    HtmlTextBlockKind::Heading(level) => {
                        let (size, height, before, after) = match level {
                            1 => (22.0, 28.0, 24.0, 12.0),
                            2 => (17.0, 22.0, 20.0, 10.0),
                            3 => (14.0, 18.0, 16.0, 8.0),
                            _ => (12.5, 16.0, 12.0, 6.0),
                        };
                        (
                            size,
                            true,
                            "#0f172a",
                            height,
                            before,
                            after,
                            0.0,
                            format!("heading-{level}"),
                        )
                    }
                    HtmlTextBlockKind::Paragraph => (
                        11.0,
                        false,
                        "#334155",
                        16.0,
                        0.0,
                        10.0,
                        0.0,
                        "paragraph".into(),
                    ),
                    HtmlTextBlockKind::ListItem { .. } => (
                        11.0,
                        false,
                        "#334155",
                        15.0,
                        0.0,
                        4.0,
                        20.0,
                        "list-item".into(),
                    ),
                };
                if runs.iter().all(|run| run.text.trim().is_empty()) {
                    continue;
                }
                if matches!(kind, HtmlTextBlockKind::Heading(_))
                    && current_y + space_before + line_height + space_after + 32.0
                        > MARGIN_TOP + CONTENT_HEIGHT
                    && current_y > MARGIN_TOP
                {
                    flush_page(&mut current_page, &mut current_page_num, sink)?;
                    current_y = MARGIN_TOP;
                } else {
                    current_y += space_before;
                }
                let wrapped_lines = wrap_styled_text(runs, CONTENT_WIDTH - indent, base_font_size);
                for (line_index, line_runs) in wrapped_lines.into_iter().enumerate() {
                    if line_runs.is_empty() {
                        continue;
                    }
                    let line_font_size = line_runs
                        .iter()
                        .map(|run| run.style.font_size.unwrap_or(base_font_size))
                        .fold(base_font_size, f64::max);
                    let actual_line_height = line_height.max(line_font_size * 1.35);
                    if current_y + actual_line_height > MARGIN_TOP + CONTENT_HEIGHT {
                        flush_page(&mut current_page, &mut current_page_num, sink)?;
                        current_y = MARGIN_TOP;
                    }
                    let mut svg_runs = Vec::with_capacity(
                        line_runs.len()
                            + usize::from(
                                matches!(kind, HtmlTextBlockKind::ListItem { .. })
                                    && line_index == 0,
                            ),
                    );
                    if let HtmlTextBlockKind::ListItem { bullet } = kind
                        && line_index == 0
                    {
                        svg_runs.push(TextRun {
                            text: bullet.clone(),
                            font_family: "sans-serif".into(),
                            font_size: base_font_size,
                            bold: base_bold,
                            italic: false,
                            fill: Paint::solid(base_color),
                            baseline_shift: 0.0,
                            glyph_x_offsets: Vec::new(),
                            target_advance: None,
                        });
                    }
                    svg_runs.extend(line_runs.into_iter().map(|run| TextRun {
                        text: run.text,
                        font_family: run.style.font_family.unwrap_or_else(|| "sans-serif".into()),
                        font_size: run.style.font_size.unwrap_or(base_font_size),
                        bold: run.style.bold.unwrap_or(base_bold),
                        italic: run.style.italic.unwrap_or(false),
                        fill: Paint::solid(run.style.color.unwrap_or_else(|| base_color.into())),
                        baseline_shift: 0.0,
                        glyph_x_offsets: Vec::new(),
                        target_advance: None,
                    }));
                    current_page.nodes.push(Node::Text {
                        id: format!("odf_text_{}_{}", current_page_num, current_page.nodes.len()),
                        x: MARGIN_LEFT
                            + if matches!(kind, HtmlTextBlockKind::ListItem { .. })
                                && line_index > 0
                            {
                                indent
                            } else {
                                0.0
                            },
                        y: current_y + line_font_size * 0.85,
                        runs: svg_runs,
                        anchor: TextAnchor::Start,
                        transform: IDENTITY,
                        opacity: 1.0,
                        stroke: Stroke::default(),
                        clip_id: None,
                        meta: SourceMeta {
                            semantic_role: semantic_role.clone(),
                            ..Default::default()
                        },
                    });
                    current_y += actual_line_height;
                }
                current_y += space_after;
            }
            HtmlBlock::CodeBlock { text } => {
                let expanded_code = expand_tab_stops(text, 4);
                let font_size = 10.0;
                let line_height = 14.0;
                let pad_y = 8.0;
                let lines: Vec<&str> = expanded_code.lines().collect();
                let mut line_idx = 0;

                while line_idx < lines.len() {
                    let remaining_page_h = (MARGIN_TOP + CONTENT_HEIGHT) - current_y;
                    if remaining_page_h < line_height * 3.0 + pad_y * 2.0 && current_y > MARGIN_TOP
                    {
                        flush_page(&mut current_page, &mut current_page_num, sink)?;
                        current_y = MARGIN_TOP;
                    }

                    let available_h = (MARGIN_TOP + CONTENT_HEIGHT) - current_y;
                    let max_lines =
                        ((available_h - pad_y * 2.0) / line_height).floor().max(1.0) as usize;
                    let chunk_end = (line_idx + max_lines).min(lines.len());
                    let chunk = &lines[line_idx..chunk_end];
                    let chunk_h = chunk.len() as f64 * line_height + pad_y * 2.0;

                    // Code background box for this chunk
                    current_page.nodes.push(Node::Path {
                        id: format!("code_bg_{}_{}", current_page_num, current_page.nodes.len()),
                        d: format!(
                            "M {:.2},{:.2} h {:.2} v {:.2} h -{:.2} Z",
                            MARGIN_LEFT, current_y, CONTENT_WIDTH, chunk_h, CONTENT_WIDTH
                        ),
                        fill_rule: "evenodd".into(),
                        fill: Paint::solid("#f8fafc"),
                        stroke: Stroke {
                            paint: Paint::solid("#e2e8f0"),
                            width: 1.0,
                            ..Default::default()
                        },
                        transform: IDENTITY,
                        clip_id: None,
                        meta: SourceMeta::default(),
                    });

                    let mut code_y = current_y + pad_y;
                    for l in chunk {
                        current_page.nodes.push(Node::Text {
                            id: format!("code_{}_{}", current_page_num, current_page.nodes.len()),
                            x: MARGIN_LEFT + 10.0,
                            y: code_y + font_size * 0.85,
                            runs: vec![TextRun {
                                text: (*l).to_string(),
                                font_family: "monospace".into(),
                                font_size,
                                bold: false,
                                italic: false,
                                fill: Paint::solid("#0f172a"),
                                baseline_shift: 0.0,
                                glyph_x_offsets: Vec::new(),
                                target_advance: None,
                            }],
                            anchor: TextAnchor::Start,
                            transform: IDENTITY,
                            opacity: 1.0,
                            stroke: Stroke::default(),
                            clip_id: None,
                            meta: SourceMeta {
                                semantic_role: "code".into(),
                                ..Default::default()
                            },
                        });
                        code_y += line_height;
                    }

                    current_y += chunk_h + 12.0;
                    line_idx = chunk_end;

                    if line_idx < lines.len() {
                        flush_page(&mut current_page, &mut current_page_num, sink)?;
                        current_y = MARGIN_TOP;
                    }
                }
            }
            HtmlBlock::Image {
                href,
                pixel_width,
                pixel_height,
                alt,
            } => {
                if *pixel_width == 0 || *pixel_height == 0 {
                    continue;
                }
                let scale = (CONTENT_WIDTH / f64::from(*pixel_width))
                    .min(360.0 / f64::from(*pixel_height))
                    .min(1.0);
                let draw_width = f64::from(*pixel_width) * scale;
                let draw_height = f64::from(*pixel_height) * scale;
                if current_y + draw_height > MARGIN_TOP + CONTENT_HEIGHT && current_y > MARGIN_TOP {
                    if current_page_num >= options.max_pages {
                        return Err(Error::LimitExceeded(
                            "HTML image blocks exceeded maximum pages".into(),
                        ));
                    }
                    flush_page(&mut current_page, &mut current_page_num, sink)?;
                    current_y = MARGIN_TOP;
                }
                let x = MARGIN_LEFT + (CONTENT_WIDTH - draw_width) * 0.5;
                current_page.nodes.push(Node::Image {
                    id: format!(
                        "document_image_{}_{}",
                        current_page_num,
                        current_page.nodes.len()
                    ),
                    href: href.clone(),
                    x,
                    y: current_y,
                    width: draw_width,
                    height: draw_height,
                    transform: IDENTITY,
                    opacity: 1.0,
                    clip_id: None,
                    meta: SourceMeta {
                        semantic_role: "document:image".into(),
                        alt_text: alt.clone(),
                        ..Default::default()
                    },
                });
                current_y += draw_height + 12.0;
            }
            HtmlBlock::HorizontalRule => {
                current_y += 10.0;
                if current_y + 10.0 > MARGIN_TOP + CONTENT_HEIGHT {
                    flush_page(&mut current_page, &mut current_page_num, sink)?;
                    current_y = MARGIN_TOP;
                }
                current_page.nodes.push(Node::Path {
                    id: format!("hr_{}_{}", current_page_num, current_page.nodes.len()),
                    d: format!(
                        "M {:.2},{:.2} h {:.2}",
                        MARGIN_LEFT, current_y, CONTENT_WIDTH
                    ),
                    fill_rule: "evenodd".into(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: Paint::solid("#cbd5e1"),
                        width: 1.0,
                        ..Default::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
                current_y += 12.0;
            }
            HtmlBlock::Table(table) => {
                let col_count = table
                    .headers
                    .len()
                    .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0))
                    .max(1);

                let font_size = 9.5;
                let header_font_size = 10.0;
                let padding_x = 8.0;
                let cell_height = 24.0;
                let header_height = 26.0;

                // Estimate relative weights for columns based on character length
                let mut col_char_counts = vec![4usize; col_count];
                for (c, h) in table.headers.iter().enumerate() {
                    if c < col_count {
                        col_char_counts[c] = col_char_counts[c].max(h.chars().count());
                    }
                }
                for row in &table.rows {
                    for (c, cell) in row.iter().enumerate() {
                        if c < col_count {
                            col_char_counts[c] = col_char_counts[c].max(cell.chars().count());
                        }
                    }
                }

                let total_chars: usize = col_char_counts.iter().sum::<usize>().max(1);
                let mut col_widths = vec![0.0f64; col_count];
                for c in 0..col_count {
                    let ratio = col_char_counts[c] as f64 / total_chars as f64;
                    let w = (CONTENT_WIDTH * ratio).max(40.0);
                    col_widths[c] = w;
                }
                let cur_sum: f64 = col_widths.iter().sum();
                let factor = CONTENT_WIDTH / cur_sum.max(1.0);
                for w in &mut col_widths {
                    *w *= factor;
                }

                let render_header = |page: &mut Page, y: f64, page_num: usize| {
                    page.nodes.push(Node::Path {
                        id: format!("tbl_hdr_bg_{}_{}", page_num, page.nodes.len()),
                        d: format!(
                            "M {:.2},{:.2} h {:.2} v {:.2} h -{:.2} Z",
                            MARGIN_LEFT, y, CONTENT_WIDTH, header_height, CONTENT_WIDTH
                        ),
                        fill_rule: "evenodd".into(),
                        fill: Paint::solid("#f1f5f9"),
                        stroke: Stroke {
                            paint: Paint::solid("#cbd5e1"),
                            width: 1.0,
                            ..Default::default()
                        },
                        transform: IDENTITY,
                        clip_id: None,
                        meta: SourceMeta::default(),
                    });

                    let mut cur_x = MARGIN_LEFT;
                    for (c, header) in table.headers.iter().enumerate() {
                        if c >= col_widths.len() {
                            break;
                        }
                        let w = col_widths[c];
                        let align = table
                            .alignments
                            .get(c)
                            .copied()
                            .unwrap_or(crate::table::TableAlign::Left);
                        let (text_x, anchor) = match align {
                            crate::table::TableAlign::Center => {
                                (cur_x + w / 2.0, TextAnchor::Middle)
                            }
                            crate::table::TableAlign::Right => {
                                (cur_x + w - padding_x, TextAnchor::End)
                            }
                            crate::table::TableAlign::Left => {
                                (cur_x + padding_x, TextAnchor::Start)
                            }
                        };

                        let max_cell_text_w = (w - padding_x * 2.0).max(10.0);
                        page.nodes.push(Node::Text {
                            id: format!("tbl_hdr_txt_{}_{}", page_num, page.nodes.len()),
                            x: text_x,
                            y: y + header_height * 0.65,
                            runs: vec![TextRun {
                                text: fit_text_to_width(
                                    header,
                                    max_cell_text_w,
                                    header_font_size * 0.6,
                                ),
                                font_family: "sans-serif".into(),
                                font_size: header_font_size,
                                bold: true,
                                italic: false,
                                fill: Paint::solid("#0f172a"),
                                baseline_shift: 0.0,
                                glyph_x_offsets: Vec::new(),
                                target_advance: None,
                            }],
                            anchor,
                            transform: IDENTITY,
                            opacity: 1.0,
                            stroke: Stroke::default(),
                            clip_id: None,
                            meta: SourceMeta {
                                semantic_role: "table-header".into(),
                                ..Default::default()
                            },
                        });
                        cur_x += w;
                    }
                };

                current_y += 8.0;
                if current_y + header_height + cell_height > MARGIN_TOP + CONTENT_HEIGHT {
                    flush_page(&mut current_page, &mut current_page_num, sink)?;
                    current_y = MARGIN_TOP;
                }

                if !table.headers.is_empty() {
                    render_header(&mut current_page, current_y, current_page_num);
                    current_y += header_height;
                }

                for (row_idx, row) in table.rows.iter().enumerate() {
                    if current_y + cell_height > MARGIN_TOP + CONTENT_HEIGHT {
                        flush_page(&mut current_page, &mut current_page_num, sink)?;
                        current_y = MARGIN_TOP;
                        if !table.headers.is_empty() {
                            render_header(&mut current_page, current_y, current_page_num);
                            current_y += header_height;
                        }
                    }

                    let bg_color = if row_idx % 2 == 1 {
                        "#f8fafc"
                    } else {
                        "#ffffff"
                    };
                    current_page.nodes.push(Node::Path {
                        id: format!(
                            "tbl_row_bg_{}_{}",
                            current_page_num,
                            current_page.nodes.len()
                        ),
                        d: format!(
                            "M {:.2},{:.2} h {:.2} v {:.2} h -{:.2} Z",
                            MARGIN_LEFT, current_y, CONTENT_WIDTH, cell_height, CONTENT_WIDTH
                        ),
                        fill_rule: "evenodd".into(),
                        fill: Paint::solid(bg_color),
                        stroke: Stroke {
                            paint: Paint::solid("#e2e8f0"),
                            width: 0.75,
                            ..Default::default()
                        },
                        transform: IDENTITY,
                        clip_id: None,
                        meta: SourceMeta::default(),
                    });

                    let mut cur_x = MARGIN_LEFT;
                    for (c, cell) in row.iter().enumerate() {
                        if c >= col_widths.len() {
                            break;
                        }
                        let w = col_widths[c];
                        let align = table
                            .alignments
                            .get(c)
                            .copied()
                            .unwrap_or(crate::table::TableAlign::Left);
                        let (text_x, anchor) = match align {
                            crate::table::TableAlign::Center => {
                                (cur_x + w / 2.0, TextAnchor::Middle)
                            }
                            crate::table::TableAlign::Right => {
                                (cur_x + w - padding_x, TextAnchor::End)
                            }
                            crate::table::TableAlign::Left => {
                                (cur_x + padding_x, TextAnchor::Start)
                            }
                        };

                        let max_cell_text_w = (w - padding_x * 2.0).max(10.0);
                        current_page.nodes.push(Node::Text {
                            id: format!(
                                "tbl_cell_{}_{}",
                                current_page_num,
                                current_page.nodes.len()
                            ),
                            x: text_x,
                            y: current_y + cell_height * 0.65,
                            runs: vec![TextRun {
                                text: fit_text_to_width(cell, max_cell_text_w, font_size * 0.58),
                                font_family: "sans-serif".into(),
                                font_size,
                                bold: false,
                                italic: false,
                                fill: Paint::solid("#334155"),
                                baseline_shift: 0.0,
                                glyph_x_offsets: Vec::new(),
                                target_advance: None,
                            }],
                            anchor,
                            transform: IDENTITY,
                            opacity: 1.0,
                            stroke: Stroke::default(),
                            clip_id: None,
                            meta: SourceMeta {
                                semantic_role: "table-cell".into(),
                                ..Default::default()
                            },
                        });
                        cur_x += w;
                    }

                    current_y += cell_height;
                }

                current_y += 12.0;
            }
        }

        if current_page_num > options.max_pages {
            return Err(Error::LimitExceeded(
                "HTML pagination exceeded maximum pages".into(),
            ));
        }
    }

    if current_page.nodes.len() > 1 {
        sink.consume(current_page)?;
    }

    Ok(())
}

/// Render HTML-like flow blocks and attach parser/resource warnings to every
/// emitted page. Format adapters that load local resources before rendering use
/// this helper so warnings remain visible in both the conversion report and
/// the page metadata.
pub(crate) fn render_blocks_to_pages_with_warnings(
    blocks: &[HtmlBlock],
    sink: &mut dyn PageConsumer,
    options: &ConvertOptions,
    warnings: &[String],
) -> Result<()> {
    let mut warning_sink = HtmlWarningSink {
        inner: sink,
        warnings,
    };
    render_blocks_to_pages(blocks, &mut warning_sink, options)
}

pub(crate) fn wrap_text(text: &str, max_width: f64, char_width: f64) -> Vec<String> {
    let mut lines = Vec::new();

    for paragraph in text.split('\n') {
        let trimmed = paragraph.trim();
        if trimmed.is_empty() {
            continue;
        }

        let tokens = tokenize_for_wrapping(trimmed);
        let mut current_line = String::new();
        let mut current_line_width = 0.0;

        for token in tokens {
            let token_width = estimate_token_width(&token, char_width);
            let needs_space = needs_space_between(&current_line, &token);
            let space_width = if needs_space { char_width } else { 0.0 };

            if !current_line.is_empty()
                && (current_line_width + space_width + token_width > max_width)
            {
                lines.push(current_line);
                current_line = String::new();
                current_line_width = 0.0;
            }

            if needs_space && !current_line.is_empty() {
                current_line.push(' ');
                current_line_width += char_width;
            }

            current_line.push_str(&token);
            current_line_width += token_width;
        }

        if !current_line.is_empty() {
            lines.push(current_line);
        }
    }

    if lines.is_empty() {
        lines.push(text.to_string());
    }

    lines
}

pub(crate) fn wrap_styled_text(
    runs: &[HtmlTextRun],
    max_width: f64,
    default_font_size: f64,
) -> Vec<Vec<HtmlTextRun>> {
    let mut wrapper = StyledTextWrapper {
        lines: Vec::new(),
        line: Vec::new(),
        line_width: 0.0,
        word: Vec::new(),
        word_width: 0.0,
        pending_space: None,
        max_width,
        default_font_size,
    };
    for run in runs {
        for character in run.text.chars() {
            if character == '\n' {
                wrapper.flush_word();
                wrapper.flush_line();
                wrapper.pending_space = None;
            } else if character.is_whitespace() {
                wrapper.flush_word();
                if !wrapper.line.is_empty() {
                    wrapper.pending_space = Some(run.style.clone());
                }
            } else if is_cjk_char(character) {
                wrapper.flush_word();
                wrapper.append_word_character(character, &run.style);
                wrapper.flush_word();
            } else {
                wrapper.append_word_character(character, &run.style);
            }
        }
    }
    wrapper.flush_word();
    wrapper.flush_line();
    if wrapper.lines.is_empty() && !runs.is_empty() {
        wrapper.lines.push(runs.to_vec());
    }
    wrapper.lines
}

struct StyledTextWrapper {
    lines: Vec<Vec<HtmlTextRun>>,
    line: Vec<HtmlTextRun>,
    line_width: f64,
    word: Vec<HtmlTextRun>,
    word_width: f64,
    pending_space: Option<HtmlTextStyle>,
    max_width: f64,
    default_font_size: f64,
}

impl StyledTextWrapper {
    fn append_word_character(&mut self, character: char, style: &HtmlTextStyle) {
        append_html_text_run(&mut self.word, &character.to_string(), style);
        self.word_width += styled_char_width(character, style, self.default_font_size);
    }

    fn flush_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        let word = std::mem::take(&mut self.word);
        let word_width = std::mem::take(&mut self.word_width);
        let space_style = self.pending_space.take();
        let space_width = space_style
            .as_ref()
            .map(|style| styled_char_width(' ', style, self.default_font_size))
            .unwrap_or(0.0);
        let closing_cjk_punctuation = word
            .iter()
            .flat_map(|run| run.text.chars())
            .all(is_cjk_closing_punct);
        if !self.line.is_empty()
            && self.line_width + space_width + word_width > self.max_width
            && !closing_cjk_punctuation
        {
            self.flush_line();
        } else if let Some(style) = space_style
            && !self.line.is_empty()
        {
            append_html_text_run(&mut self.line, " ", &style);
            self.line_width += space_width;
        }

        if word_width <= self.max_width {
            for run in &word {
                self.line_width += styled_text_width(&run.text, &run.style, self.default_font_size);
                append_html_text_run(&mut self.line, &run.text, &run.style);
            }
        } else {
            for run in &word {
                for character in run.text.chars() {
                    let width = styled_char_width(character, &run.style, self.default_font_size);
                    if !self.line.is_empty()
                        && self.line_width + width > self.max_width
                        && !is_cjk_closing_punct(character)
                    {
                        self.flush_line();
                    }
                    append_html_text_run(&mut self.line, &character.to_string(), &run.style);
                    self.line_width += width;
                }
            }
        }
    }

    fn flush_line(&mut self) {
        if !self.line.is_empty() {
            self.lines.push(std::mem::take(&mut self.line));
            self.line_width = 0.0;
        }
    }
}

fn append_html_text_run(runs: &mut Vec<HtmlTextRun>, text: &str, style: &HtmlTextStyle) {
    if let Some(last) = runs.last_mut()
        && last.style == *style
    {
        last.text.push_str(text);
    } else {
        runs.push(HtmlTextRun {
            text: text.to_owned(),
            style: style.clone(),
        });
    }
}

fn styled_char_width(character: char, style: &HtmlTextStyle, default_font_size: f64) -> f64 {
    let font_size = style
        .font_size
        .unwrap_or(default_font_size)
        .clamp(1.0, 512.0);
    let width = if is_cjk_char(character) {
        font_size * 0.99
    } else {
        font_size * 0.55
    };
    if style.bold == Some(true) {
        width * 1.05
    } else {
        width
    }
}

fn styled_text_width(text: &str, style: &HtmlTextStyle, default_font_size: f64) -> f64 {
    text.chars()
        .map(|character| styled_char_width(character, style, default_font_size))
        .sum()
}

fn is_cjk_char(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | // CJK Symbols and Punctuation
        0x3040..=0x309F | // Hiragana
        0x30A0..=0x30FF | // Katakana
        0x3400..=0x4DBF | // CJK Unified Ideographs Extension A
        0x4E00..=0x9FFF | // CJK Unified Ideographs
        0xF900..=0xFAFF | // CJK Compatibility Ideographs
        0xFF00..=0xFFEF | // Halfwidth and Fullwidth Forms
        0xAC00..=0xD7AF   // Hangul Syllables
    )
}

fn is_cjk_closing_punct(c: char) -> bool {
    matches!(
        c,
        '。' | '、'
            | '，'
            | '．'
            | '）'
            | '』'
            | '」'
            | '｝'
            | '］'
            | '！'
            | '？'
            | '：'
            | '；'
            | '…'
            | '・'
    )
}

fn estimate_token_width(token: &str, char_width: f64) -> f64 {
    let mut width = 0.0;
    for c in token.chars() {
        if is_cjk_char(c) {
            // CJK characters are full-width (approx 1.8x Latin char_width)
            width += char_width * 1.8;
        } else {
            width += char_width;
        }
    }
    width
}

fn needs_space_between(prev: &str, next: &str) -> bool {
    if prev.is_empty() {
        return false;
    }
    let last_char = prev.chars().last().unwrap_or(' ');
    let first_char = next.chars().next().unwrap_or(' ');

    !is_cjk_char(last_char) && !is_cjk_char(first_char) && !last_char.is_whitespace()
}

fn tokenize_for_wrapping(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current_latin = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            if !current_latin.is_empty() {
                tokens.push(std::mem::take(&mut current_latin));
            }
            i += 1;
            continue;
        }

        if is_cjk_char(c) {
            if !current_latin.is_empty() {
                tokens.push(std::mem::take(&mut current_latin));
            }

            let mut cjk_token = String::new();
            cjk_token.push(c);
            while i + 1 < chars.len() && is_cjk_closing_punct(chars[i + 1]) {
                i += 1;
                cjk_token.push(chars[i]);
            }
            tokens.push(cjk_token);
        } else {
            current_latin.push(c);
        }
        i += 1;
    }

    if !current_latin.is_empty() {
        tokens.push(current_latin);
    }

    tokens
}

fn create_document_page(page_num: usize) -> Page {
    let mut page = Page::new(page_num, PAGE_WIDTH, PAGE_HEIGHT, "html-page");
    page.nodes.push(Node::Path {
        id: format!("page_bg_{page_num}"),
        d: format!(
            "M 0,0 h {:.2} v {:.2} h -{:.2} Z",
            PAGE_WIDTH, PAGE_HEIGHT, PAGE_WIDTH
        ),
        fill_rule: "evenodd".into(),
        fill: Paint::solid("#ffffff"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta::default(),
    });
    page
}

pub fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '&' {
            let mut entity = String::new();
            let mut closed = false;
            while let Some(&next_c) = chars.peek() {
                if next_c == ';' {
                    chars.next();
                    closed = true;
                    break;
                } else if next_c.is_alphanumeric() || next_c == '#' {
                    entity.push(chars.next().unwrap());
                    if entity.len() > 10 {
                        break;
                    }
                } else {
                    break;
                }
            }

            if closed {
                if let Some(ch) = decode_named_or_numeric_entity(&entity) {
                    out.push_str(&ch);
                    continue;
                }
            }
            out.push('&');
            out.push_str(&entity);
            if closed {
                out.push(';');
            }
        } else {
            out.push(c);
        }
    }

    out
}

fn decode_named_or_numeric_entity(entity: &str) -> Option<String> {
    if let Some(stripped) = entity.strip_prefix('#') {
        let codepoint = if let Some(hex) = stripped
            .strip_prefix('x')
            .or_else(|| stripped.strip_prefix('X'))
        {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            stripped.parse::<u32>().ok()?
        };
        return char::from_u32(codepoint).map(|ch| ch.to_string());
    }

    let s = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{00A0}",
        "copy" => "©",
        "reg" => "®",
        "trade" => "™",
        "mdash" => "—",
        "ndash" => "–",
        "hellip" => "…",
        "bull" => "•",
        "ldquo" => "“",
        "rdquo" => "”",
        "lsquo" => "‘",
        "rsquo" => "’",
        "euro" => "€",
        "pound" => "£",
        "yen" => "¥",
        "cent" => "¢",
        "plusmn" => "±",
        "times" => "×",
        "divide" => "÷",
        "ne" => "≠",
        "le" => "≤",
        "ge" => "≥",
        "deg" => "°",
        "micro" => "µ",
        "middot" => "·",
        "rarr" => "→",
        "larr" => "←",
        _ => return None,
    };
    Some(s.to_string())
}

fn fit_text_to_width(text: &str, max_width: f64, char_width: f64) -> String {
    let text_w = estimate_token_width(text, char_width);
    if text_w <= max_width {
        return text.to_string();
    }
    let ellipsis_w = estimate_token_width("…", char_width);
    let target_w = (max_width - ellipsis_w).max(0.0);

    let mut result = String::new();
    let mut current_w = 0.0;
    for c in text.chars() {
        let cw = if is_cjk_char(c) {
            char_width * 1.8
        } else {
            char_width
        };
        if current_w + cw > target_w {
            break;
        }
        result.push(c);
        current_w += cw;
    }
    result.push('…');
    result
}

pub(crate) fn expand_tab_stops(text: &str, tab_size: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut col = 0;
    for ch in text.chars() {
        if ch == '\t' {
            let spaces = tab_size - (col % tab_size);
            for _ in 0..spaces {
                out.push(' ');
            }
            col += spaces;
        } else {
            out.push(ch);
            if ch == '\n' {
                col = 0;
            } else {
                col += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestPages(Vec<Page>);

    impl PageConsumer for TestPages {
        fn consume(&mut self, page: Page) -> Result<()> {
            self.0.push(page);
            Ok(())
        }
    }

    #[test]
    fn html_parser_enforces_event_budget() {
        assert!(matches!(
            parse_html_blocks_with_limit("<p>text</p>", 2),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn html_base_href_is_bounded_and_only_read_before_body() {
        assert_eq!(
            first_html_base_href(
                "<html><head><base href=\"assets/&amp;icons/\"></head><body><base href=\"ignored/\"></body></html>",
                32,
                4096,
            )
            .unwrap()
            .as_deref(),
            Some("assets/&icons/")
        );
        assert!(matches!(
            first_html_base_href("<html><body></body></html>", 1, 4096),
            Err(Error::LimitExceeded(_))
        ));
        assert_eq!(
            first_html_base_href(
                "<html><head><template><base href=\"template/\"></template></head><base href=\"late/\"><body></body></html>",
                32,
                4096,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn mime_inline_image_references_share_the_total_render_budget() {
        let image = InlineHtmlImage {
            href: "data:image/png;base64,abcd".into(),
            pixel_width: 1,
            pixel_height: 1,
        };
        let mut budget = HtmlImageBudget {
            data_uri_bytes: MAX_HTML_TOTAL_DATA_URI_BYTES - image.href.len() + 1,
            ..Default::default()
        };
        let mut warnings = Vec::new();
        assert!(!reserve_html_image_instance(
            &image,
            &mut budget,
            &mut warnings
        ));
        assert_eq!(
            budget.data_uri_bytes,
            MAX_HTML_TOTAL_DATA_URI_BYTES - image.href.len() + 1
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("total data URI"))
        );
    }

    #[test]
    fn image_source_scan_caps_reference_memory_and_accepts_xhtml_doctype() {
        let html = "<!DOCTYPE html><html><body><img src='one.png'/><img src='two.png'/><img src='three.png'/></body></html>";
        let (sources, exceeded) = collect_html_image_sources_with_limit(html, 100, 2).unwrap();

        assert_eq!(sources, ["one.png", "two.png"]);
        assert!(exceeded);
    }

    #[test]
    fn local_html_image_paths_decode_percent_bytes_and_stay_inside_the_base() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("site");
        std::fs::create_dir_all(base.join("assets")).unwrap();
        std::fs::write(base.join("assets/red-blue.png"), b"fixture").unwrap();
        let base = std::fs::canonicalize(base).unwrap();

        assert_eq!(
            local_html_image_path(&base, "assets/red%2Dblue.png"),
            Some(base.join("assets/red-blue.png"))
        );
        assert!(local_html_image_path(&base, "../outside.png").is_none());
        assert!(local_html_image_path(&base, "%2e%2e/outside.png").is_none());
        assert!(local_html_image_path(&base, "https://example.invalid/image.png").is_none());
        assert!(local_html_image_path(&base, "//example.invalid/image.png").is_none());
    }

    #[test]
    fn html_parser_caps_image_placeholders() {
        let html = format!(
            "<body>{}</body>",
            "<img src='remote.png' alt='remote'>".repeat(MAX_HTML_IMAGE_ELEMENTS + 3)
        );
        let (blocks, warnings, _) = parse_html_blocks_with_inline_images(
            &html,
            DEFAULT_MAX_HTML_EVENTS,
            MAX_NORMALIZED_HTML_BYTES,
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(blocks.len(), MAX_HTML_IMAGE_ELEMENTS);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("image elements exceeded"))
        );
    }

    #[test]
    fn ampersand_preprocessing_does_not_duplicate_multibyte_text() {
        assert_eq!(
            escape_bare_ampersands_limited("日本語 &copy; 3 & 4", 1024).unwrap(),
            "日本語 &copy; 3 &amp; 4"
        );
    }

    #[test]
    fn ampersand_expansion_stops_at_the_normalized_output_budget() {
        assert_eq!(
            escape_bare_ampersands_limited("& &", 11).unwrap(),
            "&amp; &amp;"
        );
        assert!(matches!(
            escape_bare_ampersands_limited("& &", 10),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn html_void_tags_without_xml_slashes_parse_without_losing_text() {
        let blocks = parse_html_blocks_with_limit(
            "<html><head><meta charset=\"utf-8\"></head><body><p>Before<img src=\"https://invalid.test/image.png\"><br>after</p></body></html>",
            1_000,
        )
        .unwrap();
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Paragraph { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Before"));
        assert!(text.contains("after"));
        assert!(!text.contains("invalid.test"));
    }

    #[test]
    fn explicit_page_break_creates_exactly_two_nonempty_pages() {
        let blocks = [
            HtmlBlock::Paragraph {
                text: "First page".into(),
            },
            HtmlBlock::PageBreak,
            HtmlBlock::Paragraph {
                text: "Second page".into(),
            },
            HtmlBlock::PageBreak,
        ];
        let mut pages = TestPages::default();
        render_blocks_to_pages(&blocks, &mut pages, &ConvertOptions::default()).unwrap();

        assert_eq!(pages.0.len(), 2);
        assert!(pages.0[0].nodes.len() > 1);
        assert!(pages.0[1].nodes.len() > 1);
    }

    #[test]
    fn expands_tabs_to_uniform_tab_stops() {
        assert_eq!(expand_tab_stops("a\tb", 4), "a   b");
        assert_eq!(expand_tab_stops("abc\td", 4), "abc d");
        assert_eq!(expand_tab_stops("abcd\te", 4), "abcd    e");
        assert_eq!(expand_tab_stops("\tline", 4), "    line");
    }
}
