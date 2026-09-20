//! Bounded text preview for Rich Text Format (RTF) documents.
//!
//! Text, Unicode escapes, paragraphs, tabs, and common ANSI escapes are kept.
//! Formatting is flattened to the shared page composer; bounded embedded PNG/JPEG
//! pictures are shown as flow blocks, while objects, other picture types, headers,
//! and advanced layout are reported as omissions.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use encoding_rs::Encoding;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ooxml::sniff_image_mime;

const MAX_RTF_GROUP_DEPTH: usize = 256;
const MAX_RTF_TOKENS: usize = 10_000_000;
const MAX_RTF_BLOCKS: usize = 200_000;
const MAX_RTF_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_RTF_BINARY_SKIP: usize = 64 * 1024 * 1024;
const MAX_RTF_IMAGE_REFERENCES: usize = 10_000;
const MAX_RTF_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_RTF_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_RTF_TOTAL_DATA_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_RTF_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_RTF_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;
const MAX_RTF_SCALE_PERCENT: u32 = 10_000;
const MAX_RTF_DISPLAY_SIDE: f64 = 100_000.0;

#[derive(Clone, Debug)]
struct RtfState {
    skip: bool,
    ignorable_destination: bool,
    unicode_fallback: usize,
    codepage: u16,
}

#[derive(Default)]
struct RtfImageBudget {
    references: usize,
    image_bytes: usize,
    data_uri_bytes: usize,
    pixels: u64,
}

#[derive(Default)]
struct RtfPictureCapture {
    mime: Option<&'static str>,
    bytes: Vec<u8>,
    pending_nibble: Option<u8>,
    decoded_byte_count: usize,
    exceeded_limit: bool,
    malformed: bool,
    width_goal_twips: Option<u32>,
    height_goal_twips: Option<u32>,
    scale_x_percent: Option<u32>,
    scale_y_percent: Option<u32>,
    invalid_dimensions: bool,
}

impl Default for RtfState {
    fn default() -> Self {
        Self {
            skip: false,
            ignorable_destination: false,
            unicode_fallback: 1,
            codepage: 1252,
        }
    }
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
            "RTF input exceeds maximum bytes ({})",
            options.max_input_bytes
        )));
    }
    let (blocks, mut warnings) = parse_rtf(&bytes)?;
    if !blocks
        .iter()
        .any(|block| !matches!(block, HtmlBlock::PageBreak | HtmlBlock::HorizontalRule))
    {
        return Err(Error::InvalidInput(
            "RTF document contains no renderable text".into(),
        ));
    }
    warnings.insert(
        0,
        "RTF character/paragraph formatting, tables, page geometry, and section pagination are approximated".into(),
    );
    render_blocks_to_pages(&blocks, sink, options)?;
    Ok(warnings)
}

fn parse_rtf(bytes: &[u8]) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut index = 0usize;
    while bytes.get(index).is_some_and(|byte| {
        byte.is_ascii_whitespace() || *byte == 0xef || *byte == 0xbb || *byte == 0xbf
    }) {
        index += 1;
    }
    if bytes.get(index..index + 5) != Some(b"{\\rtf") {
        return Err(Error::InvalidInput(
            "input does not start with a valid RTF header".into(),
        ));
    }

    let mut state = RtfState::default();
    let mut stack = Vec::new();
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let mut ansi_buffer = Vec::new();
    let mut output_bytes = 0usize;
    let mut token_count = 0usize;
    let mut fallback_to_skip = 0usize;
    let mut pending_high_surrogate = None;
    let mut saw_root = false;
    let mut saw_root_close = false;
    let mut warned_media = false;
    let mut warned_layout = false;
    let mut warned_codepage = false;
    let mut warned_invalid_encoding = false;
    let mut warned_break = false;
    let mut warned_picture_layout = false;
    let mut image_budget = RtfImageBudget::default();
    let mut warnings = Vec::new();

    while index < bytes.len() {
        token_count = token_count.saturating_add(1);
        if token_count > MAX_RTF_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "RTF token count exceeds {MAX_RTF_TOKENS}"
            )));
        }
        match bytes[index] {
            b'{' => {
                flush_ansi_buffer(
                    &mut ansi_buffer,
                    state.codepage,
                    &mut paragraph,
                    &mut output_bytes,
                    &mut warnings,
                    &mut warned_invalid_encoding,
                )?;
                if stack.len() >= MAX_RTF_GROUP_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "RTF group depth exceeds {MAX_RTF_GROUP_DEPTH}"
                    )));
                }
                stack.push(state.clone());
                if !saw_root {
                    saw_root = true;
                }
                index += 1;
            }
            b'}' => {
                flush_ansi_buffer(
                    &mut ansi_buffer,
                    state.codepage,
                    &mut paragraph,
                    &mut output_bytes,
                    &mut warnings,
                    &mut warned_invalid_encoding,
                )?;
                state = stack.pop().ok_or_else(|| {
                    Error::InvalidInput("RTF contains an unmatched closing brace".into())
                })?;
                index += 1;
                if stack.is_empty() {
                    saw_root_close = true;
                    break;
                }
            }
            b'\\' => {
                index += 1;
                let Some(&next) = bytes.get(index) else {
                    return Err(Error::InvalidInput("RTF ends after a backslash".into()));
                };
                if next.is_ascii_alphabetic() {
                    let start = index;
                    while bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
                        index += 1;
                    }
                    let word = std::str::from_utf8(&bytes[start..index])
                        .map_err(|_| Error::InvalidInput("RTF control word is not ASCII".into()))?
                        .to_ascii_lowercase();
                    let parameter_start = index;
                    if bytes.get(index) == Some(&b'-') {
                        index += 1;
                    }
                    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                        index += 1;
                    }
                    let parameter = if index > parameter_start {
                        std::str::from_utf8(&bytes[parameter_start..index])
                            .ok()
                            .and_then(|value| value.parse::<i32>().ok())
                    } else {
                        None
                    };
                    if bytes.get(index) == Some(&b' ') {
                        index += 1;
                    }
                    if word == "shppict" && !state.skip {
                        // Word wraps its preferred picture in an ignorable
                        // destination. Parse the nested \pict group instead
                        // of discarding the whole wrapper.
                        state.ignorable_destination = false;
                        continue;
                    }
                    if word == "pict" && !state.skip {
                        state.ignorable_destination = false;
                        flush_ansi_buffer(
                            &mut ansi_buffer,
                            state.codepage,
                            &mut paragraph,
                            &mut output_bytes,
                            &mut warnings,
                            &mut warned_invalid_encoding,
                        )?;
                        flush_pending_surrogate(
                            &mut pending_high_surrogate,
                            &mut paragraph,
                            &mut output_bytes,
                        )?;
                        flush_paragraph(&mut paragraph, &mut blocks)?;
                        check_block_limit(&blocks)?;
                        image_budget.references = image_budget.references.saturating_add(1);
                        let can_capture = image_budget.references <= MAX_RTF_IMAGE_REFERENCES;
                        if !can_capture {
                            push_rtf_warning_once(
                                &mut warnings,
                                "RTF picture references exceeded the supported limit; remaining pictures were omitted",
                            );
                        }
                        if !warned_picture_layout {
                            warnings.push(
                                "RTF picture anchors, crop, and text wrapping are approximated as centered flow blocks; declared goal size is used when present".into(),
                            );
                            warned_picture_layout = true;
                        }
                        let (closing_brace, picture, scanned_bytes) =
                            read_rtf_picture(bytes, index, can_capture)?;
                        token_count = token_count.saturating_add(scanned_bytes);
                        if token_count > MAX_RTF_TOKENS {
                            return Err(Error::LimitExceeded(format!(
                                "RTF token count exceeds {MAX_RTF_TOKENS}"
                            )));
                        }
                        index = closing_brace.saturating_add(1);
                        state = stack.pop().ok_or_else(|| {
                            Error::InvalidInput(
                                "RTF picture group has no matching opening brace".into(),
                            )
                        })?;
                        if can_capture {
                            attach_rtf_picture(
                                picture,
                                &mut image_budget,
                                &mut warnings,
                                &mut blocks,
                            )?;
                        }
                        check_block_limit(&blocks)?;
                        if stack.is_empty() {
                            saw_root_close = true;
                            break;
                        }
                        continue;
                    }
                    process_control_word(
                        &word,
                        parameter,
                        bytes,
                        &mut index,
                        &mut state,
                        &mut paragraph,
                        &mut ansi_buffer,
                        &mut blocks,
                        &mut output_bytes,
                        &mut fallback_to_skip,
                        &mut pending_high_surrogate,
                        &mut warnings,
                        &mut warned_media,
                        &mut warned_layout,
                        &mut warned_codepage,
                        &mut warned_invalid_encoding,
                        &mut warned_break,
                    )?;
                } else if next == b'\'' {
                    index += 1;
                    let pair = bytes.get(index..index.saturating_add(2)).ok_or_else(|| {
                        Error::InvalidInput("RTF hexadecimal escape is truncated".into())
                    })?;
                    let hex = std::str::from_utf8(pair).map_err(|_| {
                        Error::InvalidInput("RTF hexadecimal escape is not ASCII".into())
                    })?;
                    let value = u8::from_str_radix(hex, 16).map_err(|_| {
                        Error::InvalidInput("RTF hexadecimal escape is invalid".into())
                    })?;
                    index += 2;
                    if fallback_to_skip > 0 {
                        fallback_to_skip -= 1;
                    } else if !state.skip {
                        flush_pending_surrogate(
                            &mut pending_high_surrogate,
                            &mut paragraph,
                            &mut output_bytes,
                        )?;
                        buffer_ansi_byte(&mut ansi_buffer, value, output_bytes)?;
                    }
                } else {
                    index += 1;
                    let character = match next {
                        b'\\' => Some('\\'),
                        b'{' => Some('{'),
                        b'}' => Some('}'),
                        b'~' => Some('\u{00a0}'),
                        b'_' => Some('\u{2011}'),
                        b'-' => None,
                        b'*' => {
                            flush_ansi_buffer(
                                &mut ansi_buffer,
                                state.codepage,
                                &mut paragraph,
                                &mut output_bytes,
                                &mut warnings,
                                &mut warned_invalid_encoding,
                            )?;
                            state.ignorable_destination = true;
                            None
                        }
                        b'\r' | b'\n' => None,
                        _ => None,
                    };
                    if let Some(character) = character {
                        if fallback_to_skip > 0 {
                            fallback_to_skip -= 1;
                        } else if !state.skip {
                            match character {
                                special @ ('\\' | '{' | '}') => {
                                    flush_pending_surrogate(
                                        &mut pending_high_surrogate,
                                        &mut paragraph,
                                        &mut output_bytes,
                                    )?;
                                    buffer_ansi_byte(
                                        &mut ansi_buffer,
                                        special as u8,
                                        output_bytes,
                                    )?;
                                }
                                special => {
                                    flush_ansi_buffer(
                                        &mut ansi_buffer,
                                        state.codepage,
                                        &mut paragraph,
                                        &mut output_bytes,
                                        &mut warnings,
                                        &mut warned_invalid_encoding,
                                    )?;
                                    flush_pending_surrogate(
                                        &mut pending_high_surrogate,
                                        &mut paragraph,
                                        &mut output_bytes,
                                    )?;
                                    append_char(&mut paragraph, special, &mut output_bytes)?;
                                }
                            }
                        }
                    }
                }
            }
            b'\r' | b'\n' => index += 1,
            byte => {
                index += 1;
                if fallback_to_skip > 0 {
                    fallback_to_skip -= 1;
                } else if !state.skip {
                    flush_pending_surrogate(
                        &mut pending_high_surrogate,
                        &mut paragraph,
                        &mut output_bytes,
                    )?;
                    buffer_ansi_byte(&mut ansi_buffer, byte, output_bytes)?;
                }
            }
        }
    }

    if !saw_root || !saw_root_close || !stack.is_empty() {
        return Err(Error::InvalidInput(
            "RTF document has incomplete groups".into(),
        ));
    }
    if bytes[index..]
        .iter()
        .any(|byte| !byte.is_ascii_whitespace())
    {
        return Err(Error::InvalidInput(
            "RTF contains trailing bytes after its root group".into(),
        ));
    }
    flush_ansi_buffer(
        &mut ansi_buffer,
        state.codepage,
        &mut paragraph,
        &mut output_bytes,
        &mut warnings,
        &mut warned_invalid_encoding,
    )?;
    flush_pending_surrogate(
        &mut pending_high_surrogate,
        &mut paragraph,
        &mut output_bytes,
    )?;
    flush_paragraph(&mut paragraph, &mut blocks)?;
    Ok((blocks, warnings))
}

#[allow(clippy::too_many_arguments)]
fn process_control_word(
    word: &str,
    parameter: Option<i32>,
    bytes: &[u8],
    index: &mut usize,
    state: &mut RtfState,
    paragraph: &mut String,
    ansi_buffer: &mut Vec<u8>,
    blocks: &mut Vec<HtmlBlock>,
    output_bytes: &mut usize,
    fallback_to_skip: &mut usize,
    pending_high_surrogate: &mut Option<u16>,
    warnings: &mut Vec<String>,
    warned_media: &mut bool,
    warned_layout: &mut bool,
    warned_codepage: &mut bool,
    warned_invalid_encoding: &mut bool,
    warned_break: &mut bool,
) -> Result<()> {
    flush_ansi_buffer(
        ansi_buffer,
        state.codepage,
        paragraph,
        output_bytes,
        warnings,
        warned_invalid_encoding,
    )?;
    if state.ignorable_destination {
        state.skip = true;
        state.ignorable_destination = false;
    }
    if is_rtf_destination(word) {
        state.skip = true;
        if matches!(
            word,
            "pict" | "object" | "objdata" | "shppict" | "nonshppict"
        ) && !*warned_media
        {
            warnings.push("RTF embedded pictures and objects are omitted".into());
            *warned_media = true;
        }
        if matches!(
            word,
            "header" | "footer" | "headerl" | "headerr" | "footerl" | "footerr"
        ) && !warnings
            .iter()
            .any(|warning| warning == "RTF headers and footers are omitted")
        {
            warnings.push("RTF headers and footers are omitted".into());
        }
        return Ok(());
    }
    if state.skip {
        if word == "bin" {
            skip_binary(parameter, bytes, index)?;
            return Ok(());
        }
        return Ok(());
    }

    match word {
        "u" => {
            let value = parameter.ok_or_else(|| {
                Error::InvalidInput("RTF Unicode escape is missing its value".into())
            })?;
            if !(-32_768..=65_535).contains(&value) {
                return Err(Error::InvalidInput(
                    "RTF Unicode escape is outside a UTF-16 code unit".into(),
                ));
            }
            let unit = value as u16;
            append_utf16_unit(unit, pending_high_surrogate, paragraph, output_bytes)?;
            *fallback_to_skip = state.unicode_fallback;
        }
        "uc" => {
            let value = parameter.unwrap_or(1);
            state.unicode_fallback = usize::try_from(value.clamp(0, 32)).unwrap_or(1);
        }
        "ansicpg" => {
            let value = parameter.unwrap_or(1252).clamp(1, u16::MAX as i32) as u16;
            state.codepage = value;
            if encoding_for_codepage(value).is_none() && !*warned_codepage {
                warnings.push(format!(
                    "RTF ANSI code page {value} is unsupported; Windows-1252 fallback is used"
                ));
                *warned_codepage = true;
            }
        }
        "par" | "row" => flush_paragraph(paragraph, blocks)?,
        "line" => append_char(paragraph, '\n', output_bytes)?,
        "tab" | "cell" => append_text(paragraph, "    ", output_bytes)?,
        "page" => {
            flush_paragraph(paragraph, blocks)?;
            blocks.push(HtmlBlock::PageBreak);
            check_block_limit(blocks)?;
        }
        "sect" => {
            flush_paragraph(paragraph, blocks)?;
            blocks.push(HtmlBlock::PageBreak);
            check_block_limit(blocks)?;
            if !*warned_break {
                warnings.push("RTF section breaks are approximated as page breaks".into());
                *warned_break = true;
            }
        }
        "emdash" => append_char(paragraph, '\u{2014}', output_bytes)?,
        "endash" => append_char(paragraph, '\u{2013}', output_bytes)?,
        "bullet" => append_char(paragraph, '\u{2022}', output_bytes)?,
        "lquote" => append_char(paragraph, '\u{2018}', output_bytes)?,
        "rquote" => append_char(paragraph, '\u{2019}', output_bytes)?,
        "ldblquote" => append_char(paragraph, '\u{201c}', output_bytes)?,
        "rdblquote" => append_char(paragraph, '\u{201d}', output_bytes)?,
        "bin" => {
            if !*warned_media {
                warnings.push("RTF embedded binary data is omitted".into());
                *warned_media = true;
            }
            skip_binary(parameter, bytes, index)?;
        }
        "b" | "i" | "ul" | "ulnone" | "strike" | "fs" | "f" | "cf" | "highlight" | "pard"
        | "plain" | "qc" | "ql" | "qr" | "qj" | "li" | "ri" | "fi" | "sb" | "sa" | "sl"
        | "slmult" | "keep" | "keepn" | "widowctrl" | "viewkind" | "viewscale" | "deff"
        | "deflang" | "deflangfe" | "lang" => {
            if !*warned_layout {
                warnings.push("RTF font, character, and paragraph styling is flattened".into());
                *warned_layout = true;
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_rtf_destination(word: &str) -> bool {
    matches!(
        word,
        "fonttbl"
            | "colortbl"
            | "stylesheet"
            | "info"
            | "title"
            | "subject"
            | "author"
            | "manager"
            | "company"
            | "operator"
            | "category"
            | "keywords"
            | "comment"
            | "doccomm"
            | "creatim"
            | "revtim"
            | "printim"
            | "buptim"
            | "header"
            | "headerl"
            | "headerr"
            | "footer"
            | "footerl"
            | "footerr"
            | "pict"
            | "object"
            | "objdata"
            | "shppict"
            | "nonshppict"
            | "datastore"
            | "themedata"
            | "colorschememapping"
            | "listtable"
            | "listoverridetable"
            | "revtbl"
            | "generator"
            | "xmlnstbl"
            | "fldinst"
            | "listtext"
            | "annotation"
            | "atnauthor"
            | "atndate"
            | "atnicn"
    )
}

fn skip_binary(parameter: Option<i32>, bytes: &[u8], index: &mut usize) -> Result<()> {
    let length = usize::try_from(parameter.ok_or_else(|| {
        Error::InvalidInput("RTF binary payload is missing its byte count".into())
    })?)
    .map_err(|_| Error::InvalidInput("RTF binary payload length must be non-negative".into()))?;
    if length > MAX_RTF_BINARY_SKIP {
        return Err(Error::LimitExceeded(format!(
            "RTF binary payload exceeds {MAX_RTF_BINARY_SKIP} bytes"
        )));
    }
    let end = index
        .checked_add(length)
        .ok_or_else(|| Error::LimitExceeded("RTF binary payload length overflowed".into()))?;
    if end > bytes.len() {
        return Err(Error::InvalidInput(
            "RTF binary payload is truncated".into(),
        ));
    }
    *index = end;
    Ok(())
}

fn read_rtf_picture(
    bytes: &[u8],
    start: usize,
    allow_capture: bool,
) -> Result<(usize, RtfPictureCapture, usize)> {
    let mut index = start;
    let mut nested_depth = 0usize;
    let mut capture = RtfPictureCapture::default();
    let mut guid_hex_remaining = 0usize;
    let mut token_count = 0usize;
    while index < bytes.len() {
        token_count = token_count.saturating_add(1);
        if token_count > MAX_RTF_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "RTF picture token scan exceeds {MAX_RTF_TOKENS}"
            )));
        }
        match bytes[index] {
            b'}' if nested_depth == 0 => {
                if capture.pending_nibble.is_some() {
                    capture.malformed = true;
                }
                return Ok((
                    index,
                    capture,
                    index.saturating_add(1).saturating_sub(start),
                ));
            }
            b'}' => {
                nested_depth = nested_depth.saturating_sub(1);
                index += 1;
            }
            b'{' => {
                nested_depth = nested_depth.saturating_add(1);
                if nested_depth > MAX_RTF_GROUP_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "RTF picture group depth exceeds {MAX_RTF_GROUP_DEPTH}"
                    )));
                }
                index += 1;
            }
            b'\\' => {
                index += 1;
                let Some(&next) = bytes.get(index) else {
                    return Err(Error::InvalidInput(
                        "RTF picture ends after a backslash".into(),
                    ));
                };
                if next.is_ascii_alphabetic() {
                    let word_start = index;
                    while bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
                        index += 1;
                    }
                    let word = std::str::from_utf8(&bytes[word_start..index])
                        .map_err(|_| {
                            Error::InvalidInput("RTF picture control word is not ASCII".into())
                        })?
                        .to_ascii_lowercase();
                    let parameter_start = index;
                    if bytes.get(index) == Some(&b'-') {
                        index += 1;
                    }
                    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                        index += 1;
                    }
                    let parameter = if index > parameter_start {
                        std::str::from_utf8(&bytes[parameter_start..index])
                            .ok()
                            .and_then(|value| value.parse::<i32>().ok())
                    } else {
                        None
                    };
                    if bytes.get(index) == Some(&b' ') {
                        index += 1;
                    }
                    if word == "bin" {
                        let length = usize::try_from(parameter.ok_or_else(|| {
                            Error::InvalidInput(
                                "RTF picture binary payload is missing its byte count".into(),
                            )
                        })?)
                        .map_err(|_| {
                            Error::InvalidInput(
                                "RTF picture binary payload length must be non-negative".into(),
                            )
                        })?;
                        if length > MAX_RTF_BINARY_SKIP {
                            return Err(Error::LimitExceeded(format!(
                                "RTF binary payload exceeds {MAX_RTF_BINARY_SKIP} bytes"
                            )));
                        }
                        let end = index.checked_add(length).ok_or_else(|| {
                            Error::LimitExceeded(
                                "RTF picture binary payload length overflowed".into(),
                            )
                        })?;
                        let binary = bytes.get(index..end).ok_or_else(|| {
                            Error::InvalidInput("RTF picture binary payload is truncated".into())
                        })?;
                        if nested_depth == 0 && capture.mime.is_some() {
                            if capture.pending_nibble.take().is_some() {
                                capture.malformed = true;
                            }
                            append_rtf_picture_data(&mut capture, binary, allow_capture);
                        }
                        index = end;
                    } else if nested_depth == 0 {
                        match word.as_str() {
                            "pngblip" => capture.mime = Some("image/png"),
                            "jpegblip" | "jpgblip" => capture.mime = Some("image/jpeg"),
                            "blipuid" => guid_hex_remaining = 32,
                            "picwgoal" => {
                                capture.width_goal_twips =
                                    parse_rtf_positive_u32(parameter, 100_000_000);
                                capture.invalid_dimensions |= capture.width_goal_twips.is_none();
                            }
                            "pichgoal" => {
                                capture.height_goal_twips =
                                    parse_rtf_positive_u32(parameter, 100_000_000);
                                capture.invalid_dimensions |= capture.height_goal_twips.is_none();
                            }
                            "picscalex" => {
                                capture.scale_x_percent =
                                    parse_rtf_positive_u32(parameter, MAX_RTF_SCALE_PERCENT);
                                capture.invalid_dimensions |= capture.scale_x_percent.is_none();
                            }
                            "picscaley" => {
                                capture.scale_y_percent =
                                    parse_rtf_positive_u32(parameter, MAX_RTF_SCALE_PERCENT);
                                capture.invalid_dimensions |= capture.scale_y_percent.is_none();
                            }
                            _ => {}
                        }
                    }
                } else if next == b'\'' {
                    index += 1;
                    let pair = bytes.get(index..index.saturating_add(2)).ok_or_else(|| {
                        Error::InvalidInput("RTF picture hexadecimal escape is truncated".into())
                    })?;
                    let hex = std::str::from_utf8(pair).map_err(|_| {
                        Error::InvalidInput("RTF picture hexadecimal escape is not ASCII".into())
                    })?;
                    let value = u8::from_str_radix(hex, 16).map_err(|_| {
                        Error::InvalidInput("RTF picture hexadecimal escape is invalid".into())
                    })?;
                    if nested_depth == 0 && capture.mime.is_some() {
                        append_rtf_picture_data(&mut capture, &[value], allow_capture);
                    }
                    index += 2;
                } else {
                    index += 1;
                }
            }
            byte if nested_depth == 0 && capture.mime.is_some() => {
                index += 1;
                if byte.is_ascii_whitespace() {
                    continue;
                }
                if guid_hex_remaining > 0 {
                    if byte.is_ascii_hexdigit() {
                        guid_hex_remaining -= 1;
                    } else {
                        guid_hex_remaining = 0;
                        capture.malformed = true;
                    }
                    continue;
                }
                let Some(nibble) = rtf_hex_nibble(byte) else {
                    capture.malformed = true;
                    continue;
                };
                if let Some(high) = capture.pending_nibble.take() {
                    append_rtf_picture_data(&mut capture, &[(high << 4) | nibble], allow_capture);
                } else {
                    capture.pending_nibble = Some(nibble);
                }
            }
            _ => index += 1,
        }
    }
    Err(Error::InvalidInput(
        "RTF picture destination is incomplete".into(),
    ))
}

fn rtf_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_rtf_positive_u32(value: Option<i32>, maximum: u32) -> Option<u32> {
    let value = u32::try_from(value?).ok()?;
    (value > 0 && value <= maximum).then_some(value)
}

fn rtf_display_dimensions(
    picture: &RtfPictureCapture,
    pixel_width: u32,
    pixel_height: u32,
) -> Option<(u32, u32)> {
    if pixel_width == 0 || pixel_height == 0 {
        return None;
    }
    let aspect = f64::from(pixel_width) / f64::from(pixel_height);
    let (base_width, base_height) = match (picture.width_goal_twips, picture.height_goal_twips) {
        (Some(width), Some(height)) => (f64::from(width) / 20.0, f64::from(height) / 20.0),
        (Some(width), None) => {
            let width = f64::from(width) / 20.0;
            (width, width / aspect)
        }
        (None, Some(height)) => {
            let height = f64::from(height) / 20.0;
            (height * aspect, height)
        }
        (None, None) => (f64::from(pixel_width), f64::from(pixel_height)),
    };
    let width = base_width * f64::from(picture.scale_x_percent.unwrap_or(100)) / 100.0;
    let height = base_height * f64::from(picture.scale_y_percent.unwrap_or(100)) / 100.0;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let clamp_scale = (MAX_RTF_DISPLAY_SIDE / width.max(height)).min(1.0);
    Some((
        (width * clamp_scale).round().max(1.0) as u32,
        (height * clamp_scale).round().max(1.0) as u32,
    ))
}

fn append_rtf_picture_data(capture: &mut RtfPictureCapture, bytes: &[u8], allow_capture: bool) {
    capture.decoded_byte_count = capture.decoded_byte_count.saturating_add(bytes.len());
    if capture.decoded_byte_count > MAX_RTF_IMAGE_BYTES {
        capture.exceeded_limit = true;
        return;
    }
    if allow_capture {
        capture.bytes.extend_from_slice(bytes);
    }
}

fn attach_rtf_picture(
    picture: RtfPictureCapture,
    budget: &mut RtfImageBudget,
    warnings: &mut Vec<String>,
    blocks: &mut Vec<HtmlBlock>,
) -> Result<()> {
    if picture.exceeded_limit {
        push_rtf_warning_once(
            warnings,
            "RTF picture exceeded the per-image byte limit and was omitted",
        );
        return Ok(());
    }
    if picture.malformed || picture.pending_nibble.is_some() {
        push_rtf_warning_once(warnings, "malformed RTF picture data was omitted");
        return Ok(());
    }
    let Some(declared_mime) = picture.mime else {
        push_rtf_warning_once(
            warnings,
            "unsupported RTF picture types were omitted; only PNG and JPEG are embedded",
        );
        return Ok(());
    };
    let Some(mime) =
        sniff_image_mime(&picture.bytes).filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
    else {
        push_rtf_warning_once(warnings, "invalid RTF PNG/JPEG picture data was omitted");
        return Ok(());
    };
    if mime != declared_mime {
        push_rtf_warning_once(
            warnings,
            "RTF picture type marker did not match its image signature; the data signature was used",
        );
    }
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&picture.bytes, mime)
    else {
        push_rtf_warning_once(
            warnings,
            "invalid RTF PNG/JPEG picture dimensions were omitted",
        );
        return Ok(());
    };
    if picture.invalid_dimensions {
        push_rtf_warning_once(
            warnings,
            "RTF picture goal size or scale was invalid and was approximated from the image pixels",
        );
    }
    let (display_width, display_height) =
        rtf_display_dimensions(&picture, width, height).unwrap_or((width, height));
    let pixels = u64::from(width) * u64::from(height);
    let next_pixels = budget.pixels.saturating_add(pixels);
    if width == 0
        || height == 0
        || pixels > MAX_RTF_IMAGE_PIXELS
        || next_pixels > MAX_RTF_TOTAL_IMAGE_PIXELS
    {
        push_rtf_warning_once(
            warnings,
            "RTF pictures exceeded the per-image or total pixel limit and were omitted",
        );
        return Ok(());
    }
    let next_image_bytes = budget.image_bytes.saturating_add(picture.bytes.len());
    if next_image_bytes > MAX_RTF_TOTAL_IMAGE_BYTES {
        push_rtf_warning_once(
            warnings,
            "RTF pictures exceeded the total image byte limit and were omitted",
        );
        return Ok(());
    }
    let prefix = format!("data:{mime};base64,");
    let uri_bytes = prefix
        .len()
        .saturating_add(picture.bytes.len().div_ceil(3).saturating_mul(4));
    let next_uri_bytes = budget.data_uri_bytes.saturating_add(uri_bytes);
    if next_uri_bytes > MAX_RTF_TOTAL_DATA_URI_BYTES {
        push_rtf_warning_once(
            warnings,
            "RTF pictures exceeded the total data URI byte limit and were omitted",
        );
        return Ok(());
    }
    blocks.push(HtmlBlock::Image {
        href: format!("{prefix}{}", BASE64_STANDARD.encode(&picture.bytes)),
        pixel_width: display_width,
        pixel_height: display_height,
        alt: "Embedded RTF picture".into(),
    });
    budget.image_bytes = next_image_bytes;
    budget.data_uri_bytes = next_uri_bytes;
    budget.pixels = next_pixels;
    Ok(())
}

fn push_rtf_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn encoding_for_codepage(codepage: u16) -> Option<&'static Encoding> {
    let label: &'static [u8] = match codepage {
        874 => b"windows-874",
        932 => b"shift_jis",
        936 => b"gbk",
        949 => b"euc-kr",
        950 => b"big5",
        1250 => b"windows-1250",
        1251 => b"windows-1251",
        1252 => b"windows-1252",
        1253 => b"windows-1253",
        1254 => b"windows-1254",
        1255 => b"windows-1255",
        1256 => b"windows-1256",
        1257 => b"windows-1257",
        1258 => b"windows-1258",
        10000 => b"macintosh",
        28591 => b"iso-8859-1",
        28592 => b"iso-8859-2",
        28597 => b"iso-8859-7",
        28605 => b"iso-8859-15",
        65001 => b"utf-8",
        _ => return None,
    };
    Encoding::for_label(label)
}

fn buffer_ansi_byte(buffer: &mut Vec<u8>, byte: u8, output_bytes: usize) -> Result<()> {
    if output_bytes.saturating_add(buffer.len()).saturating_add(1) > MAX_RTF_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "RTF extracted text exceeds {MAX_RTF_TEXT_BYTES} bytes"
        )));
    }
    buffer.push(byte);
    Ok(())
}

fn flush_ansi_buffer(
    buffer: &mut Vec<u8>,
    codepage: u16,
    output: &mut String,
    output_bytes: &mut usize,
    warnings: &mut Vec<String>,
    warned_invalid_encoding: &mut bool,
) -> Result<()> {
    if buffer.is_empty() {
        return Ok(());
    }
    let encoding = encoding_for_codepage(codepage).unwrap_or(encoding_rs::WINDOWS_1252);
    let (decoded, had_errors) = encoding.decode_without_bom_handling(buffer);
    if had_errors && !*warned_invalid_encoding {
        warnings.push(
            "RTF contains invalid or incomplete ANSI byte sequences; replacement characters were inserted".into(),
        );
        *warned_invalid_encoding = true;
    }
    append_text(output, &decoded, output_bytes)?;
    buffer.clear();
    Ok(())
}

fn append_utf16_unit(
    unit: u16,
    pending: &mut Option<u16>,
    output: &mut String,
    output_bytes: &mut usize,
) -> Result<()> {
    if (0xd800..=0xdbff).contains(&unit) {
        flush_pending_surrogate(pending, output, output_bytes)?;
        *pending = Some(unit);
        return Ok(());
    }
    if (0xdc00..=0xdfff).contains(&unit) {
        if let Some(high) = pending.take() {
            let scalar = 0x1_0000 + (((high as u32 - 0xd800) << 10) | (unit as u32 - 0xdc00));
            let character = char::from_u32(scalar)
                .ok_or_else(|| Error::InvalidInput("invalid RTF surrogate pair".into()))?;
            return append_char(output, character, output_bytes);
        }
        return append_char(output, '\u{fffd}', output_bytes);
    }
    flush_pending_surrogate(pending, output, output_bytes)?;
    let character = char::from_u32(unit as u32)
        .ok_or_else(|| Error::InvalidInput("invalid RTF Unicode value".into()))?;
    append_char(output, character, output_bytes)
}

fn flush_pending_surrogate(
    pending: &mut Option<u16>,
    output: &mut String,
    output_bytes: &mut usize,
) -> Result<()> {
    if pending.take().is_some() {
        append_char(output, '\u{fffd}', output_bytes)?;
    }
    Ok(())
}

fn append_char(output: &mut String, character: char, total_bytes: &mut usize) -> Result<()> {
    let scalar = character as u32;
    if matches!(scalar, 0..=8 | 11..=12 | 14..=31) {
        return Err(Error::InvalidInput(
            "RTF text contains an XML-disallowed control character".into(),
        ));
    }
    *total_bytes = (*total_bytes).saturating_add(character.len_utf8());
    if *total_bytes > MAX_RTF_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "RTF extracted text exceeds {MAX_RTF_TEXT_BYTES} bytes"
        )));
    }
    output.push(character);
    Ok(())
}

fn append_text(output: &mut String, text: &str, total_bytes: &mut usize) -> Result<()> {
    *total_bytes = (*total_bytes).saturating_add(text.len());
    if *total_bytes > MAX_RTF_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "RTF extracted text exceeds {MAX_RTF_TEXT_BYTES} bytes"
        )));
    }
    output.push_str(text);
    Ok(())
}

fn flush_paragraph(paragraph: &mut String, blocks: &mut Vec<HtmlBlock>) -> Result<()> {
    let text = paragraph.trim().to_owned();
    paragraph.clear();
    if !text.is_empty() {
        blocks.push(HtmlBlock::Paragraph { text });
        check_block_limit(blocks)?;
    }
    Ok(())
}

fn check_block_limit(blocks: &[HtmlBlock]) -> Result<()> {
    if blocks.len() > MAX_RTF_BLOCKS {
        return Err(Error::LimitExceeded(format!(
            "RTF paragraph/block count exceeds {MAX_RTF_BLOCKS}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rtf_paragraphs_windows_1252_unicode_and_surrogate_pairs() {
        let rtf = br#"{\rtf1\ansi\ansicpg1252\uc1
{\fonttbl{\f0 Arial;}}
Hello \b bold\b0\par
Caf\'e9 \u8217? smile \u-10179?\u-8701?\par
{\pict\pngblip 89504e470d0a}After
}"#;
        let (blocks, warnings) = parse_rtf(rtf).unwrap();
        let paragraphs = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Paragraph { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(paragraphs, ["Hello bold", "Café ’ smile 😃", "After"]);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("invalid RTF PNG/JPEG picture data"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("picture anchors"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("styling is flattened"))
        );
    }

    #[test]
    fn decodes_multibyte_rtf_ansi_codepages() {
        let (blocks, warnings) = parse_rtf(br"{\rtf1\ansi\ansicpg932\uc1 \'82\'a0\par}").unwrap();
        assert!(matches!(&blocks[0], HtmlBlock::Paragraph { text } if text == "あ"));
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("code page 932 is unsupported"))
        );
    }

    #[test]
    fn skips_unsupported_rtf_binary_picture_and_rejects_incomplete_input() {
        let mut rtf = b"{\\rtf1 Before {\\pict\\bin4 ".to_vec();
        rtf.extend_from_slice(&[b'{', b'}', b'\\', 0]);
        rtf.extend_from_slice(b"} After\\par}");
        let (blocks, warnings) = parse_rtf(&rtf).unwrap();
        assert!(matches!(&blocks[0], HtmlBlock::Paragraph { text } if text == "Before"));
        assert!(matches!(&blocks[1], HtmlBlock::Paragraph { text } if text == "After"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("unsupported RTF picture types"))
        );

        assert!(matches!(
            parse_rtf(b"{\\rtf1{\\pict\\bin4 ab"),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            parse_rtf(b"{\\rtf1 missing close"),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn embeds_hex_encoded_png_between_surrounding_rtf_text() {
        let png = sample_png();
        let hex = png
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let rtf = format!(
            r#"{{\rtf1 Before {{\pict\pngblip\picw2\pich1\picwgoal1280\pichgoal320 {hex}}} After\par}}"#
        );
        let (blocks, warnings) = parse_rtf(rtf.as_bytes()).unwrap();

        assert!(matches!(&blocks[0], HtmlBlock::Paragraph { text } if text == "Before"));
        assert!(
            matches!(&blocks[1], HtmlBlock::Image { href, pixel_width: 64, pixel_height: 16, alt } if href.starts_with("data:image/png;base64,") && alt == "Embedded RTF picture")
        );
        assert!(matches!(&blocks[2], HtmlBlock::Paragraph { text } if text == "After"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("picture anchors"))
        );
        assert!(
            !warnings
                .iter()
                .any(|warning| warning.contains("invalid RTF PNG"))
        );
    }

    #[test]
    fn binary_picture_data_can_contain_rtf_delimiter_bytes() {
        let png = sample_png();
        let mut rtf = format!("{{\\rtf1 Before {{\\pict\\pngblip\\bin{} ", png.len()).into_bytes();
        rtf.extend_from_slice(&png);
        rtf.extend_from_slice(b"} After\\par}");
        let (blocks, _) = parse_rtf(&rtf).unwrap();

        assert!(matches!(&blocks[0], HtmlBlock::Paragraph { text } if text == "Before"));
        assert!(
            matches!(&blocks[1], HtmlBlock::Image { href, pixel_width: 2, pixel_height: 1, .. } if href.starts_with("data:image/png;base64,"))
        );
        assert!(matches!(&blocks[2], HtmlBlock::Paragraph { text } if text == "After"));
    }

    #[test]
    fn preserves_explicit_rtf_page_breaks_for_the_page_composer() {
        let (blocks, _) = parse_rtf(br"{\rtf1 First page\par\page Second page}").unwrap();
        assert!(matches!(&blocks[0], HtmlBlock::Paragraph { text } if text == "First page"));
        assert!(matches!(&blocks[1], HtmlBlock::PageBreak));
        assert!(matches!(&blocks[2], HtmlBlock::Paragraph { text } if text == "Second page"));
    }

    #[test]
    fn enforces_rtf_group_depth() {
        let mut rtf = b"{\\rtf1".to_vec();
        rtf.extend(std::iter::repeat_n(b'{', MAX_RTF_GROUP_DEPTH + 1));
        rtf.extend(std::iter::repeat_n(b'}', MAX_RTF_GROUP_DEPTH + 2));
        assert!(matches!(parse_rtf(&rtf), Err(Error::LimitExceeded(_))));
    }

    fn sample_png() -> Vec<u8> {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 0, 0, 255]).unwrap();
        }
        png
    }
}
