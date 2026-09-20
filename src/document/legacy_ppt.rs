//! Bounded text previews for legacy PowerPoint Binary presentations.
//!
//! This parser follows the live-edit/persist-directory chain before looking
//! for slides. It extracts text from slide-local OfficeArt textboxes and the
//! outline text cache; it does not reproduce the slide drawing.

use std::fs::{self, File};
use std::io::{Cursor, Read, Seek};
use std::path::Path;

use cfb::CompoundFile;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::ir::Page;

const CFB_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const RT_CURRENT_USER: u16 = 0x0FF6;
const RT_USER_EDIT: u16 = 0x0FF5;
const RT_PERSIST_DIRECTORY: u16 = 0x1772;
const RT_DOCUMENT: u16 = 0x03E8;
const RT_SLIDE_LIST_WITH_TEXT: u16 = 0x0FF0;
const RT_SLIDE_PERSIST: u16 = 0x03F3;
const RT_SLIDE: u16 = 0x03EE;
const RT_CLIENT_TEXTBOX: u16 = 0xF00D;
const RT_OUTLINE_TEXT_REF: u16 = 0x0F9E;
const RT_TEXT_HEADER: u16 = 0x0F9F;
const RT_TEXT_CHARS: u16 = 0x0FA0;
const RT_TEXT_BYTES: u16 = 0x0FA8;
const MAX_PPT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PPT_STREAM_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PPT_CFB_ENTRIES: usize = 50_000;
const MAX_PPT_PATH_BYTES: usize = 1024;
const MAX_PPT_PATHS_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_PPT_EDITS: usize = 4096;
const MAX_PPT_PERSIST_OBJECTS: usize = 500_000;
const MAX_PPT_RECORDS: usize = 1_000_000;
const MAX_PPT_RECORD_DEPTH: usize = 64;
const MAX_PPT_SLIDES: usize = 10_000;
const MAX_PPT_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PPT_TEXT_BLOCKS: usize = 500_000;

#[derive(Clone, Copy, Debug)]
struct Record {
    offset: usize,
    payload_start: usize,
    payload_end: usize,
    record_type: u16,
    instance: u16,
    version: u16,
}

#[derive(Clone, Copy, Debug)]
struct UserEdit {
    previous: usize,
    persist_directory: usize,
    document_persist_id: u32,
    encryption_persist_id: Option<u32>,
}

#[derive(Clone, Debug)]
struct Slide {
    persist_id: u32,
    slide_id: u32,
    outline_text: Vec<String>,
}

struct LegacyPptPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for LegacyPptPageSink<'_> {
    fn consume(&mut self, mut page: Page) -> Result<()> {
        page.source_format = "ppt".into();
        page.title = "Legacy PowerPoint text preview".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

/// Distinguish a legacy PowerPoint CFB file from Word, Outlook, and other OLE files.
pub(crate) fn looks_like_legacy_ppt(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if metadata.len() < 512 || metadata.len() > MAX_PPT_BYTES {
        return false;
    }
    let Ok(file) = File::open(path) else {
        return false;
    };
    let Ok(compound) = CompoundFile::open(file) else {
        return false;
    };
    compound.is_stream("Current User") && compound.is_stream("PowerPoint Document")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PPT_BYTES),
        "legacy PowerPoint presentation",
    )?;
    if bytes.len() < 512 || bytes[..8] != CFB_SIGNATURE {
        return Err(Error::InvalidInput(
            "legacy PowerPoint input is not a Compound File Binary document".into(),
        ));
    }
    let mut compound = CompoundFile::open(Cursor::new(bytes.as_slice())).map_err(|error| {
        Error::InvalidInput(format!(
            "legacy PowerPoint compound file is invalid: {error}"
        ))
    })?;
    validate_compound_entries(&compound, bytes.len())?;
    if !compound.is_stream("Current User") || !compound.is_stream("PowerPoint Document") {
        return Err(Error::InvalidInput(
            "legacy PowerPoint file lacks a Current User or PowerPoint Document stream".into(),
        ));
    }
    let current_user = read_stream(&mut compound, "Current User")?;
    let presentation = read_stream(&mut compound, "PowerPoint Document")?;
    let mut warnings = vec![
        "Legacy PowerPoint conversion extracts slide text onto A4 pages; original slide geometry, styling, pagination, and visual layout are not reconstructed".to_owned(),
        "Pictures, backgrounds, charts, tables, shapes without text, notes, audio/video, and embedded objects are omitted; macros and external content are never opened or executed".to_owned(),
    ];
    let (document, slides) = read_live_slides(&current_user, &presentation, &mut warnings)?;
    if slides.is_empty() {
        return Err(Error::InvalidInput(
            "legacy PowerPoint presentation contains no slides".into(),
        ));
    }

    let mut blocks = Vec::new();
    let mut total_text_bytes = 0usize;
    let mut record_count = 0usize;
    for (index, slide) in slides.iter().enumerate() {
        let offset = *document.get(&slide.persist_id).ok_or_else(|| {
            Error::InvalidInput("legacy PowerPoint slide persist reference is missing".into())
        })?;
        let slide_record = parse_record(&presentation, offset, presentation.len())?;
        if slide_record.record_type != RT_SLIDE || slide_record.version != 0x000F {
            return Err(Error::InvalidInput(format!(
                "legacy PowerPoint slide {} persist reference does not identify a SlideContainer",
                index + 1
            )));
        }
        let mut slide_text = Vec::new();
        let descendants = collect_descendants(&presentation, slide_record, &mut record_count)?;
        for record in descendants
            .into_iter()
            .filter(|record| record.record_type == RT_CLIENT_TEXTBOX)
        {
            if let Some(text) =
                read_client_textbox(&presentation, record, &slide.outline_text, &mut warnings)?
                && !text.trim().is_empty()
            {
                total_text_bytes = total_text_bytes.saturating_add(text.len());
                if total_text_bytes > MAX_PPT_TEXT_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "legacy PowerPoint extracted text exceeds {MAX_PPT_TEXT_BYTES} bytes"
                    )));
                }
                slide_text.push(text);
            }
        }
        if slide_text.is_empty() {
            // Some producers leave only the slide-local outline cache. Retain that text
            // if the slide has no OfficeArtClientTextbox records with local characters.
            for text in &slide.outline_text {
                if !text.trim().is_empty() {
                    total_text_bytes = total_text_bytes.saturating_add(text.len());
                    if total_text_bytes > MAX_PPT_TEXT_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "legacy PowerPoint extracted text exceeds {MAX_PPT_TEXT_BYTES} bytes"
                        )));
                    }
                    slide_text.push(text.clone());
                }
            }
        }
        if index > 0 {
            blocks.push(HtmlBlock::PageBreak);
        }
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: format!("Slide {}", index + 1),
        });
        for text in slide_text {
            append_text_blocks(&text, &mut blocks)?;
        }
        if blocks.len() > MAX_PPT_TEXT_BLOCKS {
            return Err(Error::LimitExceeded(format!(
                "legacy PowerPoint exceeds {MAX_PPT_TEXT_BLOCKS} text blocks"
            )));
        }
    }
    if total_text_bytes == 0 {
        return Err(Error::Unsupported(
            "legacy PowerPoint contains no extractable slide text; binary drawing and image rendering is not supported".into(),
        ));
    }
    let mut page_sink = LegacyPptPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn read_live_slides(
    current_user: &[u8],
    presentation: &[u8],
    warnings: &mut Vec<String>,
) -> Result<(std::collections::HashMap<u32, usize>, Vec<Slide>)> {
    let user_record = parse_record(current_user, 0, current_user.len())?;
    if user_record.record_type != RT_CURRENT_USER
        || user_record.version != 0
        || user_record.instance != 0
        || user_record.payload_end - user_record.payload_start < 20
    {
        return Err(Error::InvalidInput(
            "legacy PowerPoint Current User stream has no valid CurrentUserAtom".into(),
        ));
    }
    let current_user_payload = user_record.payload_start;
    if read_u32(current_user, current_user_payload) != Some(20) {
        return Err(Error::InvalidInput(
            "legacy PowerPoint CurrentUserAtom has an invalid fixed-field size".into(),
        ));
    }
    match read_u32(current_user, current_user_payload + 4) {
        Some(0xF3D1_C4DF) => {
            return Err(Error::Unsupported(
                "encrypted legacy PowerPoint presentations are rejected; access controls are not bypassed".into(),
            ));
        }
        Some(0xE391_C05F) => {}
        _ => {
            return Err(Error::InvalidInput(
                "legacy PowerPoint CurrentUserAtom has an invalid encryption token".into(),
            ));
        }
    }
    let username_len = read_u16(current_user, current_user_payload + 12).unwrap() as usize;
    if username_len > 255
        || current_user_payload
            .checked_add(20 + username_len)
            .is_none_or(|end| end > user_record.payload_end)
    {
        return Err(Error::InvalidInput(
            "legacy PowerPoint CurrentUserAtom has a truncated or overlong username".into(),
        ));
    }
    let current_edit = read_u32(current_user, current_user_payload + 8).ok_or_else(|| {
        Error::InvalidInput("legacy PowerPoint CurrentUserAtom is truncated".into())
    })? as usize;
    let mut edits = Vec::new();
    let mut edit_offsets = std::collections::HashSet::new();
    let mut offset = current_edit;
    for _ in 0..MAX_PPT_EDITS {
        if offset == 0 || !edit_offsets.insert(offset) {
            return Err(Error::InvalidInput(
                "legacy PowerPoint user-edit chain contains a cycle or zero edit".into(),
            ));
        }
        let record = parse_record(presentation, offset, presentation.len())?;
        if record.record_type != RT_USER_EDIT
            || record.version != 0
            || record.instance != 0
            || !matches!(record.payload_end - record.payload_start, 28 | 32)
        {
            return Err(Error::InvalidInput(
                "legacy PowerPoint user-edit chain references an invalid UserEditAtom".into(),
            ));
        }
        let payload = record.payload_start;
        let previous = read_u32(presentation, payload + 8).unwrap() as usize;
        let persist_directory = read_u32(presentation, payload + 12).unwrap() as usize;
        let document_persist_id = read_u32(presentation, payload + 16).unwrap();
        let encryption_persist_id = if record.payload_end - payload >= 32 {
            Some(read_u32(presentation, payload + 28).unwrap())
        } else {
            None
        };
        if document_persist_id != 1
            || persist_directory <= previous
            || persist_directory >= record.offset
            || (previous > 0 && previous >= record.offset)
        {
            return Err(Error::InvalidInput(
                "legacy PowerPoint UserEditAtom offsets or document reference are invalid".into(),
            ));
        }
        edits.push(UserEdit {
            previous,
            persist_directory,
            document_persist_id,
            encryption_persist_id,
        });
        if previous == 0 {
            break;
        }
        offset = previous;
    }
    if edits.len() == MAX_PPT_EDITS || edits.last().is_none_or(|edit| edit.previous != 0) {
        return Err(Error::LimitExceeded(format!(
            "legacy PowerPoint edit chain exceeds {MAX_PPT_EDITS} edits"
        )));
    }
    if edits[0]
        .encryption_persist_id
        .is_some_and(|persist_id| persist_id != 0)
    {
        return Err(Error::Unsupported("encrypted legacy PowerPoint presentations are rejected; access controls are not bypassed".into()));
    }
    let latest_document_persist_id = edits[0].document_persist_id;
    let mut persist_directory = std::collections::HashMap::new();
    let mut record_count = 0usize;
    for edit in edits.iter().rev() {
        let directory = parse_record(presentation, edit.persist_directory, presentation.len())?;
        if directory.record_type != RT_PERSIST_DIRECTORY
            || directory.version != 0
            || directory.instance != 0
        {
            return Err(Error::InvalidInput(
                "legacy PowerPoint UserEditAtom does not point to a PersistDirectoryAtom".into(),
            ));
        }
        let mut cursor = directory.payload_start;
        let mut ids_in_directory = std::collections::HashSet::new();
        while cursor < directory.payload_end {
            record_count += 1;
            if record_count > MAX_PPT_RECORDS {
                return Err(Error::LimitExceeded(format!(
                    "legacy PowerPoint record count exceeds {MAX_PPT_RECORDS}"
                )));
            }
            if cursor + 4 > directory.payload_end {
                return Err(Error::InvalidInput(
                    "legacy PowerPoint persist directory has a truncated entry".into(),
                ));
            }
            let packed = read_u32(presentation, cursor).unwrap();
            cursor += 4;
            let first_id = packed & 0x000F_FFFF;
            let count = (packed >> 20) as usize;
            if count == 0 || first_id == 0 || first_id.saturating_add(count as u32) > 0x000F_FFFF {
                return Err(Error::InvalidInput(
                    "legacy PowerPoint persist directory entry has an invalid ID range".into(),
                ));
            }
            if persist_directory.len().saturating_add(count) > MAX_PPT_PERSIST_OBJECTS {
                return Err(Error::LimitExceeded(format!(
                    "legacy PowerPoint persist directory exceeds {MAX_PPT_PERSIST_OBJECTS} objects"
                )));
            }
            let entry_bytes = count.checked_mul(4).ok_or_else(|| {
                Error::LimitExceeded("legacy PowerPoint persist offset count overflow".into())
            })?;
            if cursor
                .checked_add(entry_bytes)
                .is_none_or(|end| end > directory.payload_end)
            {
                return Err(Error::InvalidInput(
                    "legacy PowerPoint persist directory offsets are truncated".into(),
                ));
            }
            for index in 0..count {
                let id = first_id + index as u32;
                let object_offset = read_u32(presentation, cursor).unwrap() as usize;
                cursor += 4;
                if !ids_in_directory.insert(id) {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint persist directory repeats a persist object ID".into(),
                    ));
                }
                if object_offset < edit.previous || object_offset >= directory.offset {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint persist offset is outside its user-edit range".into(),
                    ));
                }
                persist_directory.insert(id, object_offset);
            }
        }
    }
    let document_offset = *persist_directory
        .get(&latest_document_persist_id)
        .ok_or_else(|| {
            Error::InvalidInput("legacy PowerPoint document persist reference is missing".into())
        })?;
    let document = parse_record(presentation, document_offset, presentation.len())?;
    if document.record_type != RT_DOCUMENT || document.version != 0x000F || document.instance != 0 {
        return Err(Error::InvalidInput(
            "legacy PowerPoint document persist reference does not identify a DocumentContainer"
                .into(),
        ));
    }
    let document_children = direct_children(presentation, document, &mut record_count)?;
    let mut slide_list = None;
    for child in document_children {
        if child.record_type == RT_SLIDE_LIST_WITH_TEXT
            && child.instance == 0
            && slide_list.replace(child).is_some()
        {
            return Err(Error::InvalidInput(
                "legacy PowerPoint DocumentContainer has duplicate presentation slide lists".into(),
            ));
        }
    }
    let slide_list = slide_list.ok_or_else(|| {
        Error::InvalidInput("legacy PowerPoint document has no presentation slide list".into())
    })?;
    let children = direct_children(presentation, slide_list, &mut record_count)?;
    let mut slides = Vec::new();
    let mut current_slide: Option<Slide> = None;
    let mut pending_text: Option<String> = None;
    let mut total_outline_text_bytes = 0usize;
    for child in children {
        match child.record_type {
            RT_SLIDE_PERSIST => {
                if let Some(text) = pending_text.take() {
                    push_outline_text(
                        current_slide.as_mut().unwrap(),
                        text,
                        &mut total_outline_text_bytes,
                    )?;
                }
                if let Some(slide) = current_slide.take() {
                    slides.push(slide);
                }
                if child.version != 0
                    || child.instance != 0
                    || child.payload_end - child.payload_start != 20
                {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint SlidePersistAtom has an invalid length".into(),
                    ));
                }
                let payload = child.payload_start;
                let persist_id = read_u32(presentation, payload).unwrap();
                let text_count = read_u32(presentation, payload + 8).unwrap() as i32;
                let slide_id = read_u32(presentation, payload + 12).unwrap();
                if persist_id == 0
                    || !(0x100..=0x7fff_ffff).contains(&slide_id)
                    || !(0..=8).contains(&text_count)
                {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint SlidePersistAtom has invalid identifiers or text count"
                            .into(),
                    ));
                }
                if slides.len() >= MAX_PPT_SLIDES {
                    return Err(Error::LimitExceeded(format!(
                        "legacy PowerPoint exceeds {MAX_PPT_SLIDES} slides"
                    )));
                }
                current_slide = Some(Slide {
                    persist_id,
                    slide_id,
                    outline_text: Vec::new(),
                });
                pending_text = None;
            }
            RT_TEXT_HEADER => {
                if let Some(text) = pending_text.take() {
                    push_outline_text(
                        current_slide.as_mut().unwrap(),
                        text,
                        &mut total_outline_text_bytes,
                    )?;
                }
                if child.version != 0
                    || child.payload_end - child.payload_start != 4
                    || current_slide.is_none()
                {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint slide outline has an invalid TextHeaderAtom".into(),
                    ));
                }
                pending_text = Some(String::new());
            }
            RT_TEXT_CHARS | RT_TEXT_BYTES if current_slide.is_some() && pending_text.is_some() => {
                let decoded = decode_text_atom(presentation, child, warnings)?;
                pending_text = Some(decoded);
            }
            _ => {
                if let Some(text) = pending_text.take() {
                    push_outline_text(
                        current_slide.as_mut().unwrap(),
                        text,
                        &mut total_outline_text_bytes,
                    )?;
                }
            }
        }
    }
    if let Some(text) = pending_text.take() {
        push_outline_text(
            current_slide.as_mut().unwrap(),
            text,
            &mut total_outline_text_bytes,
        )?;
    }
    if let Some(slide) = current_slide {
        slides.push(slide);
    }
    if slides.len() > MAX_PPT_SLIDES {
        return Err(Error::LimitExceeded(format!(
            "legacy PowerPoint exceeds {MAX_PPT_SLIDES} slides"
        )));
    }
    let mut slide_ids = std::collections::HashSet::new();
    let mut slide_persist_ids = std::collections::HashSet::new();
    for slide in &slides {
        if !slide_ids.insert(slide.slide_id)
            || !slide_persist_ids.insert(slide.persist_id)
            || !persist_directory.contains_key(&slide.persist_id)
        {
            return Err(Error::InvalidInput(
                "legacy PowerPoint slide IDs are duplicated or persist references are missing"
                    .into(),
            ));
        }
    }
    Ok((persist_directory, slides))
}

fn read_client_textbox(
    bytes: &[u8],
    record: Record,
    outline_text: &[String],
    warnings: &mut Vec<String>,
) -> Result<Option<String>> {
    if record.version != 0x000F {
        return Err(Error::InvalidInput(
            "legacy PowerPoint OfficeArtClientTextbox is not a container".into(),
        ));
    }
    let mut cursor = record.payload_start;
    let mut direct_text = None;
    let mut outline_index = None;
    let mut saw_text_header = false;
    let mut text_atom_count = 0usize;
    while cursor < record.payload_end {
        let child = parse_record(bytes, cursor, record.payload_end)?;
        match child.record_type {
            RT_OUTLINE_TEXT_REF => {
                if child.payload_end - child.payload_start != 4 {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint OutlineTextRefAtom has an invalid length".into(),
                    ));
                }
                let index = read_u32(bytes, child.payload_start).unwrap() as usize;
                if outline_index.replace(index).is_some() {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint textbox has duplicate outline references".into(),
                    ));
                }
            }
            RT_TEXT_HEADER => {
                if child.payload_end - child.payload_start != 4 || child.instance != 0 {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint textbox TextHeaderAtom is invalid".into(),
                    ));
                }
                saw_text_header = true;
            }
            RT_TEXT_CHARS | RT_TEXT_BYTES if saw_text_header => {
                text_atom_count += 1;
                if text_atom_count > 1 {
                    return Err(Error::Unsupported(
                        "legacy PowerPoint textbox contains multiple text-encoding atoms".into(),
                    ));
                }
                direct_text = Some(decode_text_atom(bytes, child, warnings)?);
            }
            _ => {}
        }
        cursor = child.payload_end;
    }
    if let Some(text) = direct_text {
        return Ok(Some(text));
    }
    if let Some(index) = outline_index {
        return outline_text.get(index).cloned().map(Some).ok_or_else(|| {
            Error::InvalidInput("legacy PowerPoint textbox outline index is out of range".into())
        });
    }
    Ok(None)
}

fn decode_text_atom(bytes: &[u8], record: Record, warnings: &mut Vec<String>) -> Result<String> {
    let text = match record.record_type {
        RT_TEXT_CHARS => {
            if !(record.payload_end - record.payload_start).is_multiple_of(2) {
                return Err(Error::InvalidInput(
                    "legacy PowerPoint UTF-16 text has an odd byte count".into(),
                ));
            }
            let units = bytes[record.payload_start..record.payload_end]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
            let mut text = String::new();
            for item in char::decode_utf16(units) {
                match item {
                    Ok('\0') => {
                        return Err(Error::InvalidInput(
                            "legacy PowerPoint text atom contains a NUL character".into(),
                        ));
                    }
                    Ok(character) => text.push(character),
                    Err(_) => {
                        text.push(char::REPLACEMENT_CHARACTER);
                        push_warning_once(
                            warnings,
                            "legacy PowerPoint contains an invalid UTF-16 surrogate; a replacement character is used",
                        );
                    }
                }
                if text.len() > MAX_PPT_TEXT_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "legacy PowerPoint text atom exceeds {MAX_PPT_TEXT_BYTES} bytes"
                    )));
                }
            }
            text
        }
        RT_TEXT_BYTES => {
            if record.payload_end - record.payload_start > MAX_PPT_TEXT_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "legacy PowerPoint byte text atom exceeds {MAX_PPT_TEXT_BYTES} bytes"
                )));
            }
            let mut text = String::with_capacity(record.payload_end - record.payload_start);
            for byte in &bytes[record.payload_start..record.payload_end] {
                if *byte == 0 {
                    return Err(Error::InvalidInput(
                        "legacy PowerPoint byte text atom contains a NUL byte".into(),
                    ));
                }
                text.push(char::from(*byte));
                if text.len() > MAX_PPT_TEXT_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "legacy PowerPoint byte text atom exceeds {MAX_PPT_TEXT_BYTES} bytes"
                    )));
                }
            }
            text
        }
        _ => {
            return Err(Error::InvalidInput(
                "legacy PowerPoint text decoder received an unsupported atom".into(),
            ));
        }
    };
    Ok(text)
}

fn push_outline_text(slide: &mut Slide, text: String, total_bytes: &mut usize) -> Result<()> {
    *total_bytes = total_bytes.saturating_add(text.len());
    if *total_bytes > MAX_PPT_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "legacy PowerPoint outline text exceeds {MAX_PPT_TEXT_BYTES} bytes"
        )));
    }
    slide.outline_text.push(text);
    Ok(())
}

fn append_text_blocks(text: &str, blocks: &mut Vec<HtmlBlock>) -> Result<()> {
    let mut paragraph = String::new();
    for character in text.chars() {
        match character {
            '\r' | '\n' | '\u{000B}' => {
                if !paragraph.trim().is_empty() {
                    blocks.push(HtmlBlock::Paragraph {
                        text: std::mem::take(&mut paragraph),
                    });
                } else {
                    paragraph.clear();
                }
            }
            '\t' => paragraph.push_str("    "),
            character if character.is_control() => {}
            character => paragraph.push(character),
        }
        if paragraph.len() > MAX_PPT_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "legacy PowerPoint paragraph exceeds {MAX_PPT_TEXT_BYTES} bytes"
            )));
        }
    }
    if !paragraph.trim().is_empty() {
        blocks.push(HtmlBlock::Paragraph { text: paragraph });
    }
    if blocks.len() > MAX_PPT_TEXT_BLOCKS {
        return Err(Error::LimitExceeded(format!(
            "legacy PowerPoint exceeds {MAX_PPT_TEXT_BLOCKS} text blocks"
        )));
    }
    Ok(())
}

fn collect_descendants(
    bytes: &[u8],
    root: Record,
    record_count: &mut usize,
) -> Result<Vec<Record>> {
    fn descend(
        bytes: &[u8],
        parent: Record,
        depth: usize,
        record_count: &mut usize,
        output: &mut Vec<Record>,
    ) -> Result<()> {
        if depth >= MAX_PPT_RECORD_DEPTH {
            return Err(Error::LimitExceeded(format!(
                "legacy PowerPoint record nesting exceeds {MAX_PPT_RECORD_DEPTH}"
            )));
        }
        let mut cursor = parent.payload_start;
        while cursor < parent.payload_end {
            let child = parse_record(bytes, cursor, parent.payload_end)?;
            *record_count = record_count.saturating_add(1);
            if *record_count > MAX_PPT_RECORDS {
                return Err(Error::LimitExceeded(format!(
                    "legacy PowerPoint record count exceeds {MAX_PPT_RECORDS}"
                )));
            }
            output.push(child);
            if child.version == 0x000F {
                descend(bytes, child, depth + 1, record_count, output)?;
            }
            cursor = child.payload_end;
        }
        Ok(())
    }
    let mut records = Vec::new();
    descend(bytes, root, 0, record_count, &mut records)?;
    Ok(records)
}

fn direct_children(bytes: &[u8], parent: Record, record_count: &mut usize) -> Result<Vec<Record>> {
    let mut children = Vec::new();
    let mut cursor = parent.payload_start;
    while cursor < parent.payload_end {
        let child = parse_record(bytes, cursor, parent.payload_end)?;
        *record_count = record_count.saturating_add(1);
        if *record_count > MAX_PPT_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "legacy PowerPoint record count exceeds {MAX_PPT_RECORDS}"
            )));
        }
        children.push(child);
        cursor = child.payload_end;
    }
    Ok(children)
}

fn parse_record(bytes: &[u8], offset: usize, bound: usize) -> Result<Record> {
    if offset
        .checked_add(8)
        .is_none_or(|end| end > bound || end > bytes.len())
    {
        return Err(Error::InvalidInput(
            "legacy PowerPoint record header is truncated".into(),
        ));
    }
    let version_instance = read_u16(bytes, offset).unwrap();
    let record_type = read_u16(bytes, offset + 2).unwrap();
    let payload_len = read_u32(bytes, offset + 4).unwrap() as usize;
    let payload_start = offset + 8;
    let payload_end = payload_start
        .checked_add(payload_len)
        .filter(|end| *end <= bound && *end <= bytes.len())
        .ok_or_else(|| {
            Error::InvalidInput("legacy PowerPoint record extends beyond its container".into())
        })?;
    Ok(Record {
        offset,
        payload_start,
        payload_end,
        record_type,
        instance: version_instance >> 4,
        version: version_instance & 0x000F,
    })
}

fn read_stream<R: Read + Seek>(compound: &mut CompoundFile<R>, name: &str) -> Result<Vec<u8>> {
    let mut stream = compound.open_stream(name).map_err(|error| {
        Error::InvalidInput(format!(
            "cannot open legacy PowerPoint {name} stream: {error}"
        ))
    })?;
    let mut bytes = Vec::new();
    Read::take(&mut stream, MAX_PPT_STREAM_BYTES.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PPT_STREAM_BYTES {
        return Err(Error::LimitExceeded(format!(
            "legacy PowerPoint {name} stream exceeds {MAX_PPT_STREAM_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

fn validate_compound_entries<R: Read + Seek>(
    compound: &CompoundFile<R>,
    file_bytes: usize,
) -> Result<()> {
    let mut entries = 0usize;
    let mut total_path_bytes = 0usize;
    for entry in compound.walk() {
        entries += 1;
        if entries > MAX_PPT_CFB_ENTRIES {
            return Err(Error::LimitExceeded(format!(
                "legacy PowerPoint compound file contains more than {MAX_PPT_CFB_ENTRIES} entries"
            )));
        }
        let path_bytes = entry.path().to_string_lossy().len();
        if path_bytes > MAX_PPT_PATH_BYTES {
            return Err(Error::LimitExceeded(format!(
                "legacy PowerPoint compound path exceeds {MAX_PPT_PATH_BYTES} bytes"
            )));
        }
        total_path_bytes = total_path_bytes.saturating_add(path_bytes);
        if total_path_bytes > MAX_PPT_PATHS_TOTAL_BYTES {
            return Err(Error::LimitExceeded(format!(
                "legacy PowerPoint compound paths exceed {MAX_PPT_PATHS_TOTAL_BYTES} bytes"
            )));
        }
        if entry.is_stream() && entry.len() > file_bytes as u64 {
            return Err(Error::InvalidInput(
                "legacy PowerPoint stream declares more bytes than its container".into(),
            ));
        }
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn push_warning_once(warnings: &mut Vec<String>, message: &str) {
    if !warnings.iter().any(|warning| warning == message) {
        warnings.push(message.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(record_type: u16, instance: u16, version: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((instance << 4) | version).to_le_bytes());
        bytes.extend_from_slice(&record_type.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn make_document(slide_id: u32, outline: &str) -> Vec<u8> {
        let mut slide_persist = Vec::new();
        slide_persist.extend_from_slice(&2u32.to_le_bytes());
        slide_persist.extend_from_slice(&0u32.to_le_bytes());
        slide_persist.extend_from_slice(&1u32.to_le_bytes());
        slide_persist.extend_from_slice(&slide_id.to_le_bytes());
        slide_persist.extend_from_slice(&0u32.to_le_bytes());
        let mut outline_records = record(RT_SLIDE_PERSIST, 0, 0, &slide_persist);
        outline_records.extend_from_slice(&record(RT_TEXT_HEADER, 0, 0, &[4, 0, 0, 0]));
        let chars: Vec<u8> = outline.encode_utf16().flat_map(u16::to_le_bytes).collect();
        outline_records.extend_from_slice(&record(RT_TEXT_CHARS, 0, 0, &chars));
        let slide_list = record(RT_SLIDE_LIST_WITH_TEXT, 0, 0xF, &outline_records);
        record(RT_DOCUMENT, 0, 0xF, &slide_list)
    }

    fn make_slide(outline: &str, outline_ref: bool) -> Vec<u8> {
        let chars: Vec<u8> = outline.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let textbox_content = if outline_ref {
            record(RT_OUTLINE_TEXT_REF, 0, 0, &0u32.to_le_bytes())
        } else {
            let mut content = record(RT_TEXT_HEADER, 0, 0, &[4, 0, 0, 0]);
            content.extend_from_slice(&record(RT_TEXT_CHARS, 0, 0, &chars));
            content
        };
        let textbox = record(RT_CLIENT_TEXTBOX, 0, 0xF, &textbox_content);
        let shape = record(0xF004, 0, 0xF, &textbox);
        let group = record(0xF003, 0, 0xF, &shape);
        let drawing_group = record(0xF002, 0, 0xF, &group);
        let drawing = record(0x040C, 0, 0xF, &drawing_group);
        record(RT_SLIDE, 0, 0xF, &drawing)
    }

    fn persist_directory(entries: &[(u32, u32)]) -> Vec<u8> {
        let mut payload = Vec::new();
        let start = entries.first().unwrap().0;
        payload.extend_from_slice(&(((entries.len() as u32) << 20) | start).to_le_bytes());
        for (_, offset) in entries {
            payload.extend_from_slice(&offset.to_le_bytes());
        }
        record(RT_PERSIST_DIRECTORY, 0, 0, &payload)
    }

    fn user_edit(
        previous: u32,
        persist_dir: u32,
        doc_id: u32,
        encryption_id: Option<u32>,
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&256u32.to_le_bytes());
        payload.extend_from_slice(&0x1FE9u16.to_le_bytes());
        payload.extend_from_slice(&[0, 3]);
        payload.extend_from_slice(&previous.to_le_bytes());
        payload.extend_from_slice(&persist_dir.to_le_bytes());
        payload.extend_from_slice(&doc_id.to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes());
        if let Some(encryption_id) = encryption_id {
            payload.extend_from_slice(&encryption_id.to_le_bytes());
        }
        record(RT_USER_EDIT, 0, 0, &payload)
    }

    fn current_user(current_edit: u32) -> Vec<u8> {
        let mut payload = vec![0; 20];
        payload[0..4].copy_from_slice(&20u32.to_le_bytes());
        payload[4..8].copy_from_slice(&0xE391_C05Fu32.to_le_bytes());
        payload[8..12].copy_from_slice(&current_edit.to_le_bytes());
        record(RT_CURRENT_USER, 0, 0, &payload)
    }

    #[test]
    fn parses_record_boundaries_and_rejects_truncation() {
        let bytes = record(RT_TEXT_CHARS, 0, 0, &[0x41, 0x00]);
        let parsed = parse_record(&bytes, 0, bytes.len()).unwrap();
        assert_eq!(parsed.record_type, RT_TEXT_CHARS);
        assert_eq!(
            decode_text_atom(&bytes, parsed, &mut Vec::new()).unwrap(),
            "A"
        );
        assert!(parse_record(&bytes, 0, bytes.len() - 1).is_err());
    }

    #[test]
    fn resolves_outline_text_and_direct_officeart_text() {
        let direct = record(RT_TEXT_HEADER, 0, 0, &[0; 4]);
        let mut chars = Vec::new();
        chars.extend_from_slice(&0x65E5u16.to_le_bytes());
        chars.extend_from_slice(&0x672Cu16.to_le_bytes());
        let chars = record(RT_TEXT_CHARS, 0, 0, &chars);
        let mut contents = direct.clone();
        contents.extend_from_slice(&chars);
        let textbox = record(RT_CLIENT_TEXTBOX, 0, 0xF, &contents);
        let parsed = parse_record(&textbox, 0, textbox.len()).unwrap();
        assert_eq!(
            read_client_textbox(&textbox, parsed, &[], &mut Vec::new())
                .unwrap()
                .as_deref(),
            Some("日本")
        );

        let mut outline = record(RT_OUTLINE_TEXT_REF, 0, 0, &0u32.to_le_bytes());
        let textbox = record(RT_CLIENT_TEXTBOX, 0, 0xF, &outline);
        let parsed = parse_record(&textbox, 0, textbox.len()).unwrap();
        assert_eq!(
            read_client_textbox(&textbox, parsed, &["cached title".into()], &mut Vec::new())
                .unwrap()
                .as_deref(),
            Some("cached title")
        );
        outline.clear();
    }

    #[test]
    fn text_bytes_are_unicode_low_bytes_and_controls_flow() {
        let bytes = record(RT_TEXT_BYTES, 0, 0, &[b'A', 0xE9, b'\r', b'B']);
        let parsed = parse_record(&bytes, 0, bytes.len()).unwrap();
        let text = decode_text_atom(&bytes, parsed, &mut Vec::new()).unwrap();
        assert_eq!(text, "Aé\rB");
        let mut blocks = Vec::new();
        append_text_blocks(&text, &mut blocks).unwrap();
        assert!(matches!(&blocks[0], HtmlBlock::Paragraph { text } if text == "Aé"));
        assert!(matches!(&blocks[1], HtmlBlock::Paragraph { text } if text == "B"));
    }

    #[test]
    fn follows_live_edit_chain_and_uses_latest_persist_override() {
        let mut bytes = make_document(256, "cached title");
        let old_slide_offset = bytes.len();
        let old_slide = make_slide("old edit", false);
        bytes.extend_from_slice(&old_slide);
        let old_directory_offset = bytes.len();
        let old_directory = persist_directory(&[(1, 0), (2, old_slide_offset as u32)]);
        bytes.extend_from_slice(&old_directory);
        let old_edit_offset = bytes.len();
        let old_edit = user_edit(0, old_directory_offset as u32, 1, None);
        bytes.extend_from_slice(&old_edit);
        let new_slide_offset = bytes.len();
        let new_slide = make_slide("current edit", false);
        bytes.extend_from_slice(&new_slide);
        let latest_directory_offset = bytes.len();
        let latest_directory = persist_directory(&[(2, new_slide_offset as u32)]);
        bytes.extend_from_slice(&latest_directory);
        let latest_edit_offset = bytes.len();
        let latest_edit = user_edit(
            old_edit_offset as u32,
            latest_directory_offset as u32,
            1,
            None,
        );
        bytes.extend_from_slice(&latest_edit);

        let mut warnings = Vec::new();
        let (directory, slides) = read_live_slides(
            &current_user(latest_edit_offset as u32),
            &bytes,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(slides.len(), 1);
        assert_eq!(directory.get(&2), Some(&new_slide_offset));
        let slide = parse_record(&bytes, new_slide_offset, bytes.len()).unwrap();
        let records = collect_descendants(&bytes, slide, &mut 0).unwrap();
        let textbox = records
            .into_iter()
            .find(|record| record.record_type == RT_CLIENT_TEXTBOX)
            .unwrap();
        assert_eq!(
            read_client_textbox(&bytes, textbox, &slides[0].outline_text, &mut warnings)
                .unwrap()
                .as_deref(),
            Some("current edit")
        );
    }

    #[test]
    fn rejects_encrypted_legacy_powerpoint_edits() {
        let mut bytes = make_document(256, "text");
        let slide_offset = bytes.len();
        let slide = make_slide("text", false);
        let slide_len = slide.len();
        bytes.extend_from_slice(&slide);
        let directory_offset = bytes.len();
        let directory = persist_directory(&[(1, 0), (2, slide_offset as u32)]);
        bytes.extend_from_slice(&directory);
        let edit_offset = bytes.len();
        let edit = user_edit(0, directory_offset as u32, 1, Some(3));
        bytes.extend_from_slice(&edit);
        assert_eq!(slide_len, make_slide("text", false).len());
        let mut warnings = Vec::new();
        let error =
            read_live_slides(&current_user(edit_offset as u32), &bytes, &mut warnings).unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)));

        let mut encrypted_token = current_user(edit_offset as u32);
        encrypted_token[12..16].copy_from_slice(&0xF3D1_C4DFu32.to_le_bytes());
        let error = read_live_slides(&encrypted_token, &bytes, &mut warnings).unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)));
    }
}
