//! Bounded SubRip (SRT) and WebVTT subtitle previews.
//!
//! Cue text and timing are rendered as inert text. No HTML, script, style,
//! media, or external reference from a track is executed or fetched.

use std::borrow::Cow;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_SUBTITLE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SUBTITLE_LINES: usize = 1_000_000;
const MAX_SUBTITLE_LINE_BYTES: usize = 1024 * 1024;
const MAX_CUES: usize = 100_000;
const MAX_CUE_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_TOTAL_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SubtitleKind {
    Srt,
    Vtt,
}

impl SubtitleKind {
    fn label(self) -> &'static str {
        match self {
            Self::Srt => "SubRip subtitles",
            Self::Vtt => "WebVTT subtitles",
        }
    }

    fn source_format(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Vtt => "vtt",
        }
    }
}

#[derive(Default)]
struct ParseWarnings {
    malformed_blocks: usize,
    comments: usize,
    metadata_blocks: usize,
    cue_settings: usize,
    inline_markup: usize,
    controls: usize,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    kind: SubtitleKind,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SUBTITLE_BYTES),
        kind.label(),
    )?;
    let (text, encoding_note) = decode_subtitle_input(bytes, kind)?;
    let (blocks, mut warnings) = parse_blocks(&text, kind)?;
    if let Some(note) = encoding_note {
        warnings.push(note.into());
    }
    let mut page_sink = SubtitlePageSink {
        inner: sink,
        warnings: &warnings,
        kind,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

struct SubtitlePageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
    kind: SubtitleKind,
}

impl PageConsumer for SubtitlePageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = self.kind.source_format().into();
        if page.title.is_empty() {
            page.title = self.kind.label().into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_subtitle_prefix(prefix: &[u8]) -> Option<SubtitleKind> {
    let text = String::from_utf8_lossy(prefix);
    let normalized = text.trim_start_matches('\u{feff}');
    if normalized.lines().next().is_some_and(|line| {
        line == "WEBVTT" || line.starts_with("WEBVTT ") || line.starts_with("WEBVTT\t")
    }) {
        return Some(SubtitleKind::Vtt);
    }
    for line in normalized.lines().take(64) {
        if let Some((start, end)) = line.split_once("-->") {
            if parse_timestamp(start.trim(), SubtitleKind::Srt).is_some()
                && end
                    .split_whitespace()
                    .next()
                    .is_some_and(|end| parse_timestamp(end, SubtitleKind::Srt).is_some())
            {
                return Some(SubtitleKind::Srt);
            }
            if parse_timestamp(start.trim(), SubtitleKind::Vtt).is_some()
                && end
                    .split_whitespace()
                    .next()
                    .is_some_and(|end| parse_timestamp(end, SubtitleKind::Vtt).is_some())
            {
                return Some(SubtitleKind::Vtt);
            }
        }
    }
    None
}

fn parse_blocks(text: &str, kind: SubtitleKind) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let normalized_storage = normalize_line_terminators(text);
    let normalized = normalized_storage.trim_start_matches('\u{feff}');
    validate(normalized, kind)?;
    let mut lines = normalized
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .peekable();
    let mut header = None::<String>;
    let mut warnings = ParseWarnings::default();
    if kind == SubtitleKind::Vtt {
        let first = lines.next().unwrap_or_default();
        if !(first == "WEBVTT" || first.starts_with("WEBVTT ") || first.starts_with("WEBVTT\t")) {
            return Err(Error::InvalidInput(
                "WebVTT input must start with the WEBVTT signature".into(),
            ));
        }
        let suffix = first.strip_prefix("WEBVTT").unwrap_or_default().trim();
        if !suffix.is_empty() {
            let (suffix, had_controls) = sanitize_text(suffix);
            warnings.controls += usize::from(had_controls);
            header = Some(suffix);
        }
        while lines.peek().is_some_and(|line| !line.trim().is_empty()) {
            let line = lines.next().unwrap_or_default().trim();
            if !line.is_empty() {
                let (line, had_controls) = sanitize_text(line);
                warnings.controls += usize::from(had_controls);
                header = Some(match header.take() {
                    Some(existing) => format!("{existing} · {line}"),
                    None => line,
                });
                if header
                    .as_ref()
                    .is_some_and(|header| header.len() > MAX_TOTAL_TEXT_BYTES)
                {
                    return Err(Error::LimitExceeded(format!(
                        "WebVTT header exceeds {MAX_TOTAL_TEXT_BYTES} bytes"
                    )));
                }
            }
        }
    }

    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: kind.label().into(),
    }];
    let mut rendered_text_bytes = 0usize;
    if let Some(header) = header {
        let (header, had_controls) = sanitize_text(&header);
        warnings.controls += usize::from(had_controls);
        rendered_text_bytes = "Track: ".len() + header.len();
        if rendered_text_bytes > MAX_TOTAL_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "subtitle text exceeds {MAX_TOTAL_TEXT_BYTES} bytes"
            )));
        }
        blocks.push(HtmlBlock::Paragraph {
            text: format!("Track: {header}"),
        });
    }

    let mut cue_count = 0usize;
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let first_line = line.trim();
        if kind == SubtitleKind::Vtt
            && (first_line == "NOTE"
                || first_line.starts_with("NOTE ")
                || first_line.starts_with("NOTE\t"))
        {
            warnings.comments = warnings.comments.saturating_add(1);
            while lines.peek().is_some_and(|line| !line.trim().is_empty()) {
                lines.next();
            }
            continue;
        }
        if kind == SubtitleKind::Vtt && (first_line == "STYLE" || first_line == "REGION") {
            warnings.metadata_blocks = warnings.metadata_blocks.saturating_add(1);
            while lines.peek().is_some_and(|line| !line.trim().is_empty()) {
                lines.next();
            }
            continue;
        }

        let mut cue_lines = vec![line];
        while lines.peek().is_some_and(|line| !line.trim().is_empty()) {
            cue_lines.push(lines.next().unwrap_or_default());
            if cue_lines.len() > MAX_SUBTITLE_LINES {
                return Err(Error::LimitExceeded(format!(
                    "{} cue contains too many lines",
                    kind.label()
                )));
            }
        }

        let timing_index = cue_lines
            .iter()
            .take(2)
            .position(|line| line.contains("-->"));
        let Some(timing_index) = timing_index else {
            warnings.malformed_blocks = warnings.malformed_blocks.saturating_add(1);
            continue;
        };
        let Some((start, end, has_settings)) = parse_timing(cue_lines[timing_index], kind) else {
            warnings.malformed_blocks = warnings.malformed_blocks.saturating_add(1);
            continue;
        };
        if start > end {
            warnings.malformed_blocks = warnings.malformed_blocks.saturating_add(1);
            continue;
        }
        if has_settings {
            warnings.cue_settings = warnings.cue_settings.saturating_add(1);
        }

        let identifier = if timing_index == 1 {
            let id = cue_lines[0].trim();
            if id.len() > MAX_IDENTIFIER_BYTES {
                warnings.malformed_blocks = warnings.malformed_blocks.saturating_add(1);
                continue;
            }
            let (id, had_controls) = sanitize_text(id);
            warnings.controls += usize::from(had_controls);
            Some(id)
        } else {
            None
        };
        let mut cue_text = String::new();
        for text_line in cue_lines.iter().skip(timing_index + 1) {
            if !cue_text.is_empty() {
                cue_text.push_str(" · ");
            }
            let (clean, had_markup, had_controls) = flatten_markup(text_line);
            warnings.inline_markup += usize::from(had_markup);
            warnings.controls += usize::from(had_controls);
            cue_text.push_str(&clean);
            if cue_text.len() > MAX_CUE_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "subtitle cue text exceeds {MAX_CUE_TEXT_BYTES} bytes"
                )));
            }
        }
        if cue_text.trim().is_empty() {
            warnings.malformed_blocks = warnings.malformed_blocks.saturating_add(1);
            continue;
        }
        let timing_text = format!("{}–{}", format_time(start), format_time(end));
        cue_count += 1;
        if cue_count > MAX_CUES {
            return Err(Error::LimitExceeded(format!(
                "{} exceeds {MAX_CUES} cues",
                kind.label()
            )));
        }
        let mut prefix = timing_text;
        if let Some(identifier) = identifier.filter(|id| !id.is_empty()) {
            prefix.push_str(" [");
            prefix.push_str(&identifier);
            prefix.push(']');
        }
        prefix.push_str("  ");
        rendered_text_bytes = rendered_text_bytes
            .checked_add(cue_text.len())
            .and_then(|value| value.checked_add(prefix.len()))
            .ok_or_else(|| Error::LimitExceeded("subtitle text size overflow".into()))?;
        if rendered_text_bytes > MAX_TOTAL_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "subtitle text exceeds {MAX_TOTAL_TEXT_BYTES} bytes"
            )));
        }
        prefix.push_str(&cue_text);
        blocks.push(HtmlBlock::Paragraph { text: prefix });
    }

    if cue_count == 0 {
        return Err(Error::InvalidInput(format!(
            "{} contains no valid subtitle cues",
            kind.label()
        )));
    }
    let mut messages = Vec::new();
    push_summary(
        &mut messages,
        warnings.malformed_blocks,
        "malformed subtitle blocks were skipped",
    );
    push_summary(
        &mut messages,
        warnings.comments,
        "WebVTT NOTE comment blocks were omitted",
    );
    push_summary(
        &mut messages,
        warnings.metadata_blocks,
        "WebVTT STYLE/REGION blocks were omitted",
    );
    push_summary(
        &mut messages,
        warnings.cue_settings,
        "cue positioning/settings were not reproduced",
    );
    push_summary(
        &mut messages,
        warnings.inline_markup,
        "subtitle inline styling and timed-text tags were flattened to readable text",
    );
    push_summary(
        &mut messages,
        warnings.controls,
        "subtitle control characters were removed",
    );
    Ok((blocks, messages))
}

fn validate(text: &str, kind: SubtitleKind) -> Result<()> {
    if text.len() as u64 > MAX_SUBTITLE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "{} exceeds {MAX_SUBTITLE_BYTES} bytes",
            kind.label()
        )));
    }
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_SUBTITLE_LINES {
            return Err(Error::LimitExceeded(format!(
                "{} exceeds {MAX_SUBTITLE_LINES} lines",
                kind.label()
            )));
        }
        if line.len() > MAX_SUBTITLE_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "subtitle line exceeds {MAX_SUBTITLE_LINE_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

fn decode_subtitle_input(
    bytes: Vec<u8>,
    kind: SubtitleKind,
) -> Result<(String, Option<&'static str>)> {
    let bytes = match String::from_utf8(bytes) {
        Ok(text) => return Ok((text, None)),
        Err(error) => error.into_bytes(),
    };
    if kind == SubtitleKind::Vtt {
        return Err(Error::InvalidInput(
            "WebVTT input must be valid UTF-8".into(),
        ));
    }
    let (encoding, payload, note) = if let Some(payload) = bytes.strip_prefix(&[0xff, 0xfe]) {
        (
            encoding_rs::UTF_16LE,
            payload,
            "SubRip input was decoded from UTF-16LE" as &'static str,
        )
    } else if let Some(payload) = bytes.strip_prefix(&[0xfe, 0xff]) {
        (
            encoding_rs::UTF_16BE,
            payload,
            "SubRip input was decoded from UTF-16BE",
        )
    } else {
        (
            encoding_rs::WINDOWS_1252,
            bytes.as_slice(),
            "SubRip input was not UTF-8; Windows-1252 fallback decoding was used",
        )
    };
    let (decoded, had_errors) = encoding.decode_without_bom_handling(payload);
    if had_errors {
        return Err(Error::InvalidInput(format!(
            "{} contains invalid text for the detected encoding",
            kind.label()
        )));
    }
    let decoded = decoded.into_owned();
    if decoded.len() as u64 > MAX_SUBTITLE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "decoded {} exceeds {MAX_SUBTITLE_BYTES} bytes",
            kind.label()
        )));
    }
    Ok((decoded, Some(note)))
}

fn normalize_line_terminators(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            normalized.push('\n');
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
        } else {
            normalized.push(character);
        }
    }
    Cow::Owned(normalized)
}

fn parse_timing(line: &str, kind: SubtitleKind) -> Option<(u64, u64, bool)> {
    let (left, right) = line.split_once("-->")?;
    let start = parse_timestamp(left.trim(), kind)?;
    let mut right_fields = right.split_whitespace();
    let end = parse_timestamp(right_fields.next()?, kind)?;
    Some((start, end, right_fields.next().is_some()))
}

fn parse_timestamp(value: &str, kind: SubtitleKind) -> Option<u64> {
    let separator = match kind {
        SubtitleKind::Srt => ',',
        SubtitleKind::Vtt => '.',
    };
    let (clock, millis) = value.split_once(separator)?;
    if millis.len() != 3 || !millis.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut parts = clock.split(':');
    let first = parts.next()?;
    let second = parts.next()?;
    let third = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (hours, minutes, seconds) = match (kind, third) {
        (SubtitleKind::Srt, Some(seconds)) => {
            let hours = first;
            let minutes = second;
            if hours.len() < 2 || minutes.len() != 2 || seconds.len() != 2 {
                return None;
            }
            (
                hours.parse::<u64>().ok()?,
                minutes.parse::<u64>().ok()?,
                seconds.parse::<u64>().ok()?,
            )
        }
        (SubtitleKind::Vtt, None) => {
            let minutes = first;
            let seconds = second;
            if minutes.len() != 2 || seconds.len() != 2 {
                return None;
            }
            (
                0,
                minutes.parse::<u64>().ok()?,
                seconds.parse::<u64>().ok()?,
            )
        }
        (SubtitleKind::Vtt, Some(seconds)) => {
            let hours = first;
            let minutes = second;
            if hours.is_empty() || minutes.len() != 2 || seconds.len() != 2 {
                return None;
            }
            (
                hours.parse::<u64>().ok()?,
                minutes.parse::<u64>().ok()?,
                seconds.parse::<u64>().ok()?,
            )
        }
        _ => return None,
    };
    if minutes > 59 || seconds > 59 {
        return None;
    }
    let millis = millis.parse::<u64>().ok()?;
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(seconds.checked_mul(1_000)?)?
        .checked_add(millis)
}

fn format_time(milliseconds: u64) -> String {
    let hours = milliseconds / 3_600_000;
    let minutes = milliseconds / 60_000 % 60;
    let seconds = milliseconds / 1_000 % 60;
    let millis = milliseconds % 1_000;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

fn flatten_markup(input: &str) -> (String, bool, bool) {
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;
    let mut had_markup = false;
    let mut had_controls = false;
    while index < input.len() {
        let rest = &input[index..];
        let Some('<') = rest.chars().next() else {
            let character = rest.chars().next().unwrap_or_default();
            if character.is_control() {
                had_controls = true;
                if character == '\t' {
                    output.push(' ');
                }
            } else {
                output.push(character);
            }
            index += character.len_utf8();
            continue;
        };
        let Some(relative_end) = rest.find('>') else {
            output.push('<');
            index += 1;
            continue;
        };
        if relative_end > 512 {
            output.push('<');
            index += 1;
            continue;
        }
        let body = rest[1..relative_end].trim();
        let normalized = body.trim_start_matches('/');
        let tag = normalized
            .split(|character: char| character.is_whitespace() || character == '.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let closing = body.starts_with('/');
        let known = matches!(
            tag.as_str(),
            "b" | "i" | "u" | "font" | "c" | "v" | "lang" | "ruby" | "rt"
        ) || is_vtt_timestamp(body);
        if known {
            had_markup = true;
            if tag == "v" && !closing {
                let speaker = normalized.strip_prefix('v').unwrap_or_default().trim();
                let speaker =
                    speaker.trim_matches(|character| character == '<' || character == '>');
                if !speaker.is_empty() {
                    let (clean_speaker, speaker_controls) = sanitize_text(speaker);
                    had_controls |= speaker_controls;
                    if !clean_speaker.is_empty() {
                        if !output.is_empty() {
                            output.push(' ');
                        }
                        output.push_str(&clean_speaker);
                        output.push_str(": ");
                    }
                }
            }
            index += relative_end + 1;
        } else {
            output.push('<');
            index += 1;
        }
    }
    (output.trim().to_string(), had_markup, had_controls)
}

fn is_vtt_timestamp(value: &str) -> bool {
    parse_timestamp(value, SubtitleKind::Vtt).is_some()
}

fn sanitize_text(value: &str) -> (String, bool) {
    let mut had_controls = false;
    let text = value
        .chars()
        .filter_map(|character| {
            if character.is_control() {
                had_controls = true;
                (character == '\t').then_some(' ')
            } else {
                Some(character)
            }
        })
        .collect();
    (text, had_controls)
}

fn push_summary(messages: &mut Vec<String>, count: usize, description: &str) {
    if count > 0 {
        messages.push(format!("{count} {description}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_srt_sequences_unicode_and_multiline_cues() {
        let input = "1\r\n00:00:01,250 --> 00:00:02,500\r\nHello <i>世界</i>\r\nSecond line\r\n\r\n2\r\n01:02:03,004 --> 01:02:04,000\r\nNext\r\n";
        let (blocks, warnings) = parse_blocks(input, SubtitleKind::Srt).unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("inline styling"))
        );
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Paragraph { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            text[0],
            "00:00:01.250–00:00:02.500 [1]  Hello 世界 · Second line"
        );
        assert!(text[1].starts_with("01:02:03.004–01:02:04.000"));
    }

    #[test]
    fn parses_webvtt_header_voice_tags_and_omits_metadata_blocks() {
        let input = "WEBVTT - English\nKind: captions\n\nNOTE generated file\ncomment\n\nSTYLE\n::cue { color: red }\n\nintro\n00:01.000 --> 00:02.000 align:start\n<v Narrator>Hello <b>there</b></v>\n";
        let (blocks, warnings) = parse_blocks(input, SubtitleKind::Vtt).unwrap();
        assert!(warnings.iter().any(|warning| warning.contains("NOTE")));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("STYLE/REGION"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("positioning/settings"))
        );
        assert!(blocks.iter().any(|block| matches!(block, HtmlBlock::Paragraph { text } if text.contains("Narrator: Hello there"))));
        assert!(blocks.iter().any(|block| matches!(block, HtmlBlock::Paragraph { text } if text.contains("English · Kind: captions"))));
    }

    #[test]
    fn rejects_malformed_timestamps_and_enforces_budgets() {
        assert!(
            parse_blocks(
                "1\n00:00:01.000 --> 00:00:02.000\nText\n",
                SubtitleKind::Srt
            )
            .is_err()
        );
        assert!(
            parse_blocks(
                "WEBVTT\n\n00:99.000 --> 01:00.000\nText\n",
                SubtitleKind::Vtt
            )
            .is_err()
        );
        assert!(parse_timestamp("00:00:02,000", SubtitleKind::Srt).is_some());
        assert!(parse_timestamp("99:59.000", SubtitleKind::Vtt).is_none());
    }

    #[test]
    fn detects_extensionless_srt_and_webvtt() {
        assert_eq!(
            looks_like_subtitle_prefix(b"WEBVTT\n\n"),
            Some(SubtitleKind::Vtt)
        );
        assert_eq!(
            looks_like_subtitle_prefix(b"1\n00:00:01,000 --> 00:00:02,000\nText\n"),
            Some(SubtitleKind::Srt)
        );
        assert_eq!(looks_like_subtitle_prefix(b"00:00:01 --> 00:00:02\n"), None);
    }

    #[test]
    fn decodes_common_subrip_encodings_and_keeps_webvtt_utf8_only() {
        let cp1252 = b"1\n00:00:01,000 --> 00:00:02,000\nCaf\xe9\n".to_vec();
        let (text, note) = decode_subtitle_input(cp1252, SubtitleKind::Srt).unwrap();
        assert_eq!(
            note,
            Some("SubRip input was not UTF-8; Windows-1252 fallback decoding was used")
        );
        assert!(text.contains("Café"));

        let utf16: Vec<u8> = "1\r00:00:01,000 --> 00:00:02,000\rCafé\r"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut utf16le = vec![0xff, 0xfe];
        utf16le.extend(utf16);
        let (text, note) = decode_subtitle_input(utf16le, SubtitleKind::Srt).unwrap();
        assert_eq!(note, Some("SubRip input was decoded from UTF-16LE"));
        let (blocks, _) = parse_blocks(&text, SubtitleKind::Srt).unwrap();
        assert!(
            blocks.iter().any(
                |block| matches!(block, HtmlBlock::Paragraph { text } if text.contains("Café"))
            )
        );

        assert!(decode_subtitle_input(b"WEBVTT\n\n\xFF".to_vec(), SubtitleKind::Vtt).is_err());
    }
}
