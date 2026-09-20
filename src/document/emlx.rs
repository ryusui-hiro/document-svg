//! Bounded Apple Mail `.emlx` single-message wrapper preview.
//!
//! The decimal byte-count line is used to isolate the RFC 5322/MIME message;
//! optional Apple property-list metadata is ignored and never interpreted.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::eml::{self, MAX_EML_BYTES};
use crate::error::{Error, Result};

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let max_bytes = options.max_input_bytes.min(MAX_EML_BYTES);
    let bytes = read_limited_file(path, max_bytes, "Apple Mail EMLX input")?;
    let (message, metadata) = split_emlx(&bytes, max_bytes)?;
    let (mut warnings, page_count) = eml::render_message_bytes(message, options, "emlx", 0, sink)?;
    if page_count == 0 {
        return Err(Error::InvalidInput(
            "Apple Mail EMLX message produced no preview pages".into(),
        ));
    }
    if metadata {
        warnings.push("Apple Mail property-list metadata was ignored".into());
    }
    Ok(warnings)
}

fn split_emlx(bytes: &[u8], max_bytes: u64) -> Result<(&[u8], bool)> {
    let newline = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or_else(|| Error::InvalidInput("Apple Mail EMLX byte-count line is missing".into()))?;
    let count_line = bytes[..newline]
        .strip_suffix(b"\r")
        .unwrap_or(&bytes[..newline]);
    if count_line.is_empty() || !count_line.iter().all(u8::is_ascii_digit) {
        return Err(Error::InvalidInput(
            "Apple Mail EMLX byte count must be unsigned decimal digits".into(),
        ));
    }
    let count = std::str::from_utf8(count_line)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| Error::InvalidInput("Apple Mail EMLX byte count is invalid".into()))?;
    if count == 0 {
        return Err(Error::InvalidInput(
            "Apple Mail EMLX contains an empty message".into(),
        ));
    }
    if count > max_bytes {
        return Err(Error::LimitExceeded(format!(
            "Apple Mail EMLX message declares {count} bytes; maximum is {max_bytes}"
        )));
    }
    let start = newline + 1;
    let end = start
        .checked_add(usize::try_from(count).map_err(|_| {
            Error::LimitExceeded("Apple Mail EMLX message length exceeds addressable memory".into())
        })?)
        .ok_or_else(|| Error::LimitExceeded("Apple Mail EMLX message length overflowed".into()))?;
    if end > bytes.len() {
        return Err(Error::InvalidInput(format!(
            "Apple Mail EMLX declares {count} message bytes but only {} remain",
            bytes.len().saturating_sub(start)
        )));
    }
    let message = &bytes[start..end];
    let metadata = bytes[end..].iter().any(|byte| !byte.is_ascii_whitespace());
    Ok((message, metadata))
}

pub(crate) fn looks_like_emlx_prefix(bytes: &[u8]) -> bool {
    let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
        return false;
    };
    let count = bytes[..newline]
        .strip_suffix(b"\r")
        .unwrap_or(&bytes[..newline]);
    if count.is_empty()
        || count.len() > 20
        || !count.iter().all(u8::is_ascii_digit)
        || std::str::from_utf8(count)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .is_none_or(|length| length == 0 || length > MAX_EML_BYTES)
    {
        return false;
    }
    let header_bytes = &bytes[newline + 1..];
    header_bytes
        .split(|byte| *byte == b'\n')
        .take(32)
        .any(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            [
                b"From:".as_slice(),
                b"To:",
                b"Date:",
                b"Subject:",
                b"Content-Type:",
                b"MIME-Version:",
            ]
            .iter()
            .any(|prefix| {
                line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(prefix)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolates_exact_message_bytes_and_ignores_metadata() {
        let message = b"Subject: Example\n\nBody\n";
        let bytes = [
            message.len().to_string().as_bytes(),
            b"\n",
            message,
            b"<plist/>",
        ]
        .concat();
        let (actual, metadata) = split_emlx(&bytes, 1024).unwrap();
        assert_eq!(actual, message);
        assert!(metadata);
        assert!(looks_like_emlx_prefix(&bytes));
    }

    #[test]
    fn rejects_bad_and_truncated_lengths() {
        assert!(split_emlx(b"-1\nSubject: x\n\n", 1024).is_err());
        assert!(split_emlx(b"100\nSubject: x\n\n", 1024).is_err());
        assert!(split_emlx(b"12\nSubject: x\n", 1024).is_err());
    }
}
