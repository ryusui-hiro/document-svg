//! Bounded text-only TTML/DFXP subtitle preview.
//!
//! This is a caption-text importer, not a TTML presentation engine. It reads
//! `<p>` cue text and a bounded subset of media-clock/offset timing; it never
//! evaluates XML entities, styles, animations, scripts, or external resources.

use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_TTML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TTML_DEPTH: usize = 256;
const MAX_TTML_TAG_BYTES: usize = 1024 * 1024;
const MAX_TTML_CUES: usize = 100_000;
const MAX_TTML_CUE_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TTML_RENDERED_TEXT_BYTES: usize = 32 * 1024 * 1024;
const TTML_NAMESPACE: &str = "http://www.w3.org/ns/ttml";
const LEGACY_TTML_NAMESPACES: &[&str] = &[
    "http://www.w3.org/2006/10/ttaf1",
    "http://www.w3.org/2006/04/ttaf1",
];

#[derive(Clone, Debug)]
struct ElementFrame {
    name: String,
    begin_ms: u64,
    in_body: bool,
    valid_timing: bool,
}

#[derive(Default)]
struct CueBuilder {
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    identifier: Option<String>,
    text: String,
    valid: bool,
}

#[derive(Default)]
struct ParseWarnings {
    malformed_cues: usize,
    unsupported_times: usize,
    flattened_nested_timing: usize,
    ignored_container_intervals: usize,
    styling: bool,
    image_content: usize,
    controls: usize,
}

pub(crate) fn looks_like_ttml_prefix(prefix: &[u8]) -> bool {
    let mut reader = Reader::from_reader(prefix);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    for _ in 0..128 {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) | Ok(Event::Empty(start)) => {
                return is_ttml_root(&start, reader.decoder()).unwrap_or(false);
            }
            Ok(Event::DocType(_)) | Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => {}
        }
        buffer.clear();
    }
    false
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_TTML_BYTES),
        "TTML input",
    )?;
    let xml = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("TTML input must be UTF-8: {error}")))?;
    let (blocks, warnings) = parse_ttml_blocks(&xml, options.max_xml_events)?;
    let mut page_sink = TtmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

struct TtmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for TtmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "ttml".into();
        if page.title.is_empty() {
            page.title = "TTML subtitles".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn parse_ttml_blocks(
    xml: &str,
    max_events: usize,
) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if xml.len() as u64 > MAX_TTML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "TTML input exceeds {MAX_TTML_BYTES} bytes"
        )));
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<ElementFrame>::new();
    let mut cue = None::<CueBuilder>;
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: "TTML subtitles".into(),
    }];
    let mut warnings = ParseWarnings::default();
    let mut event_count = 0usize;
    let mut rendered_text_bytes = 0usize;
    let mut cue_count = 0usize;
    let mut root_seen = false;
    let mut root_is_empty = false;
    let mut root_closed = false;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "TTML input exceeds {max_events} XML events"
            )));
        }
        let event = reader.read_event_into(&mut buffer)?;
        match event {
            Event::Start(start) => {
                if root_closed {
                    return Err(Error::InvalidInput(
                        "TTML XML contains content after its root element".into(),
                    ));
                }
                let raw_tag: &[u8] = start.as_ref();
                if raw_tag.len() > MAX_TTML_TAG_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "TTML XML tag exceeds {MAX_TTML_TAG_BYTES} bytes"
                    )));
                }
                let name = local_element_name(start.name().as_ref())?;
                if !root_seen {
                    validate_root(&start, reader.decoder())?;
                    validate_time_base(&start, reader.decoder())?;
                    root_seen = true;
                }
                let parent_begin = stack.last().map_or(0, |frame| frame.begin_ms);
                let parent_valid = stack.last().is_none_or(|frame| frame.valid_timing);
                let in_body = name == "body" || stack.last().is_some_and(|frame| frame.in_body);
                let (begin_ms, valid_timing) = element_begin(
                    &start,
                    reader.decoder(),
                    parent_begin,
                    parent_valid,
                    &mut warnings,
                )?;
                inspect_style_attributes(&start, &mut warnings)?;
                if has_sequential_time_container(&start, reader.decoder())? {
                    return Err(Error::Unsupported(
                        "TTML sequential time containers are not supported".into(),
                    ));
                }
                if name == "p" && in_body {
                    if cue.is_some() {
                        return Err(Error::InvalidInput(
                            "TTML paragraph cues may not be nested".into(),
                        ));
                    }
                    cue = Some(build_cue(
                        &start,
                        reader.decoder(),
                        parent_begin,
                        begin_ms,
                        valid_timing,
                        &mut warnings,
                    )?);
                } else if matches!(name.as_str(), "span" | "br")
                    && cue.is_some()
                    && has_timing_attributes(&start)?
                {
                    warnings.flattened_nested_timing =
                        warnings.flattened_nested_timing.saturating_add(1);
                }
                if name == "br"
                    && let Some(cue) = cue.as_mut()
                {
                    cue.text.push_str(" · ");
                }
                if name == "image" || name == "data" {
                    warnings.image_content = warnings.image_content.saturating_add(1);
                }
                if matches!(name.as_str(), "style" | "styling" | "region" | "layout") {
                    warnings.styling = true;
                }
                if stack.len() >= MAX_TTML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "TTML nesting exceeds {MAX_TTML_DEPTH} elements"
                    )));
                }
                stack.push(ElementFrame {
                    name,
                    begin_ms,
                    in_body,
                    valid_timing,
                });
            }
            Event::Empty(start) => {
                if root_closed {
                    return Err(Error::InvalidInput(
                        "TTML XML contains content after its root element".into(),
                    ));
                }
                let raw_tag: &[u8] = start.as_ref();
                if raw_tag.len() > MAX_TTML_TAG_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "TTML XML tag exceeds {MAX_TTML_TAG_BYTES} bytes"
                    )));
                }
                let name = local_element_name(start.name().as_ref())?;
                if !root_seen {
                    validate_root(&start, reader.decoder())?;
                    validate_time_base(&start, reader.decoder())?;
                    root_seen = true;
                    root_is_empty = true;
                    root_closed = true;
                }
                let parent_begin = stack.last().map_or(0, |frame| frame.begin_ms);
                let parent_valid = stack.last().is_none_or(|frame| frame.valid_timing);
                let in_body = name == "body" || stack.last().is_some_and(|frame| frame.in_body);
                let (begin_ms, valid_timing) = element_begin(
                    &start,
                    reader.decoder(),
                    parent_begin,
                    parent_valid,
                    &mut warnings,
                )?;
                inspect_style_attributes(&start, &mut warnings)?;
                if has_sequential_time_container(&start, reader.decoder())? {
                    return Err(Error::Unsupported(
                        "TTML sequential time containers are not supported".into(),
                    ));
                }
                if name == "p" && in_body {
                    if cue.is_some() {
                        return Err(Error::InvalidInput(
                            "TTML paragraph cues may not be nested".into(),
                        ));
                    }
                    let empty_cue = build_cue(
                        &start,
                        reader.decoder(),
                        parent_begin,
                        begin_ms,
                        valid_timing,
                        &mut warnings,
                    )?;
                    finish_cue(
                        empty_cue,
                        &mut blocks,
                        &mut warnings,
                        &mut cue_count,
                        &mut rendered_text_bytes,
                    )?;
                }
                if name == "br"
                    && let Some(cue) = cue.as_mut()
                {
                    cue.text.push_str(" · ");
                }
                if name == "image" || name == "data" {
                    warnings.image_content = warnings.image_content.saturating_add(1);
                }
                if matches!(name.as_str(), "style" | "styling" | "region" | "layout") {
                    warnings.styling = true;
                }
            }
            Event::Text(text) => {
                let decoded = text.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid TTML text encoding: {error}"))
                })?;
                let unescaped = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    Error::InvalidInput(format!("invalid TTML text entity: {error}"))
                })?;
                if let Some(cue) = cue.as_mut() {
                    append_cue_text(cue, &unescaped, &mut warnings)?;
                } else if (root_closed || !root_seen) && !unescaped.trim().is_empty() {
                    return Err(Error::InvalidInput(
                        "TTML XML has text outside its root element".into(),
                    ));
                }
            }
            Event::CData(text) => {
                if let Some(cue) = cue.as_mut() {
                    let decoded = text.decode().map_err(|error| {
                        Error::InvalidInput(format!("invalid TTML CDATA encoding: {error}"))
                    })?;
                    append_cue_text(cue, &decoded, &mut warnings)?;
                } else {
                    return Err(Error::InvalidInput(
                        "TTML CDATA appears outside a text cue".into(),
                    ));
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(cue) = cue.as_mut() {
                    let decoded = crate::ooxml::decode_xml_reference(&reference, "TTML cue text")?;
                    append_cue_text(cue, &decoded, &mut warnings)?;
                }
            }
            Event::End(end) => {
                let name = local_element_name(end.name().as_ref())?;
                let Some(frame) = stack.pop() else {
                    return Err(Error::InvalidInput(
                        "TTML XML has an unmatched end tag".into(),
                    ));
                };
                if frame.name != name {
                    return Err(Error::InvalidInput(
                        "TTML XML element nesting is malformed".into(),
                    ));
                }
                if name == "p"
                    && frame.in_body
                    && let Some(cue) = cue.take()
                {
                    finish_cue(
                        cue,
                        &mut blocks,
                        &mut warnings,
                        &mut cue_count,
                        &mut rendered_text_bytes,
                    )?;
                }
                if stack.is_empty() {
                    root_closed = true;
                }
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(
                    "TTML document type declarations are not supported".into(),
                ));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }

    if !root_seen || root_is_empty || !root_closed {
        return Err(Error::InvalidInput(
            "TTML document has no subtitle body".into(),
        ));
    }
    if !stack.is_empty() || cue.is_some() {
        return Err(Error::InvalidInput(
            "TTML document ended before all elements closed".into(),
        ));
    }
    if cue_count == 0 {
        return Err(Error::InvalidInput(
            "TTML document contains no readable text cues".into(),
        ));
    }
    Ok((blocks, warning_messages(warnings)))
}

fn validate_root(start: &BytesStart<'_>, decoder: quick_xml::encoding::Decoder) -> Result<()> {
    if local_element_name(start.name().as_ref())? != "tt" || !is_ttml_root(start, decoder)? {
        return Err(Error::InvalidInput(
            "XML root is not a recognized TTML <tt> document".into(),
        ));
    }
    Ok(())
}

fn validate_time_base(start: &BytesStart<'_>, decoder: quick_xml::encoding::Decoder) -> Result<()> {
    let time_base = attribute_value(start, b"timeBase", decoder)?;
    if time_base
        .as_deref()
        .is_some_and(|value| !value.eq_ignore_ascii_case("media"))
    {
        return Err(Error::Unsupported(
            "TTML timeBase values other than media are not supported".into(),
        ));
    }
    Ok(())
}

fn is_ttml_root(start: &BytesStart<'_>, decoder: quick_xml::encoding::Decoder) -> Result<bool> {
    if local_element_name(start.name().as_ref())? != "tt" {
        return Ok(false);
    }
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|error| {
            Error::InvalidInput(format!("invalid TTML root attribute: {error}"))
        })?;
        let key = attribute.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            let value = attribute
                .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)
                .map_err(|error| Error::InvalidInput(format!("invalid TTML namespace: {error}")))?;
            if value == TTML_NAMESPACE || LEGACY_TTML_NAMESPACES.contains(&value.as_ref()) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn element_begin(
    start: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
    parent_begin: u64,
    parent_valid: bool,
    warnings: &mut ParseWarnings,
) -> Result<(u64, bool)> {
    let element_name = local_element_name(start.name().as_ref())?;
    if element_name != "p"
        && (attribute_value(start, b"end", decoder)?.is_some()
            || attribute_value(start, b"dur", decoder)?.is_some())
    {
        warnings.ignored_container_intervals =
            warnings.ignored_container_intervals.saturating_add(1);
    }
    let Some(value) = attribute_value(start, b"begin", decoder)? else {
        return Ok((parent_begin, parent_valid));
    };
    let Some(begin) = parse_time_expression(&value) else {
        warnings.unsupported_times = warnings.unsupported_times.saturating_add(1);
        return Ok((parent_begin, false));
    };
    let Some(begin) = parent_begin.checked_add(begin) else {
        warnings.unsupported_times = warnings.unsupported_times.saturating_add(1);
        return Ok((parent_begin, false));
    };
    Ok((begin, parent_valid))
}

fn build_cue(
    start: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
    parent_begin: u64,
    absolute_begin: u64,
    valid_timing: bool,
    warnings: &mut ParseWarnings,
) -> Result<CueBuilder> {
    let mut cue = CueBuilder {
        start_ms: Some(absolute_begin),
        valid: valid_timing,
        ..CueBuilder::default()
    };
    cue.identifier = attribute_value(start, b"id", decoder)?;
    let end = attribute_value(start, b"end", decoder)?;
    let duration = attribute_value(start, b"dur", decoder)?;
    if let Some(end) = end {
        if let Some(end) = parse_time_expression(&end)
            && let Some(end) = parent_begin.checked_add(end)
        {
            cue.end_ms = Some(end);
        } else {
            cue.valid = false;
            warnings.unsupported_times = warnings.unsupported_times.saturating_add(1);
        }
    } else if let Some(duration) = duration {
        if let Some(duration) = parse_time_expression(&duration)
            && let Some(end) = absolute_begin.checked_add(duration)
        {
            cue.end_ms = Some(end);
        } else {
            cue.valid = false;
            warnings.unsupported_times = warnings.unsupported_times.saturating_add(1);
        }
    }
    if let Some(id) = &cue.identifier
        && id.len() > 1024
    {
        cue.valid = false;
        warnings.malformed_cues = warnings.malformed_cues.saturating_add(1);
    }
    Ok(cue)
}

fn has_timing_attributes(start: &BytesStart<'_>) -> Result<bool> {
    for attribute in start.attributes() {
        let attribute = attribute
            .map_err(|error| Error::InvalidInput(format!("invalid TTML cue attribute: {error}")))?;
        if matches!(
            local_attribute_name(attribute.key.as_ref()),
            b"begin" | b"end" | b"dur"
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_sequential_time_container(
    start: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
) -> Result<bool> {
    Ok(attribute_value(start, b"timeContainer", decoder)?
        .is_some_and(|value| value.eq_ignore_ascii_case("seq")))
}

fn inspect_style_attributes(start: &BytesStart<'_>, warnings: &mut ParseWarnings) -> Result<()> {
    for attribute in start.attributes() {
        let attribute = attribute
            .map_err(|error| Error::InvalidInput(format!("invalid TTML attribute: {error}")))?;
        let key = attribute.key.as_ref();
        let local = local_attribute_name(key);
        if key.starts_with(b"tts:")
            || key.starts_with(b"itts:")
            || matches!(
                local,
                b"style"
                    | b"region"
                    | b"fontSize"
                    | b"fontFamily"
                    | b"color"
                    | b"backgroundColor"
                    | b"textAlign"
                    | b"displayAlign"
            )
        {
            warnings.styling = true;
        }
    }
    Ok(())
}

fn attribute_value(
    start: &BytesStart<'_>,
    name: &[u8],
    decoder: quick_xml::encoding::Decoder,
) -> Result<Option<String>> {
    for attribute in start.attributes() {
        let attribute = attribute
            .map_err(|error| Error::InvalidInput(format!("invalid TTML XML attribute: {error}")))?;
        if local_attribute_name(attribute.key.as_ref()) == name {
            let value = attribute
                .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, decoder)
                .map_err(|error| {
                    Error::InvalidInput(format!("invalid TTML attribute value: {error}"))
                })?;
            return Ok(Some(value.into_owned()));
        }
    }
    Ok(None)
}

fn local_element_name(name: &[u8]) -> Result<String> {
    let local = name.rsplit(|byte| *byte == b':').next().unwrap_or(name);
    std::str::from_utf8(local)
        .map(str::to_owned)
        .map_err(|error| Error::InvalidInput(format!("TTML element name is not UTF-8: {error}")))
}

fn local_attribute_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

fn append_cue_text(cue: &mut CueBuilder, text: &str, warnings: &mut ParseWarnings) -> Result<()> {
    for character in text.chars() {
        if character.is_control() && !matches!(character, '\t' | '\n' | '\r') {
            warnings.controls = warnings.controls.saturating_add(1);
            continue;
        }
        cue.text.push(character);
        if cue.text.len() > MAX_TTML_CUE_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "TTML cue text exceeds {MAX_TTML_CUE_TEXT_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

fn finish_cue(
    cue: CueBuilder,
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut ParseWarnings,
    cue_count: &mut usize,
    rendered_text_bytes: &mut usize,
) -> Result<()> {
    let text = cue.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !cue.valid || text.is_empty() {
        warnings.malformed_cues = warnings.malformed_cues.saturating_add(1);
        return Ok(());
    }
    let start = cue.start_ms.unwrap_or(0);
    if cue.end_ms.is_some_and(|end| end < start) {
        warnings.malformed_cues = warnings.malformed_cues.saturating_add(1);
        return Ok(());
    }
    *cue_count = cue_count.saturating_add(1);
    if *cue_count > MAX_TTML_CUES {
        return Err(Error::LimitExceeded(format!(
            "TTML exceeds {MAX_TTML_CUES} cues"
        )));
    }
    let mut line = format!(
        "{}–{}",
        format_time(start),
        cue.end_ms.map_or_else(|| "?".into(), format_time)
    );
    if let Some(identifier) = cue.identifier.filter(|identifier| !identifier.is_empty()) {
        line.push_str(" [");
        line.push_str(&sanitize_identifier(&identifier));
        line.push(']');
    }
    line.push_str("  ");
    line.push_str(&text);
    *rendered_text_bytes = rendered_text_bytes.saturating_add(line.len());
    if *rendered_text_bytes > MAX_TTML_RENDERED_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "TTML rendered text exceeds {MAX_TTML_RENDERED_TEXT_BYTES} bytes"
        )));
    }
    blocks.push(HtmlBlock::Paragraph { text: line });
    Ok(())
}

fn parse_time_expression(value: &str) -> Option<u64> {
    if value.contains(':') {
        let (clock, fraction) = value
            .split_once('.')
            .map_or((value, None), |(clock, fraction)| (clock, Some(fraction)));
        let parts = clock.split(':').collect::<Vec<_>>();
        let [hours, minutes, seconds] = parts.as_slice() else {
            return None;
        };
        if hours.len() < 2
            || minutes.len() != 2
            || seconds.len() != 2
            || !hours.bytes().all(|byte| byte.is_ascii_digit())
            || !minutes.bytes().all(|byte| byte.is_ascii_digit())
            || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let hours = hours.parse::<u64>().ok()?;
        let minutes = minutes.parse::<u64>().ok()?;
        let seconds = seconds.parse::<u64>().ok()?;
        if minutes > 59 || seconds > 59 {
            return None;
        }
        let mut milliseconds = hours
            .checked_mul(3_600_000)?
            .checked_add(minutes.checked_mul(60_000)?)?
            .checked_add(seconds.checked_mul(1_000)?)?;
        if let Some(fraction) = fraction {
            milliseconds = milliseconds.checked_add(parse_fraction_ms(fraction)?)?;
        }
        return Some(milliseconds);
    }

    let (number, factor) = if let Some(value) = value.strip_suffix("ms") {
        (value, 1)
    } else if let Some(value) = value.strip_suffix('h') {
        (value, 3_600_000)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60_000)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1_000)
    } else {
        return None;
    };
    parse_decimal_scaled(number, factor)
}

fn parse_decimal_scaled(value: &str, factor: u64) -> Option<u64> {
    let (whole, fraction) = value
        .split_once('.')
        .map_or((value, None), |(whole, fraction)| (whole, Some(fraction)));
    if whole.is_empty() || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let whole = whole.parse::<u64>().ok()?.checked_mul(factor)?;
    let Some(fraction) = fraction else {
        return Some(whole);
    };
    if fraction.is_empty()
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let digits = fraction.parse::<u64>().ok()?;
    let scale = 10u64.checked_pow(fraction.len() as u32)?;
    let fractional_ms = digits
        .checked_mul(factor)?
        .checked_add(scale / 2)?
        .checked_div(scale)?;
    whole.checked_add(fractional_ms)
}

fn parse_fraction_ms(fraction: &str) -> Option<u64> {
    if fraction.is_empty()
        || fraction.len() > 9
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let digits = fraction.parse::<u64>().ok()?;
    let scale = 10u64.checked_pow(fraction.len() as u32)?;
    digits
        .checked_mul(1_000)?
        .checked_add(scale / 2)?
        .checked_div(scale)
}

fn format_time(milliseconds: u64) -> String {
    let hours = milliseconds / 3_600_000;
    let minutes = milliseconds / 60_000 % 60;
    let seconds = milliseconds / 1_000 % 60;
    let millis = milliseconds % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

fn sanitize_identifier(identifier: &str) -> String {
    identifier
        .chars()
        .filter(|character| !character.is_control())
        .take(1024)
        .collect()
}

fn warning_messages(warnings: ParseWarnings) -> Vec<String> {
    let mut messages = Vec::new();
    if warnings.malformed_cues > 0 {
        messages.push(format!(
            "{} malformed or empty TTML cues were skipped",
            warnings.malformed_cues
        ));
    }
    if warnings.unsupported_times > 0 {
        messages.push(format!(
            "{} TTML time expression(s) were unsupported; affected cues were skipped",
            warnings.unsupported_times
        ));
    }
    if warnings.flattened_nested_timing > 0 {
        messages.push(format!(
            "{} TTML span timing value(s) were flattened into their paragraph cue",
            warnings.flattened_nested_timing
        ));
    }
    if warnings.ignored_container_intervals > 0 {
        messages.push(format!(
            "{} TTML container end/dur boundary value(s) were ignored; paragraph cue end/dur values are applied",
            warnings.ignored_container_intervals
        ));
    }
    if warnings.styling {
        messages.push("TTML styles, regions, and visual layout were not reproduced".into());
    }
    if warnings.image_content > 0 {
        messages.push(format!(
            "{} TTML image/data element(s) were omitted; only text-profile content is rendered",
            warnings.image_content
        ));
    }
    if warnings.controls > 0 {
        messages.push(format!(
            "{} TTML control character(s) were removed",
            warnings.controls
        ));
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter"
    xmlns:tts="http://www.w3.org/ns/ttml#styling" ttp:timeBase="media">
  <head><styling><style xml:id="bold" tts:fontWeight="bold"/></styling>
    <layout><region xml:id="bottom" tts:origin="10% 80%"/></layout></head>
  <body><div>
    <p xml:id="cue-1" begin="00:00:01.000" end="00:00:03.500" region="bottom">Hello <span style="bold">world</span>.<br/>字幕</p>
    <p begin="3.5s" dur="2s">Later &amp; next.</p>
  </div></body>
</tt>"#;

    #[test]
    fn parses_ttml_text_clock_times_offsets_styles_and_identifiers() {
        let (blocks, warnings) = parse_ttml_blocks(SIMPLE, 10_000).unwrap();
        let paragraphs = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Paragraph { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(paragraphs.len(), 2);
        assert!(paragraphs[0].contains("00:00:01.000–00:00:03.500 [cue-1]  Hello world. · 字幕"));
        assert!(paragraphs[1].contains("00:00:03.500–00:00:05.500  Later & next."));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("styles, regions"))
        );
    }

    #[test]
    fn detects_ttml_by_namespace_and_rejects_other_xml() {
        assert!(looks_like_ttml_prefix(SIMPLE.as_bytes()));
        assert!(!looks_like_ttml_prefix(
            b"<root xmlns=\"urn:example\"><tt>text</tt></root>"
        ));
    }

    #[test]
    fn rejects_doctypes_non_media_time_bases_and_sequential_timing() {
        let doctype =
            r#"<!DOCTYPE tt [<!ENTITY x "expanded">]><tt xmlns="http://www.w3.org/ns/ttml"/>"#;
        assert!(parse_ttml_blocks(doctype, 1_000).is_err());
        let clock = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" ttp:timeBase="clock"/>"#;
        assert!(matches!(
            parse_ttml_blocks(clock, 1_000),
            Err(Error::Unsupported(_))
        ));
        let sequential =
            r#"<tt xmlns="http://www.w3.org/ns/ttml"><body timeContainer="seq"/></tt>"#;
        assert!(matches!(
            parse_ttml_blocks(sequential, 1_000),
            Err(Error::Unsupported(_))
        ));
        let multiple_roots = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body/></tt><tt xmlns="http://www.w3.org/ns/ttml"/>"#;
        assert!(parse_ttml_blocks(multiple_roots, 1_000).is_err());
    }

    #[test]
    fn skips_frame_based_times_with_warning_and_preserves_safe_text() {
        let source = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><p begin="00:00:01:12" end="00:00:02:00">not timed</p><p begin="00:00:02.000" end="00:00:03.000">&lt;script&gt;literal&lt;/script&gt;</p></body></tt>"#;
        let (blocks, warnings) = parse_ttml_blocks(source, 1_000).unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("time expression"))
        );
        assert!(blocks.iter().any(|block| matches!(block, HtmlBlock::Paragraph { text } if text.contains("<script>literal</script>"))));
    }

    #[test]
    fn warns_when_parent_end_does_not_clip_child_cue() {
        let source = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><div begin="1s" dur="5s"><p begin="1s" end="6s">clipped later</p></div></body></tt>"#;
        let (blocks, warnings) = parse_ttml_blocks(source, 1_000).unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("container end/dur boundary"))
        );
        assert!(blocks.iter().any(|block| matches!(block, HtmlBlock::Paragraph { text } if text.contains("00:00:02.000–00:00:07.000"))));
    }

    #[test]
    fn omits_external_image_references_and_enforces_event_limits() {
        let source = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body><p begin="0s" end="1s">Caption <image src="https://example.invalid/caption.png"/></p></body></tt>"#;
        let (blocks, warnings) = parse_ttml_blocks(source, 1_000).unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("image/data element"))
        );
        assert!(blocks.iter().all(|block| match block {
            HtmlBlock::Heading { text, .. } | HtmlBlock::Paragraph { text } =>
                !text.contains("example.invalid"),
            _ => true,
        }));
        assert!(matches!(
            parse_ttml_blocks(SIMPLE, 1),
            Err(Error::LimitExceeded(_))
        ));
    }
}
