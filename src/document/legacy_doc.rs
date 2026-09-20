//! Bounded text previews for legacy Word Binary File Format documents.
//!
//! The WordDocument stream and File Information Block (FIB) identify the
//! main-story text. Formatting runs, original page geometry, tables, images,
//! headers/footnotes, and embedded objects are not reconstructed.

use std::fs::{self, File};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use cfb::CompoundFile;
use encoding_rs::{Encoding, WINDOWS_1252};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::Page;

const WORD_MAGIC: u16 = 0xA5EC;
const CFB_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const MAX_WORD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WORD_STREAM_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WORD_CLX_BYTES: usize = 16 * 1024 * 1024;
const MAX_WORD_MAIN_CPS: usize = 16 * 1024 * 1024;
const MAX_WORD_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_WORD_PIECES: usize = 1_000_000;
const MAX_WORD_CLX_PROPERTY_RECORDS: usize = 100_000;
const MAX_WORD_BLOCKS: usize = 200_000;
const MAX_WORD_FIELD_DEPTH: usize = 128;
const MAX_WORD_CFB_ENTRIES: usize = 50_000;
const MAX_WORD_PATH_BYTES: usize = 1024;
const MAX_WORD_PATHS_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const FIB_FCLCB_CLX_INDEX: usize = 33;

#[derive(Clone, Copy, Debug)]
struct Fib {
    lid: u16,
    complex: bool,
    encrypted: bool,
    which_table_stream: bool,
    is_template: bool,
    far_east: bool,
    cb_mac: usize,
    ccp_text: usize,
    total_cps: usize,
    fc_min: usize,
    fc_mac: usize,
    fc_clx: usize,
    lcb_clx: usize,
}

struct LegacyDocPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    source_format: &'static str,
    title: &'static str,
    warnings: &'a [String],
}

impl PageConsumer for LegacyDocPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = self.source_format.into();
        if page.title.is_empty() {
            page.title = self.title.into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

/// Detect a Word binary CFB file without treating every OLE file as `.doc`.
pub(crate) fn looks_like_legacy_doc(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() < 512 {
        return false;
    }
    if metadata.len() > MAX_WORD_BYTES {
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dot"))
        {
            let Ok(mut file) = File::open(path) else {
                return false;
            };
            let mut signature = [0; 8];
            return file.read_exact(&mut signature).is_ok() && signature == CFB_SIGNATURE;
        }
        return false;
    }
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(mut compound) = CompoundFile::open(file) else {
        return false;
    };
    if !compound.is_stream("WordDocument") {
        return false;
    }
    let Ok(mut stream) = compound.open_stream("WordDocument") else {
        return false;
    };
    let mut magic = [0; 2];
    stream.read_exact(&mut magic).is_ok() && u16::from_le_bytes(magic) == WORD_MAGIC
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    source_format: &'static str,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_WORD_BYTES),
        "legacy Word document",
    )?;
    if bytes.len() < 512 || bytes[..8] != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return Err(Error::InvalidInput(
            "legacy Word input is not a Compound File Binary document".into(),
        ));
    }

    let mut compound = CompoundFile::open(Cursor::new(bytes.as_slice())).map_err(|error| {
        Error::InvalidInput(format!("legacy Word compound file is invalid: {error}"))
    })?;
    validate_compound_entries(&compound, bytes.len())?;
    if !compound.is_stream("WordDocument") {
        return Err(Error::InvalidInput(
            "legacy Word file does not contain a WordDocument stream".into(),
        ));
    }
    let word_document = read_stream(&mut compound, "WordDocument", MAX_WORD_STREAM_BYTES)?;
    let fib = parse_fib(&word_document)?;
    if fib.encrypted {
        return Err(Error::Unsupported(
            "encrypted or obfuscated legacy Word documents are rejected; access controls are not bypassed".into(),
        ));
    }
    if fib.ccp_text == 0 {
        return Err(Error::InvalidInput(
            "legacy Word document has no main-story text".into(),
        ));
    }

    let mut warnings = vec![
        "Legacy Word character text is flowed onto A4 pages; original styles, pagination, and exact layout are not reconstructed".to_owned(),
        "Tables are flattened to text with cell separators; images, headers, footnotes, comments, and embedded objects are omitted, and macros are never executed".to_owned(),
    ];
    let encoding = encoding_for_lid(fib.lid, fib.far_east);
    let text = if fib.complex {
        if fib.lcb_clx == 0 {
            return Err(Error::InvalidInput(
                "complex legacy Word document has no piece table".into(),
            ));
        }
        let table_name = if fib.which_table_stream {
            "1Table"
        } else {
            "0Table"
        };
        if !compound.is_stream(table_name) {
            return Err(Error::InvalidInput(format!(
                "legacy Word FIB selects a missing {table_name} stream"
            )));
        }
        let clx = read_stream_range(
            &mut compound,
            table_name,
            fib.fc_clx,
            fib.lcb_clx,
            MAX_WORD_CLX_BYTES,
        )?;
        extract_piece_table_text(
            &word_document,
            fib.cb_mac,
            fib.ccp_text,
            &clx,
            encoding,
            &mut warnings,
        )?
    } else {
        extract_contiguous_text(&word_document, &fib, encoding, &mut warnings)?
    };
    if text.len() > MAX_WORD_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "legacy Word extracted text exceeds {MAX_WORD_TEXT_BYTES} bytes"
        )));
    }
    let blocks = text_to_blocks(&text, &mut warnings)?;
    if blocks.is_empty() {
        return Err(Error::InvalidInput(
            "legacy Word document contains no renderable main-story text".into(),
        ));
    }
    let title = if fib.is_template {
        "Legacy Word template"
    } else {
        "Legacy Word document"
    };
    let mut page_sink = LegacyDocPageSink {
        inner: sink,
        source_format,
        title,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_fib(word_document: &[u8]) -> Result<Fib> {
    if word_document.len() < 32 {
        return Err(Error::InvalidInput(
            "legacy WordDocument stream is shorter than its FIB".into(),
        ));
    }
    if read_u16(word_document, 0) != Some(WORD_MAGIC) {
        return Err(Error::InvalidInput(
            "legacy WordDocument stream has an invalid FIB signature".into(),
        ));
    }
    let version = read_u16(word_document, 2).unwrap_or_default();
    if !matches!(version, 0x00C1 | 0x00D9 | 0x0101 | 0x010C | 0x0112) {
        return Err(Error::Unsupported(format!(
            "legacy Word binary version 0x{version:04X} is outside the Word 97–2007 subset"
        )));
    }
    let flags = read_u16(word_document, 10).unwrap_or_default();
    let encrypted = flags & 0x0100 != 0;
    let which_table_stream = flags & 0x0200 != 0;
    let complex = flags & 0x0004 != 0;
    let is_template = flags & 0x0001 != 0;
    let far_east = flags & 0x4000 != 0;
    let lid = read_u16(word_document, 6).unwrap_or(0x0409);
    let fc_min = read_u32(word_document, 24).unwrap_or_default() as usize;
    let fc_mac = read_u32(word_document, 28).unwrap_or_default() as usize;

    let csw = read_u16(word_document, 32)
        .ok_or_else(|| Error::InvalidInput("legacy Word FIB is missing the FibRgW count".into()))?
        as usize;
    if csw > 256 {
        return Err(Error::LimitExceeded(
            "legacy Word FIB FibRgW count is excessive".into(),
        ));
    }
    let after_fib_rg_w =
        34usize
            .checked_add(csw.checked_mul(2).ok_or_else(|| {
                Error::LimitExceeded("legacy Word FIB FibRgW size overflowed".into())
            })?)
            .ok_or_else(|| Error::LimitExceeded("legacy Word FIB offset overflowed".into()))?;
    let cslw = read_u16(word_document, after_fib_rg_w)
        .ok_or_else(|| Error::InvalidInput("legacy Word FIB is missing the FibRgLw count".into()))?
        as usize;
    if !(11..=256).contains(&cslw) {
        return Err(Error::InvalidInput(format!(
            "legacy Word FIB has an invalid FibRgLw count {cslw}"
        )));
    }
    let fib_rg_lw = after_fib_rg_w + 2;
    let fib_rg_lw_bytes = cslw
        .checked_mul(4)
        .ok_or_else(|| Error::LimitExceeded("legacy Word FIB FibRgLw size overflowed".into()))?;
    let cb_mac = usize::try_from(
        read_u32(word_document, fib_rg_lw)
            .ok_or_else(|| Error::InvalidInput("legacy Word FIB is missing cbMac".into()))?,
    )
    .map_err(|_| Error::LimitExceeded("legacy Word cbMac does not fit this platform".into()))?;
    let ccp_text_raw = read_i32(word_document, fib_rg_lw + 12)
        .ok_or_else(|| Error::InvalidInput("legacy Word FIB is missing ccpText".into()))?;
    if ccp_text_raw < 0 || ccp_text_raw as usize > MAX_WORD_MAIN_CPS {
        return Err(Error::LimitExceeded(format!(
            "legacy Word main story declares {ccp_text_raw} character positions; maximum is {MAX_WORD_MAIN_CPS}"
        )));
    }
    let mut total_cps = ccp_text_raw as usize;
    let mut has_subdocument_text = false;
    for index in [4usize, 5, 7, 8, 9, 10] {
        let value = read_i32(word_document, fib_rg_lw + index * 4).ok_or_else(|| {
            Error::InvalidInput("legacy Word FIB story counts are truncated".into())
        })?;
        if value < 0 {
            return Err(Error::InvalidInput(
                "legacy Word FIB contains a negative story character count".into(),
            ));
        }
        let value = value as usize;
        has_subdocument_text |= value != 0;
        total_cps = total_cps
            .checked_add(value)
            .ok_or_else(|| Error::LimitExceeded("legacy Word story CP count overflowed".into()))?;
    }
    if has_subdocument_text {
        total_cps = total_cps
            .checked_add(1)
            .ok_or_else(|| Error::LimitExceeded("legacy Word story CP count overflowed".into()))?;
    }
    if total_cps > MAX_WORD_MAIN_CPS.saturating_mul(8) {
        return Err(Error::LimitExceeded(format!(
            "legacy Word stories declare {total_cps} CPs; maximum is {}",
            MAX_WORD_MAIN_CPS.saturating_mul(8)
        )));
    }
    if cb_mac > word_document.len() {
        return Err(Error::InvalidInput(
            "legacy Word FIB cbMac exceeds its WordDocument stream".into(),
        ));
    }
    let fc_lcb_count_offset = fib_rg_lw
        .checked_add(fib_rg_lw_bytes)
        .ok_or_else(|| Error::LimitExceeded("legacy Word FIB offset overflowed".into()))?;
    let fc_lcb_count = read_u16(word_document, fc_lcb_count_offset)
        .ok_or_else(|| Error::InvalidInput("legacy Word FIB is missing its fc/lcb count".into()))?
        as usize;
    if !matches!(fc_lcb_count, 0x005D | 0x006C | 0x0088 | 0x00A4 | 0x00B7) {
        return Err(Error::InvalidInput(format!(
            "legacy Word FIB has an unsupported fc/lcb count {fc_lcb_count}"
        )));
    }
    let fc_lcb_base = fc_lcb_count_offset + 2;
    let fc_lcb_bytes = fc_lcb_count
        .checked_mul(8)
        .ok_or_else(|| Error::LimitExceeded("legacy Word FIB fc/lcb size overflowed".into()))?;
    if fc_lcb_base
        .checked_add(fc_lcb_bytes)
        .is_none_or(|end| end > word_document.len())
    {
        return Err(Error::InvalidInput(
            "legacy Word FIB fc/lcb array is truncated".into(),
        ));
    }
    let clx_pair = fc_lcb_base + FIB_FCLCB_CLX_INDEX * 8;
    let fc_clx = read_u32(word_document, clx_pair).unwrap() as usize;
    let lcb_clx = read_u32(word_document, clx_pair + 4).unwrap() as usize;

    Ok(Fib {
        lid,
        complex,
        encrypted,
        which_table_stream,
        is_template,
        far_east,
        cb_mac,
        ccp_text: ccp_text_raw as usize,
        total_cps,
        fc_min,
        fc_mac,
        fc_clx,
        lcb_clx,
    })
}

fn extract_contiguous_text(
    word_document: &[u8],
    fib: &Fib,
    encoding: Option<&'static Encoding>,
    warnings: &mut Vec<String>,
) -> Result<String> {
    if fib.fc_min >= fib.fc_mac || fib.fc_mac > fib.cb_mac || fib.fc_mac > word_document.len() {
        return Err(Error::InvalidInput(
            "simple legacy Word text range is outside its WordDocument stream".into(),
        ));
    }
    let range = &word_document[fib.fc_min..fib.fc_mac];
    let unit_width = [1usize, 2]
        .into_iter()
        .find(|width| {
            let story_bytes = fib.total_cps.checked_mul(*width);
            let main_bytes = fib.ccp_text.checked_mul(*width);
            story_bytes == Some(range.len())
                || story_bytes.and_then(|size| size.checked_add(*width)) == Some(range.len())
                || main_bytes == Some(range.len())
                || main_bytes.and_then(|size| size.checked_add(*width)) == Some(range.len())
        })
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "simple legacy Word text range has {} bytes for {} main-story and {} total CPs",
                range.len(),
                fib.ccp_text,
                fib.total_cps
            ))
        })?;
    let main_bytes = fib
        .ccp_text
        .checked_mul(unit_width)
        .ok_or_else(|| Error::LimitExceeded("legacy Word main text size overflowed".into()))?;
    if main_bytes > range.len() {
        return Err(Error::InvalidInput(
            "legacy Word simple text range is shorter than its main story".into(),
        ));
    }
    if unit_width == 1 {
        decode_compressed(&range[..main_bytes], encoding, warnings)
    } else {
        decode_utf16le(&range[..main_bytes], warnings)
    }
}

fn extract_piece_table_text(
    word_document: &[u8],
    cb_mac: usize,
    ccp_text: usize,
    clx: &[u8],
    encoding: Option<&'static Encoding>,
    warnings: &mut Vec<String>,
) -> Result<String> {
    let plc_pcd = parse_clx(clx)?;
    let piece_count = (plc_pcd.len() - 4) / 12;
    if piece_count == 0 || piece_count > MAX_WORD_PIECES {
        return Err(Error::LimitExceeded(format!(
            "legacy Word piece table has {piece_count} pieces; maximum is {MAX_WORD_PIECES}"
        )));
    }
    let cp_bytes = (piece_count + 1)
        .checked_mul(4)
        .ok_or_else(|| Error::LimitExceeded("legacy Word CP table size overflowed".into()))?;
    let pcd_base = cp_bytes;
    let first_cp = read_u32(&plc_pcd, 0).unwrap_or(u32::MAX);
    if first_cp != 0 {
        return Err(Error::InvalidInput(
            "legacy Word piece table does not start at CP zero".into(),
        ));
    }
    let mut text = String::new();
    let mut previous_cp = 0usize;
    for index in 0..piece_count {
        let next_cp = read_u32(&plc_pcd, (index + 1) * 4).unwrap_or(u32::MAX) as usize;
        if next_cp <= previous_cp || next_cp > MAX_WORD_MAIN_CPS.saturating_mul(8) {
            return Err(Error::InvalidInput(
                "legacy Word piece table has non-increasing or excessive CP values".into(),
            ));
        }
        let pcd_offset = pcd_base + index * 8;
        let fc_raw = read_u32(&plc_pcd, pcd_offset + 2)
            .ok_or_else(|| Error::InvalidInput("legacy Word PCD is truncated".into()))?;
        let compressed = fc_raw & 0x4000_0000 != 0;
        let fc = (fc_raw & 0x3FFF_FFFF) as usize;
        let story_end = next_cp.min(ccp_text);
        if story_end > previous_cp {
            let cp_count = story_end - previous_cp;
            let (byte_offset, byte_count) = if compressed {
                (fc / 2, cp_count)
            } else {
                (
                    fc,
                    cp_count.checked_mul(2).ok_or_else(|| {
                        Error::LimitExceeded("legacy Word UTF-16 piece size overflowed".into())
                    })?,
                )
            };
            let byte_end = byte_offset.checked_add(byte_count).ok_or_else(|| {
                Error::LimitExceeded("legacy Word piece offset overflowed".into())
            })?;
            if byte_end > word_document.len() || byte_end > cb_mac {
                return Err(Error::InvalidInput(
                    "legacy Word piece extends beyond WordDocument data".into(),
                ));
            }
            let piece = &word_document[byte_offset..byte_end];
            let decoded = if compressed {
                decode_compressed(piece, encoding, warnings)?
            } else {
                decode_utf16le(piece, warnings)?
            };
            if text.len().saturating_add(decoded.len()) > MAX_WORD_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "legacy Word extracted text exceeds {MAX_WORD_TEXT_BYTES} bytes"
                )));
            }
            text.push_str(&decoded);
        }
        previous_cp = next_cp;
        if previous_cp >= ccp_text {
            break;
        }
    }
    if previous_cp < ccp_text {
        return Err(Error::InvalidInput(
            "legacy Word piece table does not cover its main-story CP range".into(),
        ));
    }
    Ok(text)
}

fn parse_clx(clx: &[u8]) -> Result<Vec<u8>> {
    let mut offset = 0usize;
    let mut property_records = 0usize;
    while offset < clx.len() {
        match clx[offset] {
            0x01 => {
                property_records += 1;
                if property_records > MAX_WORD_CLX_PROPERTY_RECORDS {
                    return Err(Error::LimitExceeded(format!(
                        "legacy Word CLX has more than {MAX_WORD_CLX_PROPERTY_RECORDS} property records"
                    )));
                }
                let size = read_u16(clx, offset + 1)
                    .ok_or_else(|| Error::InvalidInput("legacy Word CLX Prc is truncated".into()))?
                    as usize;
                if size > 0x3FA2 {
                    return Err(Error::LimitExceeded(
                        "legacy Word CLX property record exceeds its specification limit".into(),
                    ));
                }
                offset = offset.checked_add(3 + size).ok_or_else(|| {
                    Error::LimitExceeded("legacy Word CLX offset overflowed".into())
                })?;
                if offset > clx.len() {
                    return Err(Error::InvalidInput(
                        "legacy Word CLX property record exceeds its table".into(),
                    ));
                }
            }
            0x02 => {
                let size = read_u32(clx, offset + 1).ok_or_else(|| {
                    Error::InvalidInput("legacy Word CLX Pcdt size is truncated".into())
                })? as usize;
                let start = offset.checked_add(5).ok_or_else(|| {
                    Error::LimitExceeded("legacy Word CLX offset overflowed".into())
                })?;
                let end = start.checked_add(size).ok_or_else(|| {
                    Error::LimitExceeded("legacy Word CLX size overflowed".into())
                })?;
                if end != clx.len() || size < 4 || !(size - 4).is_multiple_of(12) {
                    return Err(Error::InvalidInput(
                        "legacy Word CLX Pcdt has an invalid PlcPcd size".into(),
                    ));
                }
                return Ok(clx[start..end].to_vec());
            }
            marker => {
                return Err(Error::InvalidInput(format!(
                    "legacy Word CLX has unexpected marker 0x{marker:02X}"
                )));
            }
        }
    }
    Err(Error::InvalidInput(
        "legacy Word CLX does not contain a Pcdt".into(),
    ))
}

fn decode_compressed(
    bytes: &[u8],
    encoding: Option<&'static Encoding>,
    warnings: &mut Vec<String>,
) -> Result<String> {
    let encoding = encoding.unwrap_or_else(|| {
        push_warning_once(
            warnings,
            "legacy Word compressed text has no reliable code page in its FIB language ID; Windows-1252 fallback is used",
        );
        WINDOWS_1252
    });
    let (text, had_errors) = encoding.decode_without_bom_handling(bytes);
    if had_errors {
        push_warning_once(
            warnings,
            "legacy Word compressed characters contain bytes invalid for the inferred code page; replacement characters are used",
        );
    }
    if text.len() > MAX_WORD_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "legacy Word text exceeds {MAX_WORD_TEXT_BYTES} bytes"
        )));
    }
    Ok(text.into_owned())
}

fn decode_utf16le(bytes: &[u8], warnings: &mut Vec<String>) -> Result<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::InvalidInput(
            "legacy Word UTF-16 piece has an odd byte count".into(),
        ));
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
    let mut text = String::new();
    for decoded in char::decode_utf16(units) {
        match decoded {
            Ok(character) => text.push(character),
            Err(_) => {
                text.push(char::REPLACEMENT_CHARACTER);
                push_warning_once(
                    warnings,
                    "legacy Word text contains an invalid UTF-16 surrogate; a replacement character is used",
                );
            }
        }
        if text.len() > MAX_WORD_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "legacy Word text exceeds {MAX_WORD_TEXT_BYTES} bytes"
            )));
        }
    }
    Ok(text)
}

fn text_to_blocks(text: &str, warnings: &mut Vec<String>) -> Result<Vec<HtmlBlock>> {
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let mut field_stack = Vec::<bool>::new();
    let mut field_instruction_depth = 0usize;
    let mut omitted_controls = false;
    for character in text.chars() {
        match character {
            '\u{13}' => {
                if field_stack.len() >= MAX_WORD_FIELD_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "legacy Word field nesting exceeds {MAX_WORD_FIELD_DEPTH}"
                    )));
                }
                field_stack.push(true);
                field_instruction_depth += 1;
            }
            '\u{14}' => {
                if let Some(instruction) = field_stack.last_mut()
                    && *instruction
                {
                    *instruction = false;
                    field_instruction_depth -= 1;
                }
            }
            '\u{15}' => {
                if field_stack.pop() == Some(true) {
                    field_instruction_depth -= 1;
                }
            }
            _ if field_instruction_depth != 0 => continue,
            '\r' => push_paragraph(&mut blocks, &mut paragraph)?,
            '\u{0C}' => {
                push_paragraph(&mut blocks, &mut paragraph)?;
                blocks.push(HtmlBlock::PageBreak);
            }
            '\u{0B}' => push_paragraph(&mut blocks, &mut paragraph)?,
            '\u{07}' => paragraph.push_str("  |  "),
            '\u{01}' | '\u{08}' => {
                paragraph.push_str("[embedded object]");
                omitted_controls = true;
            }
            '\t' => paragraph.push_str("    "),
            character if character.is_control() => omitted_controls = true,
            character => paragraph.push(character),
        }
        if paragraph.len() > MAX_WORD_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "legacy Word paragraph exceeds {MAX_WORD_TEXT_BYTES} bytes"
            )));
        }
        if blocks.len() > MAX_WORD_BLOCKS {
            return Err(Error::LimitExceeded(format!(
                "legacy Word document exceeds {MAX_WORD_BLOCKS} text blocks"
            )));
        }
    }
    if !paragraph.is_empty() {
        push_paragraph(&mut blocks, &mut paragraph)?;
    }
    if omitted_controls {
        push_warning_once(
            warnings,
            "legacy Word object anchors or nonprinting controls were omitted or shown as placeholders",
        );
    }
    if blocks.len() > MAX_WORD_BLOCKS {
        return Err(Error::LimitExceeded(format!(
            "legacy Word document exceeds {MAX_WORD_BLOCKS} text blocks"
        )));
    }
    Ok(blocks)
}

fn push_paragraph(blocks: &mut Vec<HtmlBlock>, paragraph: &mut String) -> Result<()> {
    if blocks.len() >= MAX_WORD_BLOCKS {
        return Err(Error::LimitExceeded(format!(
            "legacy Word document exceeds {MAX_WORD_BLOCKS} text blocks"
        )));
    }
    blocks.push(HtmlBlock::Paragraph {
        text: std::mem::take(paragraph),
    });
    Ok(())
}

fn validate_compound_entries<R: Read + Seek>(
    compound: &CompoundFile<R>,
    file_bytes: usize,
) -> Result<()> {
    let mut entries = 0usize;
    let mut total_path_bytes = 0usize;
    for entry in compound.walk() {
        entries += 1;
        if entries > MAX_WORD_CFB_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "legacy Word compound file contains more than {MAX_WORD_CFB_ENTRIES} entries"
            )));
        }
        let path_bytes = entry.path().to_string_lossy().len();
        if path_bytes > MAX_WORD_PATH_BYTES {
            return Err(Error::LimitExceeded(format!(
                "legacy Word compound path exceeds {MAX_WORD_PATH_BYTES} bytes"
            )));
        }
        total_path_bytes = total_path_bytes.saturating_add(path_bytes);
        if total_path_bytes > MAX_WORD_PATHS_TOTAL_BYTES {
            return Err(Error::LimitExceeded(format!(
                "legacy Word compound paths exceed {MAX_WORD_PATHS_TOTAL_BYTES} bytes"
            )));
        }
        if entry.is_stream() && entry.len() > file_bytes as u64 {
            return Err(Error::InvalidInput(
                "legacy Word stream declares more bytes than its container".into(),
            ));
        }
    }
    Ok(())
}

fn read_stream<R: Read + Seek>(
    compound: &mut CompoundFile<R>,
    name: &str,
    maximum: u64,
) -> Result<Vec<u8>> {
    let mut stream = compound.open_stream(name).map_err(|error| {
        Error::InvalidInput(format!("cannot open legacy Word {name} stream: {error}"))
    })?;
    let length = stream.len();
    if length == 0 || length > maximum {
        return Err(Error::LimitExceeded(format!(
            "legacy Word {name} stream has {length} bytes; allowed range is 1..={maximum}"
        )));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(length)
            .unwrap_or(usize::MAX)
            .min(8 * 1024 * 1024),
    );
    stream.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != length {
        return Err(Error::InvalidInput(format!(
            "legacy Word {name} stream ended before its declared size"
        )));
    }
    Ok(bytes)
}

fn read_stream_range(
    compound: &mut CompoundFile<Cursor<&[u8]>>,
    name: &str,
    offset: usize,
    length: usize,
    maximum: usize,
) -> Result<Vec<u8>> {
    if length == 0 || length > maximum {
        return Err(Error::LimitExceeded(format!(
            "legacy Word {name} table range has {length} bytes; maximum is {maximum}"
        )));
    }
    let mut stream = compound.open_stream(name).map_err(|error| {
        Error::InvalidInput(format!("cannot open legacy Word {name} stream: {error}"))
    })?;
    let end = offset
        .checked_add(length)
        .ok_or_else(|| Error::LimitExceeded("legacy Word table range overflowed".into()))?;
    if end as u64 > stream.len() {
        return Err(Error::InvalidInput(format!(
            "legacy Word table range {offset}..{end} exceeds {name} stream"
        )));
    }
    stream.seek(SeekFrom::Start(offset as u64))?;
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn encoding_for_lid(lid: u16, far_east: bool) -> Option<&'static Encoding> {
    // MS-DOC normalizes some East Asian install language IDs to English on
    // newer Word versions. In that case the compressed-text code page cannot
    // be determined from the FIB alone.
    if far_east && lid & 0x03FF == 0x0009 {
        return None;
    }
    let codepage = match lid {
        0x0404 | 0x0C04 | 0x1404 => 950,
        0x0804 | 0x1004 => 936,
        _ => match lid & 0x03FF {
            0x01 => 1256,
            0x04 => 936,
            0x05 | 0x0E | 0x15 | 0x18 | 0x1A | 0x2E => 1250,
            0x08 => 1253,
            0x0D => 1255,
            0x11 => 932,
            0x12 => 949,
            0x19 => 1251,
            0x1E => 874,
            0x1F => 1254,
            0x2A => 1258,
            _ => 1252,
        },
    };
    let label: &'static [u8] = match codepage {
        874 => b"windows-874",
        932 => b"shift_jis",
        936 => b"gbk",
        949 => b"euc-kr",
        950 => b"big5",
        1250..=1258 => match codepage {
            1250 => b"windows-1250",
            1251 => b"windows-1251",
            1252 => b"windows-1252",
            1253 => b"windows-1253",
            1254 => b"windows-1254",
            1255 => b"windows-1255",
            1256 => b"windows-1256",
            1257 => b"windows-1257",
            _ => b"windows-1258",
        },
        _ => return None,
    };
    Encoding::for_label(label)
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn read_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_clx(pieces: &[(u32, u32, usize, bool)]) -> Vec<u8> {
        let plc_size = (pieces.len() + 1) * 4 + pieces.len() * 8;
        let mut plc = Vec::with_capacity(plc_size);
        if let Some((first_cp, _, _, _)) = pieces.first() {
            plc.extend_from_slice(&first_cp.to_le_bytes());
        }
        for (_, end_cp, _, _) in pieces {
            plc.extend_from_slice(&end_cp.to_le_bytes());
        }
        for (_, _, offset, compressed) in pieces {
            plc.extend_from_slice(&[0, 0]);
            let fc = if *compressed {
                0x4000_0000u32 | (*offset as u32 * 2)
            } else {
                *offset as u32
            };
            plc.extend_from_slice(&fc.to_le_bytes());
            plc.extend_from_slice(&[0, 0]);
        }
        let mut clx = vec![0x02];
        clx.extend_from_slice(&(u32::try_from(plc.len()).unwrap()).to_le_bytes());
        clx.extend_from_slice(&plc);
        clx
    }

    #[test]
    fn decodes_compressed_and_utf16_piece_table_runs() {
        let mut word_document = vec![0u8; 256];
        word_document[200..209].copy_from_slice(b"Old text ");
        for (index, unit) in "binary".encode_utf16().enumerate() {
            word_document[220 + index * 2..222 + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        let clx = synthetic_clx(&[(0, 9, 200, true), (9, 15, 220, false)]);
        let text = extract_piece_table_text(
            &word_document,
            word_document.len(),
            15,
            &clx,
            Some(WINDOWS_1252),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(text, "Old text binary");
    }

    #[test]
    fn decodes_japanese_compressed_text_and_flags_ambiguous_far_east_ids() {
        let encoding = encoding_for_lid(0x0411, true).unwrap();
        assert_eq!(
            decode_compressed(&[0x82, 0xA0], Some(encoding), &mut Vec::new()).unwrap(),
            "あ"
        );
        assert!(encoding_for_lid(0x0409, true).is_none());
    }

    #[test]
    fn shows_stored_field_results_without_rendering_field_instructions() {
        let mut warnings = Vec::new();
        let blocks =
            text_to_blocks("Before \u{13} PAGE \u{14} 2 \u{15} after\r", &mut warnings).unwrap();
        let HtmlBlock::Paragraph { text } = &blocks[0] else {
            panic!("expected a paragraph block");
        };
        assert!(text.contains('2'));
        assert!(text.contains("Before") && text.contains("after"));
        assert!(!text.contains("PAGE"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn bounds_legacy_word_field_nesting() {
        let fields = "\u{13}".repeat(MAX_WORD_FIELD_DEPTH + 1);
        assert!(matches!(
            text_to_blocks(&fields, &mut Vec::new()),
            Err(Error::LimitExceeded(message)) if message.contains("field nesting")
        ));
    }

    #[test]
    fn rejects_piece_offsets_outside_word_document_before_slicing() {
        let word_document = vec![0u8; 64];
        let clx = synthetic_clx(&[(0, 1, 1024, true)]);
        assert!(matches!(
            extract_piece_table_text(
                &word_document,
                word_document.len(),
                1,
                &clx,
                Some(WINDOWS_1252),
                &mut Vec::new(),
            ),
            Err(Error::InvalidInput(message)) if message.contains("extends beyond")
        ));
    }

    #[test]
    fn recognizes_an_encryption_flag_in_the_fib() {
        let fib_base_len = 32;
        let csw = 14usize;
        let fib_rg_lw_count = 22usize;
        let fc_lcb_count = 93usize;
        let mut word_document =
            vec![0u8; fib_base_len + 2 + csw * 2 + 2 + fib_rg_lw_count * 4 + 2 + fc_lcb_count * 8];
        word_document[0..2].copy_from_slice(&WORD_MAGIC.to_le_bytes());
        word_document[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
        word_document[6..8].copy_from_slice(&0x0409u16.to_le_bytes());
        word_document[10..12].copy_from_slice(&0x4101u16.to_le_bytes());
        word_document[32..34].copy_from_slice(&(csw as u16).to_le_bytes());
        let cslw_offset = 34 + csw * 2;
        word_document[cslw_offset..cslw_offset + 2]
            .copy_from_slice(&(fib_rg_lw_count as u16).to_le_bytes());
        let cb_rg_fc_lcb_offset = cslw_offset + 2 + fib_rg_lw_count * 4;
        word_document[cb_rg_fc_lcb_offset..cb_rg_fc_lcb_offset + 2]
            .copy_from_slice(&(fc_lcb_count as u16).to_le_bytes());
        let fib = parse_fib(&word_document).unwrap();
        assert!(fib.encrypted);
        assert!(fib.is_template);
        assert!(fib.far_east);
    }
}
