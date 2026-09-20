//! Bounded mbox mailbox archive preview using the shared MIME e-mail renderer.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::eml::{self, MAX_EML_BYTES};
use crate::error::{Error, Result};

const MAX_MBOX_MESSAGES: usize = 10_000;
const MAX_MBOX_LINE_BYTES: usize = 1024 * 1024;
const MAX_MBOX_LINES: usize = 1_000_000;

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
            "mbox input exceeds maximum limit of {max_bytes} bytes"
        )));
    }
    preflight_mailbox(&bytes)?;

    let mut messages = Vec::<(usize, usize)>::new();
    let mut separator_count = 0usize;
    let mut current_message_start = None;
    let mut offset = 0usize;
    for line_with_eol in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line_start = offset;
        offset += line_with_eol.len();
        let content = line_with_eol.strip_suffix(b"\n").unwrap_or(line_with_eol);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        if is_mbox_separator(content) {
            separator_count += 1;
            if separator_count > MAX_MBOX_MESSAGES {
                return Err(Error::LimitExceeded(format!(
                    "mbox contains more than {MAX_MBOX_MESSAGES} messages"
                )));
            }
            if let Some(start) = current_message_start {
                if line_start <= start {
                    return Err(Error::InvalidInput(
                        "mbox contains an empty message between separators".into(),
                    ));
                }
                messages.push((start, line_start));
            } else if bytes[..line_start]
                .iter()
                .any(|byte| !byte.is_ascii_whitespace())
            {
                return Err(Error::InvalidInput(
                    "mbox contains unsupported data before its first From_ separator".into(),
                ));
            }
            current_message_start = Some(offset);
        }
    }
    if let Some(start) = current_message_start {
        if start >= bytes.len() {
            return Err(Error::InvalidInput(
                "mbox ends with a separator but no message".into(),
            ));
        }
        messages.push((start, bytes.len()));
    }
    if messages.is_empty() {
        return Err(Error::InvalidInput(
            "mbox contains no RFC 4155 From_ message separators".into(),
        ));
    }
    if messages.len() > options.max_pages.min(MAX_MBOX_MESSAGES) {
        return Err(Error::LimitExceeded(format!(
            "mbox contains {} messages; maximum is {} pages",
            messages.len(),
            options.max_pages.min(MAX_MBOX_MESSAGES)
        )));
    }

    let mut warnings = Vec::new();
    let mut output_pages = 0usize;
    for (start, end) in messages {
        let remaining_pages = options.max_pages.saturating_sub(output_pages);
        let mut message_options = options.clone();
        message_options.max_pages = remaining_pages;
        let (message_warnings, page_count) = eml::render_message_bytes(
            &bytes[start..end],
            &message_options,
            "mbox",
            output_pages,
            sink,
        )?;
        if page_count == 0 {
            return Err(Error::InvalidInput(
                "mbox message produced no output pages".into(),
            ));
        }
        output_pages = output_pages
            .checked_add(page_count)
            .ok_or_else(|| Error::LimitExceeded("mbox page count overflowed".into()))?;
        for warning in message_warnings {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    Ok(warnings)
}

pub(crate) fn looks_like_mbox_prefix(bytes: &[u8]) -> bool {
    let first_line = bytes
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let first_line = first_line.strip_suffix(b"\r").unwrap_or(first_line);
    is_mbox_separator(first_line)
}

fn preflight_mailbox(bytes: &[u8]) -> Result<()> {
    let mut line_count = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        line_count += 1;
        if line_count > MAX_MBOX_LINES {
            return Err(Error::LimitExceeded(format!(
                "mbox input exceeds {MAX_MBOX_LINES} lines"
            )));
        }
        if line.len() > MAX_MBOX_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "mbox line exceeds {MAX_MBOX_LINE_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

fn is_mbox_separator(line: &[u8]) -> bool {
    let Some(envelope) = line.strip_prefix(b"From ") else {
        return false;
    };
    let fields = envelope
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() < 6 || fields[0].is_empty() {
        return false;
    }
    let weekday = fields[1].strip_suffix(b",").unwrap_or(fields[1]);
    let month = fields[2];
    let valid_weekday = [
        b"Mon".as_slice(),
        b"Tue",
        b"Wed",
        b"Thu",
        b"Fri",
        b"Sat",
        b"Sun",
    ]
    .iter()
    .any(|candidate| weekday.eq_ignore_ascii_case(candidate));
    let valid_month = [
        b"Jan".as_slice(),
        b"Feb",
        b"Mar",
        b"Apr",
        b"May",
        b"Jun",
        b"Jul",
        b"Aug",
        b"Sep",
        b"Oct",
        b"Nov",
        b"Dec",
    ]
    .iter()
    .any(|candidate| month.eq_ignore_ascii_case(candidate));
    let valid_day = std::str::from_utf8(fields[3])
        .ok()
        .and_then(|day| day.parse::<u8>().ok())
        .is_some_and(|day| (1..=31).contains(&day));
    let time = fields[4];
    let valid_time = {
        let mut components = time.split(|byte| *byte == b':');
        matches!(
            (components.next(), components.next(), components.next(), components.next()),
            (Some(hour), Some(minute), Some(second), None)
                if hour.len() == 2
                    && minute.len() == 2
                    && second.len() == 2
                    && hour.iter().chain(minute).chain(second).all(u8::is_ascii_digit)
        )
    };
    let has_year = fields[5..]
        .iter()
        .any(|field| field.len() == 4 && field.iter().all(u8::is_ascii_digit));
    valid_weekday && valid_month && valid_day && valid_time && has_year
}
