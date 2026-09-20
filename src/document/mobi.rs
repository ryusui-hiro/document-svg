//! Bounded PalmDOC/MOBI text preview.
//!
//! The reader supports the original PalmDOC compression modes (uncompressed
//! and PalmDOC LZ77) and the common UTF-8/Windows-1252 encodings. Huff/CDIC,
//! DRM/encrypted records, image records, and executable content are rejected
//! or omitted with warnings. The decoded HTML-like text is passed through the
//! same inert HTML subset used by the web-document reader.

use std::collections::HashMap;
use std::path::Path;

use encoding_rs::WINDOWS_1252;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{
    parse_html_blocks_with_inline_images_budgeted, render_blocks_to_pages_with_warnings,
};
use crate::error::{Error, Result};

const MAX_MOBI_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_MOBI_RECORDS: usize = 100_000;
const MAX_MOBI_TEXT_RECORDS: usize = 100_000;
const MAX_MOBI_TEXT_BYTES: usize = 128 * 1024 * 1024;
const MAX_MOBI_RECORD_OUTPUT_BYTES: usize = 1024 * 1024;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.windows(4).any(|window| window == b"MOBI")
        || (bytes.len() >= 78 && &bytes[60..64] == b"BOOK")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_MOBI_INPUT_BYTES),
        "MOBI/PalmDOC input",
    )?;
    convert_bytes(&bytes, options, sink)
}

fn convert_bytes(
    bytes: &[u8],
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let (name, html, mut warnings) = parse_records(bytes)?;
    let html = if html.to_ascii_lowercase().contains("<html") {
        html
    } else {
        let escaped = quick_xml::escape::escape(&html);
        format!("<html><body><p>{escaped}</p></body></html>")
    };
    let (blocks, mut parser_warnings, _) = parse_html_blocks_with_inline_images_budgeted(
        &html,
        options.max_xml_events,
        usize::try_from(options.max_input_bytes)
            .unwrap_or(usize::MAX)
            .min(512 * 1024 * 1024),
        &HashMap::new(),
    )?;
    if !name.is_empty() {
        parser_warnings.insert(0, format!("MOBI book title: {name}"));
    }
    warnings.append(&mut parser_warnings);
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "MOBI book contains no renderable text".into(),
        ));
    }
    warnings.insert(
        0,
        "MOBI/PalmDOC HTML is shown through the safe subset; images, CSS, links, scripts, and exact reader layout are not reconstructed".into(),
    );
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn parse_records(bytes: &[u8]) -> Result<(String, String, Vec<String>)> {
    if bytes.len() < 78 {
        return Err(Error::InvalidInput(
            "MOBI/PalmDOC file is shorter than its Palm database header".into(),
        ));
    }
    let record_count = be_u16(bytes, 76)
        .ok_or_else(|| Error::InvalidInput("MOBI record count is truncated".into()))?
        as usize;
    if record_count == 0 || record_count > MAX_MOBI_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "MOBI record count exceeds {MAX_MOBI_RECORDS}"
        )));
    }
    let list_end = 78usize
        .checked_add(
            record_count
                .checked_mul(8)
                .ok_or_else(|| Error::LimitExceeded("MOBI record list overflowed".into()))?,
        )
        .ok_or_else(|| Error::LimitExceeded("MOBI record list overflowed".into()))?;
    if list_end > bytes.len() {
        return Err(Error::InvalidInput("MOBI record list is truncated".into()));
    }
    let mut offsets = Vec::with_capacity(record_count);
    for index in 0..record_count {
        let offset = be_u32(bytes, 78 + index * 8)
            .ok_or_else(|| Error::InvalidInput("MOBI record offset is truncated".into()))?
            as usize;
        if offset < list_end
            || offset > bytes.len()
            || offsets.last().is_some_and(|previous| offset < *previous)
        {
            return Err(Error::InvalidInput(
                "MOBI record offsets are invalid".into(),
            ));
        }
        offsets.push(offset);
    }
    let record = |index: usize| -> Result<&[u8]> {
        let start = *offsets
            .get(index)
            .ok_or_else(|| Error::InvalidInput("MOBI record index is out of range".into()))?;
        let end = offsets.get(index + 1).copied().unwrap_or(bytes.len());
        if end < start || end > bytes.len() {
            return Err(Error::InvalidInput(
                "MOBI record boundary is invalid".into(),
            ));
        }
        Ok(&bytes[start..end])
    };
    let header = record(0)?;
    if header.len() < 16 {
        return Err(Error::InvalidInput(
            "MOBI PalmDOC header is truncated".into(),
        ));
    }
    let compression = be_u16(header, 0).unwrap_or_default();
    let text_length = be_u32(header, 4).unwrap_or_default() as usize;
    let text_records = be_u16(header, 8).unwrap_or_default() as usize;
    let record_size = be_u16(header, 10).unwrap_or_default();
    let encryption = be_u16(header, 12).unwrap_or_default();
    if encryption != 0 {
        return Err(Error::Unsupported(
            "encrypted/DRM MOBI records are not decoded".into(),
        ));
    }
    if text_records == 0 || text_records > MAX_MOBI_TEXT_RECORDS {
        return Err(Error::LimitExceeded(format!(
            "MOBI text record count exceeds {MAX_MOBI_TEXT_RECORDS}"
        )));
    }
    if record_size != 0 && record_size > 4096 {
        return Err(Error::InvalidInput(
            "MOBI PalmDOC text record size is invalid".into(),
        ));
    }
    let mut warnings = Vec::new();
    if compression == 17480 {
        return Err(Error::Unsupported(
            "MOBI Huff/CDIC compression is not supported".into(),
        ));
    }
    if compression != 1 && compression != 2 {
        return Err(Error::Unsupported(format!(
            "MOBI compression type {compression} is unsupported"
        )));
    }
    let encoding = if header.len() >= 32 && &header[16..20] == b"MOBI" {
        match be_u32(header, 16 + 12).unwrap_or(1252) {
            65001 => None,
            1252 => Some(WINDOWS_1252),
            other => {
                warnings.push(format!(
                    "MOBI text encoding {other} is unsupported; Windows-1252 fallback is used"
                ));
                Some(WINDOWS_1252)
            }
        }
    } else {
        Some(WINDOWS_1252)
    };
    let name = if header.len() >= 92 && &header[16..20] == b"MOBI" {
        let offset = be_u32(header, 16 + 68).unwrap_or(0) as usize;
        let length = be_u32(header, 16 + 72).unwrap_or(0) as usize;
        if length > 0
            && offset
                .checked_add(length)
                .is_some_and(|end| end <= header.len())
        {
            String::from_utf8_lossy(&header[offset..offset + length])
                .trim()
                .to_owned()
        } else {
            String::from_utf8_lossy(&bytes[..32])
                .trim_matches('\0')
                .trim()
                .to_owned()
        }
    } else {
        String::from_utf8_lossy(&bytes[..32])
            .trim_matches('\0')
            .trim()
            .to_owned()
    };
    let mut text = Vec::new();
    for index in 0..text_records {
        let raw = record(index + 1)?;
        let decoded = if compression == 1 {
            raw.to_vec()
        } else {
            decompress_palmdoc(raw, MAX_MOBI_RECORD_OUTPUT_BYTES)?
        };
        text.extend_from_slice(&decoded);
        if text.len() > MAX_MOBI_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "MOBI decoded text exceeds {MAX_MOBI_TEXT_BYTES} bytes"
            )));
        }
    }
    if text_length > 0 {
        if text.len() < text_length {
            return Err(Error::InvalidInput(
                "MOBI text records are shorter than declared text length".into(),
            ));
        }
        text.truncate(text_length);
    }
    let html = if let Some(encoding) = encoding {
        encoding.decode(&text).0.into_owned()
    } else {
        String::from_utf8(text)
            .map_err(|error| Error::InvalidInput(format!("MOBI UTF-8 text is invalid: {error}")))?
    };
    Ok((name, html, warnings))
}

fn decompress_palmdoc(input: &[u8], max_output: usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut cursor = 0usize;
    while cursor < input.len() {
        let first = input[cursor];
        cursor += 1;
        match first {
            0x00..=0x08 => {
                let count = first as usize;
                let end = cursor.checked_add(count).ok_or_else(|| {
                    Error::LimitExceeded("MOBI compression length overflowed".into())
                })?;
                if end > input.len() {
                    return Err(Error::InvalidInput(
                        "MOBI PalmDOC literal run is truncated".into(),
                    ));
                }
                output.extend_from_slice(&input[cursor..end]);
                cursor = end;
            }
            0x09..=0x7f => output.push(first),
            0x80..=0xbf => {
                let second = *input.get(cursor).ok_or_else(|| {
                    Error::InvalidInput("MOBI PalmDOC back-reference is truncated".into())
                })?;
                cursor += 1;
                let distance = (((first as usize) << 5) | (second as usize >> 3)) & 0x07ff;
                let length = (second as usize & 0x07) + 3;
                if distance == 0 || distance > output.len() {
                    return Err(Error::InvalidInput(
                        "MOBI PalmDOC back-reference is invalid".into(),
                    ));
                }
                for _ in 0..length {
                    let index = output.len() - distance;
                    let byte = output[index];
                    output.push(byte);
                    if output.len() > max_output {
                        return Err(Error::LimitExceeded(
                            "MOBI PalmDOC record exceeds the decoded record limit".into(),
                        ));
                    }
                }
            }
            0xc0..=0xff => {
                output.push(b' ');
                output.push(first & 0x7f);
            }
        }
        if output.len() > max_output {
            return Err(Error::LimitExceeded(
                "MOBI PalmDOC record exceeds the decoded record limit".into(),
            ));
        }
    }
    Ok(output)
}

fn be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
    ]))
}

fn be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset + 1)?,
        *bytes.get(offset + 2)?,
        *bytes.get(offset + 3)?,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_palmdoc_compression_and_rejects_bad_backreferences() {
        assert_eq!(decompress_palmdoc(b"abc", 100).unwrap(), b"abc");
        assert_eq!(decompress_palmdoc(&[0xc1], 100).unwrap(), b" A");
        assert_eq!(
            decompress_palmdoc(&[3, b'a', b'b', b'c', 0x80, 0x18], 100).unwrap(),
            b"abcabc"
        );
        assert!(decompress_palmdoc(&[0x80, 0], 100).is_err());
    }

    #[test]
    fn detects_mobi_marker() {
        assert!(looks_like_prefix(b"prefix MOBI suffix"));
        assert!(!looks_like_prefix(b"plain text"));
    }
}
