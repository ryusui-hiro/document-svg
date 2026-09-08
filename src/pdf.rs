use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use rayon::prelude::*;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{
    GradientStop, IDENTITY, LineCap, LineJoin, LinearGradient, Matrix, Node, Page, Paint,
    RadialGradient, SourceMeta, Stroke, TextAnchor, TextRun, TilingPatternDefinition, compose,
    transform_point,
};

const MAX_GRAPHICS_STACK: usize = 256;
const MAX_FORM_DEPTH: usize = 32;
const MAX_PAGE_MESH_TRIANGLES: usize = 50_000;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let document = Document::load(path)?;
    if document.is_encrypted() {
        return Err(Error::Unsupported(
            "encrypted PDFs are rejected; access controls are not bypassed".into(),
        ));
    }
    let pages = document.get_pages().into_iter().collect::<Vec<_>>();
    if pages.len() > options.max_pages {
        return Err(Error::LimitExceeded(format!(
            "PDF contains {} pages; maximum is {}",
            pages.len(),
            options.max_pages
        )));
    }
    let page_content_limit = usize::try_from(options.max_zip_entry_bytes)
        .unwrap_or(usize::MAX)
        .min(usize::MAX / 2);
    let mut warnings = Vec::new();
    if options.jobs > 1 && pages.len() > 1 {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(options.jobs)
            .build()
            .map_err(|error| {
                Error::InvalidInput(format!("cannot create PDF worker pool: {error}"))
            })?;
        for batch in pages.chunks(options.jobs.max(1)) {
            let rendered = pool.install(|| {
                batch
                    .par_iter()
                    .map(|(page_number, page_id)| {
                        render_page(
                            &document,
                            *page_number as usize,
                            *page_id,
                            page_content_limit,
                            options.outline_embedded_pdf_text,
                        )
                    })
                    .collect::<Vec<_>>()
            });
            for page in rendered {
                let page = page?;
                warnings.extend(page.warnings.iter().cloned());
                sink.consume(page)?;
            }
        }
    } else {
        for (page_number, page_id) in pages {
            let page = render_page(
                &document,
                page_number as usize,
                page_id,
                page_content_limit,
                options.outline_embedded_pdf_text,
            )?;
            warnings.extend(page.warnings.iter().cloned());
            sink.consume(page)?;
        }
    }
    Ok(deduplicate(warnings))
}

fn render_page(
    document: &Document,
    page_number: usize,
    page_id: ObjectId,
    content_limit: usize,
    outline_embedded_pdf_text: bool,
) -> Result<Page> {
    let box_values = inherited_page_array(document, page_id, b"CropBox")
        .or_else(|| inherited_page_array(document, page_id, b"MediaBox"))
        .unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let rotation = inherited_page_number(document, page_id, b"Rotate")
        .unwrap_or(0.0)
        .round() as i32;
    let rotation = rotation.rem_euclid(360);
    let user_unit = page_user_unit(document, page_id)?;
    let source_width = (box_values[2] - box_values[0]).abs().max(1.0) * user_unit;
    let source_height = (box_values[3] - box_values[1]).abs().max(1.0) * user_unit;
    let (width, height) = if matches!(rotation, 90 | 270) {
        (source_height, source_width)
    } else {
        (source_width, source_height)
    };
    let page_matrix = compose(
        [user_unit, 0.0, 0.0, user_unit, 0.0, 0.0],
        page_matrix(box_values, rotation),
    );
    let mut page = Page::new(page_number, width, height, "pdf");
    page.description = format!("PDF page {page_number} converted from content operators");
    let content_bytes = document.get_page_content_with_limit(page_id, content_limit)?;
    let operations = decode_content_operations(&content_bytes, content_limit, &mut page)?;
    let fonts = build_font_decoders(document, page_id, content_limit, &mut page)?;
    let mut interpreter = Interpreter {
        document,
        page: &mut page,
        page_matrix,
        page_id,
        fonts,
        resources: Vec::new(),
        state: GraphicsState::default(),
        content_base_ctm: IDENTITY,
        stack: Vec::new(),
        path: PathBuilder::default(),
        pending_clip_rule: None,
        text: TextState::default(),
        node_counter: 0,
        clip_counter: 0,
        mask_counter: 0,
        compatibility_depth: 0,
        visited_forms: HashSet::new(),
        visited_patterns: HashSet::new(),
        mesh_output_count: Rc::new(Cell::new(0)),
        content_limit,
        outline_embedded_pdf_text,
    };
    interpreter.interpret(&operations, 0)?;
    if !interpreter.stack.is_empty() {
        interpreter
            .page
            .warn("PDF graphics-state stack was not balanced at end of page");
    }
    Ok(page)
}

fn decode_content_operations(
    data: &[u8],
    content_limit: usize,
    page: &mut Page,
) -> Result<Vec<Operation>> {
    let mut operations = Vec::new();
    let mut position = 0usize;
    while let Some(inline_start) = find_pdf_syntax_token(data, position, b"BI") {
        if inline_start > position {
            operations.extend(Content::decode(&data[position..inline_start])?.operations);
        }
        let dictionary_start = inline_start + 2;
        let Some(id_position) = find_pdf_syntax_token(data, dictionary_start, b"ID") else {
            return Err(Error::InvalidInput(
                "PDF inline image has no ID delimiter".into(),
            ));
        };
        let spec = InlineImageSpec::parse(&data[dictionary_start..id_position]);
        let data_start = skip_inline_separator(data, id_position + 2);
        let mut search_position = data_start;
        let mut decoded = None::<(Stream, usize)>;
        while let Some(ei_position) = find_pdf_token(data, search_position, b"EI") {
            let raw_end = trim_inline_data_end(data, data_start, ei_position);
            if let Some(stream) = spec.decode(&data[data_start..raw_end], content_limit) {
                decoded = Some((stream, ei_position + 2));
                break;
            }
            search_position = ei_position + 2;
        }
        if let Some((stream, next_position)) = decoded {
            operations.push(Operation::new("BI", vec![Object::Stream(stream)]));
            position = next_position;
        } else if let Some(ei_position) = find_pdf_token(data, data_start, b"EI") {
            page.warn(format!(
                "PDF inline image filter {} could not be recovered",
                spec.filter.as_deref().unwrap_or("unfiltered")
            ));
            position = ei_position + 2;
        } else {
            return Err(Error::InvalidInput(
                "PDF inline image has no EI delimiter".into(),
            ));
        }
    }
    if position < data.len() {
        operations.extend(Content::decode(&data[position..])?.operations);
    }
    Ok(operations)
}

fn find_pdf_syntax_token(data: &[u8], start: usize, token: &[u8]) -> Option<usize> {
    let mut position = start;
    let mut literal_depth = 0usize;
    let mut escaped = false;
    let mut hex_string = false;
    let mut comment = false;
    while position < data.len() {
        let byte = data[position];
        if comment {
            if matches!(byte, b'\r' | b'\n') {
                comment = false;
            }
            position += 1;
            continue;
        }
        if literal_depth > 0 {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'(' {
                literal_depth += 1;
            } else if byte == b')' {
                literal_depth -= 1;
            }
            position += 1;
            continue;
        }
        if hex_string {
            if byte == b'>' {
                hex_string = false;
            }
            position += 1;
            continue;
        }
        match byte {
            b'%' => {
                comment = true;
                position += 1;
                continue;
            }
            b'(' => {
                literal_depth = 1;
                position += 1;
                continue;
            }
            b'<' if data.get(position + 1) != Some(&b'<') => {
                hex_string = true;
                position += 1;
                continue;
            }
            _ => {}
        }
        if data.get(position..position + token.len()) == Some(token) {
            let before = position
                .checked_sub(1)
                .and_then(|index| data.get(index))
                .is_none_or(|byte| is_pdf_delimiter(*byte));
            let after = data
                .get(position + token.len())
                .is_none_or(|byte| is_pdf_delimiter(*byte));
            if before && after {
                return Some(position);
            }
        }
        position += 1;
    }
    None
}

fn find_pdf_token(data: &[u8], start: usize, token: &[u8]) -> Option<usize> {
    let mut search = start;
    while search + token.len() <= data.len() {
        let offset = data
            .get(search..)?
            .windows(token.len())
            .position(|window| window == token)?;
        let position = search + offset;
        let delimited = {
            let before = position
                .checked_sub(1)
                .and_then(|index| data.get(index))
                .is_none_or(|byte| is_pdf_delimiter(*byte));
            let after = data
                .get(position + token.len())
                .is_none_or(|byte| is_pdf_delimiter(*byte));
            before && after
        };
        if delimited {
            return Some(position);
        }
        search = position + 1;
    }
    None
}

fn is_pdf_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'[' | b']' | b'<' | b'>' | b'(' | b')' | b'/')
}

fn skip_inline_separator(data: &[u8], mut position: usize) -> usize {
    if data.get(position..position + 2) == Some(b"\r\n") {
        return position + 2;
    }
    if data
        .get(position)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        position += 1;
    }
    position
}

fn trim_inline_data_end(data: &[u8], start: usize, mut end: usize) -> usize {
    if end > start && data[end - 1] == b'\n' {
        end -= 1;
        if end > start && data[end - 1] == b'\r' {
            end -= 1;
        }
    } else if end > start && data[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    end
}

#[derive(Default)]
struct InlineImageSpec {
    width: usize,
    height: usize,
    bits: usize,
    color_space: String,
    filter: Option<String>,
    k: i64,
    columns: usize,
    rows: usize,
    end_of_block: bool,
    end_of_line: bool,
    byte_aligned: bool,
    black_is_one: bool,
    damaged_rows_before_error: usize,
    interpolate: bool,
}

impl InlineImageSpec {
    fn parse(data: &[u8]) -> Self {
        let tokens = inline_dictionary_tokens(data);
        let value = |names: &[&str]| {
            tokens.iter().enumerate().find_map(|(index, token)| {
                names
                    .contains(&token.as_str())
                    .then(|| tokens.get(index + 1).cloned())
                    .flatten()
            })
        };
        let integer = |names: &[&str], default: i64| {
            value(names)
                .and_then(|value| value.parse().ok())
                .unwrap_or(default)
        };
        let boolean =
            |names: &[&str], default: bool| value(names).map_or(default, |value| value == "true");
        let width = integer(&["/W", "/Width"], 0).max(0) as usize;
        let height = integer(&["/H", "/Height"], 0).max(0) as usize;
        Self {
            width,
            height,
            bits: integer(&["/BPC", "/BitsPerComponent"], 8).max(1) as usize,
            color_space: value(&["/CS", "/ColorSpace"]).unwrap_or_else(|| "/G".into()),
            filter: value(&["/F", "/Filter"]),
            k: integer(&["/K"], 0),
            columns: integer(&["/Columns"], width as i64).max(0) as usize,
            rows: integer(&["/Rows"], height as i64).max(0) as usize,
            end_of_block: boolean(&["/EndOfBlock"], true),
            end_of_line: boolean(&["/EndOfLine"], false),
            byte_aligned: boolean(&["/EncodedByteAlign"], false),
            black_is_one: boolean(&["/BlackIs1"], false),
            damaged_rows_before_error: integer(&["/DamagedRowsBeforeError"], 0).max(0) as usize,
            interpolate: boolean(&["/I", "/Interpolate"], false),
        }
    }

    fn decode(&self, raw: &[u8], content_limit: usize) -> Option<Stream> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let filter = self.filter.as_deref().unwrap_or("");
        let pixels = if matches!(filter, "/CCF" | "/CCITTFaxDecode") {
            self.decode_ccitt(raw)?
        } else if filter.is_empty() {
            raw.to_vec()
        } else {
            let filter_name = match filter {
                "/LZW" | "/LZWDecode" => b"LZWDecode".to_vec(),
                "/Fl" | "/FlateDecode" => b"FlateDecode".to_vec(),
                "/RL" | "/RunLengthDecode" => b"RunLengthDecode".to_vec(),
                "/A85" | "/ASCII85Decode" => b"ASCII85Decode".to_vec(),
                "/AHx" | "/ASCIIHexDecode" => b"ASCIIHexDecode".to_vec(),
                _ => return None,
            };
            let mut dictionary = Dictionary::new();
            dictionary.set("Filter", Object::Name(filter_name));
            Stream::new(dictionary, raw.to_vec())
                .decompressed_content_with_limit(content_limit)
                .ok()?
        };
        let components = match self.color_space.as_str() {
            "/RGB" | "/DeviceRGB" => 3,
            "/CMYK" | "/DeviceCMYK" => 4,
            _ => 1,
        };
        let expected = self
            .width
            .saturating_mul(self.height)
            .saturating_mul(components)
            .saturating_mul(self.bits)
            .div_ceil(8);
        if self.bits == 8 && pixels.len() < expected {
            return None;
        }
        let mut dictionary = Dictionary::new();
        dictionary.set("Subtype", Object::Name(b"Image".to_vec()));
        dictionary.set("Width", self.width as i64);
        dictionary.set("Height", self.height as i64);
        dictionary.set(
            "BitsPerComponent",
            if filter.contains("CCF") {
                8
            } else {
                self.bits as i64
            },
        );
        dictionary.set(
            "ColorSpace",
            Object::Name(match self.color_space.as_str() {
                "/RGB" | "/DeviceRGB" => b"DeviceRGB".to_vec(),
                "/CMYK" | "/DeviceCMYK" => b"DeviceCMYK".to_vec(),
                _ => b"DeviceGray".to_vec(),
            }),
        );
        dictionary.set("Interpolate", self.interpolate);
        Some(Stream::new(dictionary, pixels))
    }

    fn decode_ccitt(&self, raw: &[u8]) -> Option<Vec<u8>> {
        let mode = if self.k < 0 {
            hayro_ccitt::EncodingMode::Group4
        } else if self.k == 0 {
            hayro_ccitt::EncodingMode::Group3_1D
        } else {
            hayro_ccitt::EncodingMode::Group3_2D { k: self.k as u32 }
        };
        let settings = hayro_ccitt::DecodeSettings {
            columns: self.columns.max(self.width) as u32,
            rows: self.rows.max(self.height) as u32,
            end_of_block: self.end_of_block,
            end_of_line: self.end_of_line,
            rows_are_byte_aligned: self.byte_aligned,
            encoding: mode,
            invert_black: self.black_is_one,
        };
        let mut decoder = GrayCcittDecoder::default();
        let mut context = hayro_ccitt::DecoderContext::new(settings);
        let required = self.width.saturating_mul(self.height);
        if hayro_ccitt::decode(raw, &mut decoder, &mut context).is_ok()
            && decoder.pixels.len() >= required
        {
            decoder.pixels.truncate(required);
            return Some(decoder.pixels);
        }
        if self.damaged_rows_before_error == 0
            || !self.end_of_line
            || self.k <= 0
            || self.byte_aligned
        {
            return None;
        }
        let reference_row = decoder
            .rows
            .checked_sub(1)
            .and_then(|row| {
                let start = row.checked_mul(self.width)?;
                decoder.pixels.get(start..start.checked_add(self.width)?)
            })
            .map(<[u8]>::to_vec);
        repair_group3_2d_damaged_row(
            raw,
            settings,
            required,
            decoder.rows,
            reference_row.as_deref(),
        )
    }
}

fn ccitt_spec_from_stream(stream: &Stream, width: usize, height: usize) -> InlineImageSpec {
    let parameters = stream
        .dict
        .get(b"DecodeParms")
        .or_else(|_| stream.dict.get(b"DP"))
        .ok()
        .and_then(|value| match value {
            Object::Dictionary(dictionary) => Some(dictionary),
            Object::Array(values) => values.first().and_then(|value| value.as_dict().ok()),
            _ => None,
        });
    let integer = |name: &[u8], default: i64| {
        parameters
            .and_then(|dictionary| dictionary.get(name).and_then(Object::as_i64).ok())
            .unwrap_or(default)
    };
    let boolean = |name: &[u8], default: bool| {
        parameters
            .and_then(|dictionary| dictionary.get(name).and_then(Object::as_bool).ok())
            .unwrap_or(default)
    };
    InlineImageSpec {
        width,
        height,
        bits: 8,
        color_space: "/G".into(),
        filter: Some("/CCITTFaxDecode".into()),
        k: integer(b"K", 0),
        columns: integer(b"Columns", width as i64).max(1) as usize,
        rows: integer(b"Rows", height as i64).max(1) as usize,
        end_of_block: boolean(b"EndOfBlock", true),
        end_of_line: boolean(b"EndOfLine", false),
        byte_aligned: boolean(b"EncodedByteAlign", false),
        black_is_one: boolean(b"BlackIs1", false),
        damaged_rows_before_error: integer(b"DamagedRowsBeforeError", 0).max(0) as usize,
        interpolate: stream
            .dict
            .get(b"Interpolate")
            .and_then(Object::as_bool)
            .unwrap_or(false),
    }
}

fn repair_group3_2d_damaged_row(
    raw: &[u8],
    settings: hayro_ccitt::DecodeSettings,
    required_pixels: usize,
    failing_row: usize,
    reference_row: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let bit_length = raw.len().checked_mul(8)?;
    let mut eol_positions = Vec::new();
    for position in 0..bit_length.saturating_sub(11) {
        if (0..11).all(|offset| packed_bit(raw, position + offset) == 0)
            && packed_bit(raw, position + 11) == 1
        {
            eol_positions.push(position);
            if eol_positions.len() > settings.rows as usize + 6 {
                break;
            }
        }
    }
    let row_count = settings.rows as usize;
    let mut candidates = vec![failing_row];
    if failing_row > 0 {
        candidates.push(failing_row - 1);
    }
    if failing_row + 1 < row_count {
        candidates.push(failing_row + 1);
    }
    for candidate in candidates {
        let Some(&start) = eol_positions.get(candidate) else {
            continue;
        };
        let Some(&end) = eol_positions.get(candidate + 1) else {
            continue;
        };
        if candidate == failing_row
            && let Some(reference_row) = reference_row
        {
            let mut repaired = Vec::with_capacity(raw.len());
            let mut repaired_bits = 0usize;
            append_packed_bits(raw, 0, start, &mut repaired, &mut repaired_bits);
            for _ in 0..11 {
                push_packed_bit(&mut repaired, &mut repaired_bits, 0);
            }
            push_packed_bit(&mut repaired, &mut repaired_bits, 1);
            push_packed_bit(&mut repaired, &mut repaired_bits, 0);
            let black_runs = reference_row
                .iter()
                .enumerate()
                .filter(|(index, pixel)| {
                    **pixel == 0 && (*index == 0 || reference_row[*index - 1] != 0)
                })
                .count();
            if black_runs == 0 {
                push_packed_bit(&mut repaired, &mut repaired_bits, 1);
            } else {
                for _ in 0..black_runs {
                    for bit in [0, 0, 0, 1] {
                        push_packed_bit(&mut repaired, &mut repaired_bits, bit);
                    }
                }
            }
            append_packed_bits(raw, end, bit_length, &mut repaired, &mut repaired_bits);
            if let Some(pixels) = decode_repaired_ccitt(&repaired, settings, required_pixels) {
                return Some(pixels);
            }
        }
        let mut replacements = Vec::new();
        if candidate > 0 {
            replacements.push((eol_positions[candidate - 1], start));
        }
        if let Some(&next_end) = eol_positions.get(candidate + 2) {
            replacements.push((end, next_end));
        }
        for (replacement_start, replacement_end) in replacements {
            let mut repaired = Vec::with_capacity(raw.len());
            let mut repaired_bits = 0usize;
            append_packed_bits(raw, 0, start, &mut repaired, &mut repaired_bits);
            append_packed_bits(
                raw,
                replacement_start,
                replacement_end,
                &mut repaired,
                &mut repaired_bits,
            );
            append_packed_bits(raw, end, bit_length, &mut repaired, &mut repaired_bits);
            if let Some(pixels) = decode_repaired_ccitt(&repaired, settings, required_pixels) {
                return Some(pixels);
            }
        }
        let mut repaired = Vec::with_capacity(raw.len());
        let mut repaired_bits = 0usize;
        append_packed_bits(raw, 0, start, &mut repaired, &mut repaired_bits);
        for _ in 0..11 {
            push_packed_bit(&mut repaired, &mut repaired_bits, 0);
        }
        push_packed_bit(&mut repaired, &mut repaired_bits, 1);
        push_packed_bit(&mut repaired, &mut repaired_bits, 0);
        push_packed_bit(&mut repaired, &mut repaired_bits, 1);
        append_packed_bits(raw, end, bit_length, &mut repaired, &mut repaired_bits);
        if let Some(pixels) = decode_repaired_ccitt(&repaired, settings, required_pixels) {
            return Some(pixels);
        }
    }
    None
}

fn decode_repaired_ccitt(
    data: &[u8],
    settings: hayro_ccitt::DecodeSettings,
    required_pixels: usize,
) -> Option<Vec<u8>> {
    let mut decoder = GrayCcittDecoder::default();
    let mut context = hayro_ccitt::DecoderContext::new(settings);
    hayro_ccitt::decode(data, &mut decoder, &mut context).ok()?;
    if decoder.pixels.len() < required_pixels {
        return None;
    }
    decoder.pixels.truncate(required_pixels);
    Some(decoder.pixels)
}

fn packed_bit(data: &[u8], position: usize) -> u8 {
    (data[position / 8] >> (7 - position % 8)) & 1
}

fn append_packed_bits(
    source: &[u8],
    start: usize,
    end: usize,
    output: &mut Vec<u8>,
    output_bits: &mut usize,
) {
    for position in start..end {
        push_packed_bit(output, output_bits, packed_bit(source, position));
    }
}

fn push_packed_bit(output: &mut Vec<u8>, bit_length: &mut usize, bit: u8) {
    if bit_length.is_multiple_of(8) {
        output.push(0);
    }
    if bit != 0 {
        let index = output.len() - 1;
        output[index] |= 1 << (7 - *bit_length % 8);
    }
    *bit_length += 1;
}

fn inline_dictionary_tokens(data: &[u8]) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut position = 0usize;
    while position < data.len() {
        while data
            .get(position)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            position += 1;
        }
        if position >= data.len() {
            break;
        }
        let start = position;
        if data[position] == b'/' {
            position += 1;
            while position < data.len() && !is_pdf_delimiter(data[position]) {
                position += 1;
            }
            tokens.push(String::from_utf8_lossy(&data[start..position]).into_owned());
        } else if position + 1 < data.len()
            && matches!(&data[position..position + 2], b"<<" | b">>")
        {
            position += 2;
            tokens.push(String::from_utf8_lossy(&data[start..position]).into_owned());
        } else {
            while position < data.len() && !is_pdf_delimiter(data[position]) {
                position += 1;
            }
            if position == start {
                position += 1;
            }
            tokens.push(String::from_utf8_lossy(&data[start..position]).into_owned());
        }
    }
    tokens
}

#[derive(Default)]
struct GrayCcittDecoder {
    pixels: Vec<u8>,
    rows: usize,
}

impl hayro_ccitt::Decoder for GrayCcittDecoder {
    fn push_pixel(&mut self, white: bool) {
        self.pixels.push(if white { 255 } else { 0 });
    }

    fn push_pixel_chunk(&mut self, white: bool, chunk_count: u32) {
        self.pixels.extend(std::iter::repeat_n(
            if white { 255 } else { 0 },
            chunk_count as usize * 8,
        ));
    }

    fn next_line(&mut self) {
        self.rows += 1;
    }
}

#[derive(Clone, Debug)]
struct GraphicsState {
    ctm: Matrix,
    fill: Paint,
    stroke: Paint,
    fill_color_space: Object,
    stroke_color_space: Object,
    fill_pattern_clip: Option<PatternClip>,
    stroke_pattern_clip: Option<PatternClip>,
    line_width: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash_array: Vec<f64>,
    dash_offset: f64,
    fill_alpha: f64,
    stroke_alpha: f64,
    clip_id: Option<String>,
    blend_mode: String,
    mask_id: Option<String>,
    alpha_is_shape: bool,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: IDENTITY,
            fill: Paint::solid("#000000"),
            stroke: Paint::solid("#000000"),
            fill_color_space: Object::Name(b"DeviceGray".to_vec()),
            stroke_color_space: Object::Name(b"DeviceGray".to_vec()),
            fill_pattern_clip: None,
            stroke_pattern_clip: None,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash_array: Vec::new(),
            dash_offset: 0.0,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            clip_id: None,
            blend_mode: "normal".into(),
            mask_id: None,
            alpha_is_shape: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PatternClip {
    bbox: [f64; 4],
    matrix: Matrix,
}

#[derive(Clone, Debug)]
struct TextState {
    font_name: Vec<u8>,
    font_size: f64,
    text_matrix: Matrix,
    line_matrix: Matrix,
    character_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    leading: f64,
    rise: f64,
    rendering_mode: i64,
    pending_clips: Vec<PendingTextClip>,
}

#[derive(Clone, Debug)]
struct SavedTextParameters {
    font_name: Vec<u8>,
    font_size: f64,
    character_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
    leading: f64,
    rise: f64,
    rendering_mode: i64,
}

#[derive(Clone, Debug)]
struct SavedGraphicsState {
    graphics: GraphicsState,
    text: SavedTextParameters,
}

impl TextState {
    fn saved_parameters(&self) -> SavedTextParameters {
        SavedTextParameters {
            font_name: self.font_name.clone(),
            font_size: self.font_size,
            character_spacing: self.character_spacing,
            word_spacing: self.word_spacing,
            horizontal_scale: self.horizontal_scale,
            leading: self.leading,
            rise: self.rise,
            rendering_mode: self.rendering_mode,
        }
    }

    fn restore_parameters(&mut self, saved: SavedTextParameters) {
        self.font_name = saved.font_name;
        self.font_size = saved.font_size;
        self.character_spacing = saved.character_spacing;
        self.word_spacing = saved.word_spacing;
        self.horizontal_scale = saved.horizontal_scale;
        self.leading = saved.leading;
        self.rise = saved.rise;
        self.rendering_mode = saved.rendering_mode;
    }
}

#[derive(Clone, Debug)]
struct PendingTextClip {
    d: String,
    transform: Matrix,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            font_name: Vec::new(),
            font_size: 12.0,
            text_matrix: IDENTITY,
            line_matrix: IDENTITY,
            character_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            rendering_mode: 0,
            pending_clips: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct PathBuilder {
    data: String,
    has_content: bool,
    current_point: Option<(f64, f64)>,
    subpath_start: Option<(f64, f64)>,
}

impl PathBuilder {
    fn command(&mut self, operator: &str, values: &[f64]) {
        if self.has_content {
            self.data.push(' ');
        }
        self.data.push_str(operator);
        for value in values {
            self.data.push(' ');
            self.data.push_str(&fmt(*value));
        }
        self.has_content = true;
        match operator {
            "M" if values.len() >= 2 => {
                let point = (values[0], values[1]);
                self.current_point = Some(point);
                self.subpath_start = Some(point);
            }
            "L" if values.len() >= 2 => {
                self.current_point = Some((values[0], values[1]));
            }
            "C" if values.len() >= 6 => {
                self.current_point = Some((values[4], values[5]));
            }
            "Z" => self.current_point = self.subpath_start,
            _ => {}
        }
    }

    fn take(&mut self) -> String {
        self.has_content = false;
        self.current_point = None;
        self.subpath_start = None;
        std::mem::take(&mut self.data)
    }

    fn clear(&mut self) {
        self.data.clear();
        self.has_content = false;
        self.current_point = None;
        self.subpath_start = None;
    }
}

#[derive(Clone, Debug)]
struct FontDecoder {
    family: String,
    bold: bool,
    italic: bool,
    requires_outline: bool,
    font_data: Option<Arc<[u8]>>,
    glyph_names: HashMap<u8, String>,
    unicode_map: HashMap<Vec<u8>, String>,
    code_lengths: Vec<usize>,
    fallback_kind: FontFallback,
    widths: HashMap<u32, f64>,
    default_width: f64,
    type3: Option<Arc<Type3Font>>,
}

#[derive(Clone, Debug)]
struct Type3Font {
    font_matrix: Matrix,
    resources: Option<Dictionary>,
    glyphs: HashMap<u8, Stream>,
}

#[derive(Clone, Copy, Debug)]
enum FontFallback {
    OneByte,
    Utf16Be,
}

impl FontDecoder {
    fn decode(&self, bytes: &[u8]) -> (String, bool) {
        if !self.unicode_map.is_empty() {
            let mut result = String::new();
            let mut position = 0usize;
            let mut replacement = false;
            while position < bytes.len() {
                let mut matched = false;
                for length in &self.code_lengths {
                    if position + length > bytes.len() {
                        continue;
                    }
                    if let Some(value) = self.unicode_map.get(&bytes[position..position + length]) {
                        result.push_str(value);
                        position += length;
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    result.push('\u{fffd}');
                    position += 1;
                    replacement = true;
                }
            }
            return (result, replacement);
        }
        match self.fallback_kind {
            FontFallback::OneByte => (bytes.iter().map(|byte| char::from(*byte)).collect(), false),
            FontFallback::Utf16Be => {
                let units = bytes
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
                let mut replacement = !bytes.len().is_multiple_of(2);
                let result = char::decode_utf16(units)
                    .map(|item| match item {
                        Ok(character) => character,
                        Err(_) => {
                            replacement = true;
                            '\u{fffd}'
                        }
                    })
                    .collect();
                (result, replacement)
            }
        }
    }

    fn width(&self, bytes: &[u8]) -> f64 {
        let code_length = if matches!(self.fallback_kind, FontFallback::Utf16Be) {
            self.code_lengths.first().copied().unwrap_or(2).max(1)
        } else {
            1
        };
        bytes
            .chunks(code_length)
            .map(|code| {
                self.widths
                    .get(&bytes_to_u32(code))
                    .copied()
                    .unwrap_or(self.default_width)
            })
            .sum::<f64>()
            / 1_000.0
    }

    fn code_count(&self, bytes: &[u8]) -> usize {
        let code_length = if matches!(self.fallback_kind, FontFallback::Utf16Be) {
            self.code_lengths.first().copied().unwrap_or(2).max(1)
        } else {
            1
        };
        bytes.len().div_ceil(code_length)
    }

    fn glyph_x_offsets(
        &self,
        bytes: &[u8],
        font_size: f64,
        character_spacing: f64,
        word_spacing: f64,
        horizontal_scale: f64,
    ) -> Option<Vec<f64>> {
        let code_length = if matches!(self.fallback_kind, FontFallback::Utf16Be) {
            self.code_lengths.first().copied().unwrap_or(2).max(1)
        } else {
            1
        };
        let inverse_horizontal_scale = 1.0 / horizontal_scale.abs().max(1e-12);
        let mut offsets = Vec::with_capacity(bytes.len().div_ceil(code_length));
        let mut x = 0.0;
        for code in bytes.chunks(code_length) {
            let (decoded, replacement) = self.decode(code);
            let mut characters = decoded.chars();
            let character = characters.next()?;
            if replacement || characters.next().is_some() {
                return None;
            }
            offsets.push(x);
            let width = self
                .widths
                .get(&bytes_to_u32(code))
                .copied()
                .unwrap_or(self.default_width)
                / 1_000.0
                * font_size;
            let spacing = character_spacing + if character == ' ' { word_spacing } else { 0.0 };
            x += width + spacing * inverse_horizontal_scale;
        }
        Some(offsets)
    }

    fn outline_path(
        &self,
        bytes: &[u8],
        character_spacing_em: f64,
        word_spacing_em: f64,
    ) -> std::result::Result<String, &'static str> {
        let data = self
            .font_data
            .as_ref()
            .ok_or("embedded font program is unavailable")?;
        let face = match ttf_parser::Face::parse(data, 0) {
            Ok(face) => face,
            Err(_) => {
                return self.outline_type1(data, bytes, character_spacing_em, word_spacing_em);
            }
        };
        let units_per_em = f64::from(face.units_per_em()).max(1.0);
        let code_length = if matches!(self.fallback_kind, FontFallback::Utf16Be) {
            self.code_lengths.first().copied().unwrap_or(2).max(1)
        } else {
            1
        };
        let mut builder = SvgGlyphOutline::new(units_per_em);
        let mut x_offset = 0.0;
        let mut has_visible_character = false;
        for code in bytes.chunks(code_length) {
            let (decoded, replacement) = self.decode(code);
            if replacement || decoded.is_empty() {
                return Err("font code could not be mapped to Unicode");
            }
            builder.x_offset = x_offset;
            for character in decoded.chars() {
                has_visible_character |= !character.is_whitespace();
                let glyph_id = face
                    .glyph_index(character)
                    .or_else(|| {
                        face.tables().cmap.and_then(|cmap| {
                            cmap.subtables
                                .into_iter()
                                .find_map(|subtable| subtable.glyph_index(bytes_to_u32(code)))
                        })
                    })
                    .ok_or("Unicode character has no glyph in the embedded font")?;
                let _ = face.outline_glyph(glyph_id, &mut builder);
            }
            let word_spacing = if decoded == " " { word_spacing_em } else { 0.0 };
            x_offset += self
                .widths
                .get(&bytes_to_u32(code))
                .copied()
                .unwrap_or(self.default_width)
                / 1_000.0
                + character_spacing_em
                + word_spacing;
        }
        if builder.path.is_empty() {
            if has_visible_character {
                Err("text contains no visible glyph outlines")
            } else {
                Ok(String::new())
            }
        } else {
            Ok(builder.path)
        }
    }

    fn outline_type1(
        &self,
        data: &[u8],
        bytes: &[u8],
        character_spacing_em: f64,
        word_spacing_em: f64,
    ) -> std::result::Result<String, &'static str> {
        let Ok(font) = stet_fonts::type1_parser::parse_type1(data) else {
            return self.outline_cff(data, bytes, character_spacing_em, word_spacing_em);
        };
        let lookup = |name: &str| font.charstrings.get(name).cloned();
        let mut path = String::new();
        let mut x_offset = 0.0;
        let mut has_visible_character = false;
        for code in bytes {
            let (decoded, replacement) = self.decode(&[*code]);
            if replacement {
                return Err("font code could not be mapped to text");
            }
            has_visible_character |= decoded.chars().any(|character| !character.is_whitespace());
            let glyph_name = self
                .glyph_names
                .get(code)
                .map(String::as_str)
                .or_else(|| font.encoding.get(usize::from(*code)).map(String::as_str))
                .filter(|name| !name.is_empty() && *name != ".notdef")
                .or_else(|| (!decoded.is_empty()).then_some(decoded.as_str()))
                .ok_or("Type1 character code has no glyph name")?;
            if let Some(charstring) = font.charstrings.get(glyph_name) {
                let outline = stet_fonts::charstring::execute_charstring_ex(
                    charstring,
                    &font.subrs,
                    font.len_iv,
                    false,
                    Some(&lookup),
                )
                .map_err(|_| "Type1 glyph outline could not be decoded")?;
                append_stet_path(&mut path, &outline.path, font.font_matrix, x_offset);
            } else if !decoded.chars().all(char::is_whitespace) {
                return Err("Type1 glyph name is absent from CharStrings");
            }
            let word_spacing = if decoded == " " { word_spacing_em } else { 0.0 };
            x_offset += self
                .widths
                .get(&u32::from(*code))
                .copied()
                .unwrap_or(self.default_width)
                / 1_000.0
                + character_spacing_em
                + word_spacing;
        }
        if path.is_empty() {
            if has_visible_character {
                Err("Type1 text contains no visible glyph outlines")
            } else {
                Ok(String::new())
            }
        } else {
            Ok(path)
        }
    }

    fn outline_cff(
        &self,
        data: &[u8],
        bytes: &[u8],
        character_spacing_em: f64,
        word_spacing_em: f64,
    ) -> std::result::Result<String, &'static str> {
        let fonts = stet_fonts::cff_parser::parse_cff(data)
            .map_err(|_| "embedded font program is neither OpenType, Type1, nor CFF")?;
        let font = fonts.first().ok_or("CFF font set is empty")?;
        let mut path = String::new();
        let mut x_offset = 0.0;
        let mut has_visible_character = false;
        for code in bytes {
            let (decoded, replacement) = self.decode(&[*code]);
            if replacement {
                return Err("CFF character code could not be mapped to text");
            }
            has_visible_character |= decoded.chars().any(|character| !character.is_whitespace());
            let glyph_id = self
                .glyph_names
                .get(code)
                .and_then(|name| font.charset.iter().position(|candidate| candidate == name))
                .or_else(|| {
                    let mut characters = decoded.chars();
                    let character = characters.next()?;
                    if characters.next().is_some() || u32::from(character) > u32::from(u16::MAX) {
                        return None;
                    }
                    font.charset.iter().position(|name| {
                        stet_fonts::agl::glyph_name_to_unicode(name) == Some(character as u16)
                    })
                })
                .or_else(|| {
                    font.encoding
                        .get(usize::from(*code))
                        .copied()
                        .map(usize::from)
                        .filter(|glyph_id| *glyph_id != 0)
                });
            if let Some(glyph_id) = glyph_id {
                let charstring = font
                    .char_strings
                    .get(glyph_id)
                    .ok_or("CFF glyph index is outside CharStrings")?;
                let (local_subrs, default_width, nominal_width, font_matrix) = if font.is_cid {
                    let fd_index = font
                        .fd_select
                        .get(glyph_id)
                        .copied()
                        .map(usize::from)
                        .unwrap_or(0);
                    let fd = font
                        .fd_array
                        .get(fd_index)
                        .ok_or("CFF CID glyph has no font dictionary")?;
                    (
                        fd.local_subrs.as_slice(),
                        fd.default_width_x,
                        fd.nominal_width_x,
                        fd.font_matrix.unwrap_or(font.font_matrix),
                    )
                } else {
                    (
                        font.local_subrs.as_slice(),
                        font.default_width_x,
                        font.nominal_width_x,
                        font.font_matrix,
                    )
                };
                let outline = stet_fonts::type2_charstring::execute_type2_charstring(
                    charstring,
                    local_subrs,
                    &font.global_subrs,
                    default_width,
                    nominal_width,
                    false,
                )
                .map_err(|_| "CFF Type2 glyph outline could not be decoded")?;
                append_stet_path(&mut path, &outline.path, font_matrix, x_offset);
            } else if !decoded.chars().all(char::is_whitespace) {
                return Err("CFF character code has no glyph");
            }
            let word_spacing = if decoded == " " { word_spacing_em } else { 0.0 };
            x_offset += self
                .widths
                .get(&u32::from(*code))
                .copied()
                .unwrap_or(self.default_width)
                / 1_000.0
                + character_spacing_em
                + word_spacing;
        }
        if path.is_empty() {
            if has_visible_character {
                Err("CFF text contains no visible glyph outlines")
            } else {
                Ok(String::new())
            }
        } else {
            Ok(path)
        }
    }
}

struct SvgGlyphOutline {
    path: String,
    units_per_em: f64,
    x_offset: f64,
}

impl SvgGlyphOutline {
    fn new(units_per_em: f64) -> Self {
        Self {
            path: String::new(),
            units_per_em,
            x_offset: 0.0,
        }
    }

    fn point(&self, x: f32, y: f32) -> (f64, f64) {
        (
            self.x_offset + f64::from(x) / self.units_per_em,
            f64::from(y) / self.units_per_em,
        )
    }

    fn command(&mut self, operator: &str, points: &[(f64, f64)]) {
        if !self.path.is_empty() {
            self.path.push(' ');
        }
        self.path.push_str(operator);
        for (x, y) in points {
            self.path.push_str(&format!(" {} {}", fmt(*x), fmt(*y)));
        }
    }
}

impl ttf_parser::OutlineBuilder for SvgGlyphOutline {
    fn move_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        self.command("M", &[point]);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let point = self.point(x, y);
        self.command("L", &[point]);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let control = self.point(x1, y1);
        let point = self.point(x, y);
        self.command("Q", &[control, point]);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let first = self.point(x1, y1);
        let second = self.point(x2, y2);
        let point = self.point(x, y);
        self.command("C", &[first, second, point]);
    }

    fn close(&mut self) {
        self.command("Z", &[]);
    }
}

fn append_stet_path(
    output: &mut String,
    path: &stet_fonts::PsPath,
    matrix: [f64; 6],
    x_offset: f64,
) {
    let transform = |x: f64, y: f64| {
        (
            x_offset + matrix[0] * x + matrix[2] * y + matrix[4],
            matrix[1] * x + matrix[3] * y + matrix[5],
        )
    };
    for segment in &path.segments {
        if !output.is_empty() {
            output.push(' ');
        }
        match *segment {
            stet_fonts::PathSegment::MoveTo(x, y) => {
                let point = transform(x, y);
                output.push_str(&format!("M {} {}", fmt(point.0), fmt(point.1)));
            }
            stet_fonts::PathSegment::LineTo(x, y) => {
                let point = transform(x, y);
                output.push_str(&format!("L {} {}", fmt(point.0), fmt(point.1)));
            }
            stet_fonts::PathSegment::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x3,
                y3,
            } => {
                let first = transform(x1, y1);
                let second = transform(x2, y2);
                let third = transform(x3, y3);
                output.push_str(&format!(
                    "C {} {} {} {} {} {}",
                    fmt(first.0),
                    fmt(first.1),
                    fmt(second.0),
                    fmt(second.1),
                    fmt(third.0),
                    fmt(third.1)
                ));
            }
            stet_fonts::PathSegment::ClosePath => output.push('Z'),
        }
    }
}

struct Interpreter<'a, 'page> {
    document: &'a Document,
    page: &'page mut Page,
    page_matrix: Matrix,
    page_id: ObjectId,
    fonts: HashMap<Vec<u8>, FontDecoder>,
    resources: Vec<Dictionary>,
    state: GraphicsState,
    content_base_ctm: Matrix,
    stack: Vec<SavedGraphicsState>,
    path: PathBuilder,
    pending_clip_rule: Option<String>,
    text: TextState,
    node_counter: usize,
    clip_counter: usize,
    mask_counter: usize,
    compatibility_depth: usize,
    visited_forms: HashSet<ObjectId>,
    visited_patterns: HashSet<ObjectId>,
    mesh_output_count: Rc<Cell<usize>>,
    content_limit: usize,
    outline_embedded_pdf_text: bool,
}

impl Interpreter<'_, '_> {
    fn interpret(&mut self, operations: &[Operation], depth: usize) -> Result<()> {
        if depth > MAX_FORM_DEPTH {
            return Err(Error::LimitExceeded(format!(
                "PDF Form XObject recursion exceeds {MAX_FORM_DEPTH}"
            )));
        }
        for operation in operations {
            self.operator(operation, depth)?;
        }
        Ok(())
    }

    fn operator(&mut self, operation: &Operation, depth: usize) -> Result<()> {
        let operands = &operation.operands;
        match operation.operator.as_str() {
            "BX" => self.compatibility_depth = self.compatibility_depth.saturating_add(1),
            "EX" => self.compatibility_depth = self.compatibility_depth.saturating_sub(1),
            "q" => {
                if self.stack.len() >= MAX_GRAPHICS_STACK {
                    return Err(Error::LimitExceeded(format!(
                        "PDF graphics-state stack exceeds {MAX_GRAPHICS_STACK}"
                    )));
                }
                self.stack.push(SavedGraphicsState {
                    graphics: self.state.clone(),
                    text: self.text.saved_parameters(),
                });
            }
            "Q" => {
                if let Some(saved) = self.stack.pop() {
                    self.state = saved.graphics;
                    self.text.restore_parameters(saved.text);
                } else {
                    self.page.warn("PDF Q operator has no matching q");
                }
            }
            "cm" => {
                if let Some(matrix) = matrix_operands(operands) {
                    let previous = self.state.ctm;
                    let next = compose(previous, matrix);
                    let fill = self.state.fill.clone();
                    let stroke = self.state.stroke.clone();
                    self.state.fill = self.rebase_pattern_paint(&fill, previous, next);
                    self.state.stroke = self.rebase_pattern_paint(&stroke, previous, next);
                    self.state.ctm = next;
                }
            }
            "w" => self.state.line_width = number(operands.first()).unwrap_or(1.0).max(0.0),
            "J" => {
                self.state.line_cap = match integer(operands.first()).unwrap_or(0) {
                    1 => LineCap::Round,
                    2 => LineCap::Square,
                    _ => LineCap::Butt,
                };
            }
            "j" => {
                self.state.line_join = match integer(operands.first()).unwrap_or(0) {
                    1 => LineJoin::Round,
                    2 => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                };
            }
            "M" => self.state.miter_limit = number(operands.first()).unwrap_or(10.0).max(1.0),
            "d" => self.apply_dash(operands),
            "m" => self.path.command("M", &numbers(operands, 2)),
            "l" => self.path.command("L", &numbers(operands, 2)),
            "c" => self.path.command("C", &numbers(operands, 6)),
            "v" => {
                let values = numbers(operands, 4);
                if values.len() == 4
                    && let Some((current_x, current_y)) = self.path.current_point
                {
                    self.path.command(
                        "C",
                        &[
                            current_x, current_y, values[0], values[1], values[2], values[3],
                        ],
                    );
                }
            }
            "y" => {
                let values = numbers(operands, 4);
                if values.len() == 4 {
                    self.path.command(
                        "C",
                        &[
                            values[0], values[1], values[2], values[3], values[2], values[3],
                        ],
                    );
                }
            }
            "h" => self.path.command("Z", &[]),
            "re" => {
                let values = numbers(operands, 4);
                if values.len() == 4 {
                    let [x, y, width, height] = [values[0], values[1], values[2], values[3]];
                    self.path.command("M", &[x, y]);
                    self.path.command("L", &[x + width, y]);
                    self.path.command("L", &[x + width, y + height]);
                    self.path.command("L", &[x, y + height]);
                    self.path.command("Z", &[]);
                }
            }
            "W" => self.pending_clip_rule = Some("nonzero".into()),
            "W*" => self.pending_clip_rule = Some("evenodd".into()),
            "S" => self.paint_path(false, true, false, "nonzero"),
            "s" => self.paint_path(false, true, true, "nonzero"),
            "f" | "F" => self.paint_path(true, false, false, "nonzero"),
            "f*" => self.paint_path(true, false, false, "evenodd"),
            "B" => self.paint_path(true, true, false, "nonzero"),
            "B*" => self.paint_path(true, true, false, "evenodd"),
            "b" => self.paint_path(true, true, true, "nonzero"),
            "b*" => self.paint_path(true, true, true, "evenodd"),
            "n" => self.finish_clip_and_clear(),
            "g" => {
                self.state.fill_color_space = Object::Name(b"DeviceGray".to_vec());
                self.state.fill_pattern_clip = None;
                self.state.fill = gray_paint(
                    number(operands.first()).unwrap_or(0.0),
                    self.state.fill_alpha,
                )
            }
            "G" => {
                self.state.stroke_color_space = Object::Name(b"DeviceGray".to_vec());
                self.state.stroke_pattern_clip = None;
                self.state.stroke = gray_paint(
                    number(operands.first()).unwrap_or(0.0),
                    self.state.stroke_alpha,
                )
            }
            "rg" => {
                self.state.fill_color_space = Object::Name(b"DeviceRGB".to_vec());
                self.state.fill_pattern_clip = None;
                self.state.fill = rgb_paint(&numbers(operands, 3), self.state.fill_alpha);
            }
            "RG" => {
                self.state.stroke_color_space = Object::Name(b"DeviceRGB".to_vec());
                self.state.stroke_pattern_clip = None;
                self.state.stroke = rgb_paint(&numbers(operands, 3), self.state.stroke_alpha);
            }
            "k" => {
                self.state.fill_color_space = Object::Name(b"DeviceCMYK".to_vec());
                self.state.fill_pattern_clip = None;
                self.state.fill = cmyk_paint(&numbers(operands, 4), self.state.fill_alpha);
            }
            "K" => {
                self.state.stroke_color_space = Object::Name(b"DeviceCMYK".to_vec());
                self.state.stroke_pattern_clip = None;
                self.state.stroke = cmyk_paint(&numbers(operands, 4), self.state.stroke_alpha);
            }
            "cs" => {
                if let Some(name) = name(operands.first()) {
                    self.state.fill_color_space = self.resolve_color_space_name(name);
                    self.state.fill_pattern_clip = None;
                }
            }
            "CS" => {
                if let Some(name) = name(operands.first()) {
                    self.state.stroke_color_space = self.resolve_color_space_name(name);
                    self.state.stroke_pattern_clip = None;
                }
            }
            "sc" | "scn" => self.set_current_color(false, operands),
            "SC" | "SCN" => self.set_current_color(true, operands),
            "BT" => {
                self.text.text_matrix = IDENTITY;
                self.text.line_matrix = IDENTITY;
                self.text.pending_clips.clear();
            }
            "Tf" => {
                self.text.font_name = name(operands.first()).unwrap_or_default().to_vec();
                self.text.font_size = number(operands.get(1)).unwrap_or(12.0).abs();
            }
            "Tm" => {
                if let Some(matrix) = matrix_operands(operands) {
                    self.text.text_matrix = matrix;
                    self.text.line_matrix = matrix;
                }
            }
            "Td" => self.move_text(
                number(operands.first()).unwrap_or(0.0),
                number(operands.get(1)).unwrap_or(0.0),
            ),
            "TD" => {
                let y = number(operands.get(1)).unwrap_or(0.0);
                self.text.leading = -y;
                self.move_text(number(operands.first()).unwrap_or(0.0), y);
            }
            "T*" => self.move_text(0.0, -self.text.leading),
            "Tc" => self.text.character_spacing = number(operands.first()).unwrap_or(0.0),
            "Tw" => self.text.word_spacing = number(operands.first()).unwrap_or(0.0),
            "Tz" => self.text.horizontal_scale = number(operands.first()).unwrap_or(100.0) / 100.0,
            "TL" => self.text.leading = number(operands.first()).unwrap_or(0.0),
            "Ts" => self.text.rise = number(operands.first()).unwrap_or(0.0),
            "Tr" => self.text.rendering_mode = integer(operands.first()).unwrap_or(0),
            "Tj" => {
                if let Some(bytes) = string_bytes(operands.first()) {
                    self.show_text(bytes);
                }
            }
            "TJ" => self.show_text_array(operands.first()),
            "'" => {
                self.move_text(0.0, -self.text.leading);
                if let Some(bytes) = string_bytes(operands.first()) {
                    self.show_text(bytes);
                }
            }
            "\"" => {
                self.text.word_spacing = number(operands.first()).unwrap_or(0.0);
                self.text.character_spacing = number(operands.get(1)).unwrap_or(0.0);
                self.move_text(0.0, -self.text.leading);
                if let Some(bytes) = string_bytes(operands.get(2)) {
                    self.show_text(bytes);
                }
            }
            "Do" => {
                if let Some(name) = name(operands.first()) {
                    self.draw_xobject(name, depth)?;
                }
            }
            "gs" => {
                if let Some(name) = name(operands.first()) {
                    self.apply_ext_gstate(name)?;
                }
            }
            "sh" => {
                if let Some(name) = name(operands.first()) {
                    self.draw_shading(name)?;
                }
            }
            "BI" => {
                if let Some(Object::Stream(stream)) = operands.first() {
                    let inline_image = normalize_inline_image(stream.clone());
                    self.draw_image(&inline_image, b"inline")?;
                } else {
                    self.page.warn(
                        "PDF filtered inline image was skipped by the syntax decoder and needs native filter recovery",
                    );
                }
            }
            "ID" | "EI" => {}
            "ET" => self.apply_pending_text_clips(),
            "T_s" | "T_w" | "T_L" | "T_c" => {}
            unsupported
                if self.compatibility_depth == 0
                    && !matches!(
                        unsupported,
                        "ri" | "i" | "d0" | "d1" | "BMC" | "BDC" | "EMC" | "MP" | "DP"
                    ) =>
            {
                self.page
                    .warn(format!("PDF operator {unsupported} is not yet implemented"));
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_dash(&mut self, operands: &[Object]) {
        self.state.dash_array = operands
            .first()
            .and_then(|object| object.as_array().ok())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| number(Some(value)))
                    .collect()
            })
            .unwrap_or_default();
        self.state.dash_offset = number(operands.get(1)).unwrap_or(0.0);
    }

    fn resolve_color_space_name(&self, name: &[u8]) -> Object {
        match name {
            b"DeviceGray" | b"G" | b"DeviceRGB" | b"RGB" | b"DeviceCMYK" | b"CMYK" | b"Pattern" => {
                Object::Name(name.to_vec())
            }
            _ => self
                .lookup_named_resource(b"ColorSpace", name)
                .unwrap_or_else(|| Object::Name(name.to_vec())),
        }
    }

    fn resolve_color_space_object(&self, object: &Object) -> Object {
        let resolved = self
            .document
            .dereference(object)
            .ok()
            .map(|(_, value)| value)
            .unwrap_or(object);
        if let Object::Name(name) = resolved {
            self.resolve_color_space_name(name)
        } else {
            resolved.clone()
        }
    }

    fn set_current_color(&mut self, stroke: bool, operands: &[Object]) {
        let color_space = if stroke {
            self.state.stroke_color_space.clone()
        } else {
            self.state.fill_color_space.clone()
        };
        if pdf_color_space_is_pattern(self.document, &color_space) {
            let Some(pattern_name) = operands.iter().rev().find_map(|value| value.as_name().ok())
            else {
                self.page
                    .warn("PDF Pattern color selection has no pattern name");
                return;
            };
            let opacity = if stroke {
                self.state.stroke_alpha
            } else {
                self.state.fill_alpha
            };
            let Some((paint, pattern_clip)) =
                self.pattern_paint(pattern_name, &color_space, operands, opacity)
            else {
                return;
            };
            if stroke {
                self.state.stroke = paint;
                self.state.stroke_pattern_clip = pattern_clip;
            } else {
                self.state.fill = paint;
                self.state.fill_pattern_clip = pattern_clip;
            }
            return;
        }
        let components = numbers(operands, usize::MAX);
        let opacity = if stroke {
            self.state.stroke_alpha
        } else {
            self.state.fill_alpha
        };
        let paint = Paint::Solid {
            color: components_to_color(self.document, Some(&color_space), &components),
            opacity,
        };
        if stroke {
            self.state.stroke = paint;
            self.state.stroke_pattern_clip = None;
        } else {
            self.state.fill = paint;
            self.state.fill_pattern_clip = None;
        }
    }

    fn rebase_pattern_paint(&mut self, paint: &Paint, previous: Matrix, next: Matrix) -> Paint {
        let Paint::PatternRef { id, opacity } = paint else {
            return paint.clone();
        };
        let Some(inverse) = inverse_matrix(next) else {
            return paint.clone();
        };
        let Some(mut definition) = self
            .page
            .patterns
            .iter()
            .find(|pattern| pattern.id == *id)
            .cloned()
        else {
            return paint.clone();
        };
        definition.transform = compose(inverse, compose(previous, definition.transform));
        definition.id = pattern_variant_id(id, definition.transform);
        let variant_id = definition.id.clone();
        if !self
            .page
            .patterns
            .iter()
            .any(|pattern| pattern.id == variant_id)
        {
            self.page.patterns.push(definition);
        }
        Paint::PatternRef {
            id: variant_id,
            opacity: *opacity,
        }
    }

    fn pattern_paint(
        &mut self,
        pattern_name: &[u8],
        pattern_color_space: &Object,
        operands: &[Object],
        opacity: f64,
    ) -> Option<(Paint, Option<PatternClip>)> {
        let Some(pattern) = self.lookup_named_resource(b"Pattern", pattern_name) else {
            self.page.warn(format!(
                "PDF pattern {} was not found",
                String::from_utf8_lossy(pattern_name)
            ));
            return None;
        };
        let pattern_dictionary = match &pattern {
            Object::Dictionary(dictionary) => dictionary,
            Object::Stream(stream) => &stream.dict,
            _ => {
                self.page.warn("PDF Pattern resource is not a dictionary");
                return None;
            }
        };
        let pattern_type = pattern_dictionary
            .get(b"PatternType")
            .and_then(Object::as_i64)
            .unwrap_or(0);
        if pattern_type == 1 {
            return self.tiling_pattern_paint(pattern_name, pattern_color_space, operands, opacity);
        }
        if pattern_type != 2 {
            self.page.warn(format!(
                "PDF pattern {} has unsupported PatternType {pattern_type}",
                String::from_utf8_lossy(pattern_name)
            ));
            return None;
        }
        let shading_object = pattern_dictionary.get(b"Shading").ok()?;
        let (_, shading_object) = self.document.dereference(shading_object).ok()?;
        let shading_dictionary = match shading_object {
            Object::Dictionary(dictionary) => dictionary,
            Object::Stream(stream) => &stream.dict,
            _ => {
                self.page
                    .warn("PDF shading pattern has no shading dictionary");
                return None;
            }
        };
        let shading_type = shading_dictionary
            .get(b"ShadingType")
            .and_then(Object::as_i64)
            .unwrap_or(0);
        if !matches!(shading_type, 2 | 3) {
            self.page.warn(format!(
                "PDF shading PatternType 2 uses unsupported shading type {shading_type}"
            ));
            return None;
        }
        let coordinates = shading_dictionary
            .get(b"Coords")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, 6))
            .unwrap_or_default();
        let required = if shading_type == 2 { 4 } else { 6 };
        if coordinates.len() != required {
            self.page
                .warn("PDF shading PatternType 2 has invalid /Coords");
            return None;
        }
        let function = shading_dictionary.get(b"Function").ok()?;
        let domain = shading_dictionary
            .get(b"Domain")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, 2))
            .filter(|values| values.len() == 2)
            .unwrap_or_else(|| vec![0.0, 1.0]);
        let color_space = shading_dictionary.get(b"ColorSpace").ok();
        let extend = shading_dictionary
            .get(b"Extend")
            .and_then(Object::as_array)
            .ok()
            .map(|values| {
                [
                    values
                        .first()
                        .and_then(|value| value.as_bool().ok())
                        .unwrap_or(false),
                    values
                        .get(1)
                        .and_then(|value| value.as_bool().ok())
                        .unwrap_or(false),
                ]
            })
            .unwrap_or([false, false]);
        let opacity = opacity.clamp(0.0, 1.0);
        let background_components = shading_dictionary
            .get(b"Background")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, usize::MAX));
        let background_rgb = background_components
            .as_ref()
            .and_then(|components| components_to_rgb(self.document, color_space, components, 0));
        let background = background_components
            .as_ref()
            .map(|values| components_to_color(self.document, color_space, values));
        let layered_opacity = if background_rgb.is_some() {
            opacity * (2.0 - opacity)
        } else {
            opacity
        };
        let mut stops = Vec::with_capacity(35);
        for index in 0..=32 {
            let offset = f64::from(index) / 32.0;
            let input = domain[0] + (domain[1] - domain[0]) * offset;
            let Some(components) = evaluate_pdf_function(self.document, function, input, 0) else {
                self.page
                    .warn("PDF shading pattern uses an unsupported function");
                return None;
            };
            let color = if let Some(background_rgb) = background_rgb {
                let source = components_to_rgb(self.document, color_space, &components, 0)
                    .unwrap_or_else(|| default_rgb_components(&components));
                let denominator = (2.0 - opacity).max(1e-12);
                let layered = std::array::from_fn(|channel| {
                    (source[channel] + (1.0 - opacity) * background_rgb[channel]) / denominator
                });
                let [red, green, blue] = rgb_to_bytes(layered);
                format!("#{red:02X}{green:02X}{blue:02X}")
            } else {
                components_to_color(self.document, color_space, &components)
            };
            stops.push(GradientStop {
                offset,
                color,
                opacity: layered_opacity,
            });
        }
        apply_gradient_extension_with_background(
            &mut stops,
            extend,
            background.as_deref(),
            opacity,
        );
        let pattern_matrix = pattern_dictionary
            .get(b"Matrix")
            .and_then(Object::as_array)
            .ok()
            .and_then(|values| matrix_operands(values))
            .unwrap_or(IDENTITY);
        let pattern_matrix = inverse_matrix(self.state.ctm)
            .map(|inverse| compose(inverse, compose(self.content_base_ctm, pattern_matrix)))
            .unwrap_or(pattern_matrix);
        let pattern_clip = shading_dictionary
            .get(b"BBox")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, 4))
            .filter(|values| values.len() == 4)
            .map(|bbox| PatternClip {
                bbox: [bbox[0], bbox[1], bbox[2], bbox[3]],
                matrix: pattern_matrix,
            });
        if shading_type == 2 {
            let (x1, y1, x2, y2) = transformed_axial_axis(
                pattern_matrix,
                coordinates[0],
                coordinates[1],
                coordinates[2],
                coordinates[3],
            )?;
            return Some((
                Paint::LinearGradient(Box::new(LinearGradient {
                    x1,
                    y1,
                    x2,
                    y2,
                    stops,
                })),
                pattern_clip,
            ));
        }
        let mut first_radius = coordinates[2].max(0.0);
        let mut second_radius = coordinates[5].max(0.0);
        let mut first_x = coordinates[0];
        let mut first_y = coordinates[1];
        let mut second_x = coordinates[3];
        let mut second_y = coordinates[4];
        let center_distance = (second_x - first_x).hypot(second_y - first_y);
        if center_distance + first_radius.min(second_radius)
            > first_radius.max(second_radius) + 1e-9
        {
            self.page
                .warn("PDF noncontained radial shading pattern requires field tessellation");
            return None;
        }
        if first_radius > second_radius {
            std::mem::swap(&mut first_radius, &mut second_radius);
            std::mem::swap(&mut first_x, &mut second_x);
            std::mem::swap(&mut first_y, &mut second_y);
            for stop in &mut stops {
                stop.offset = 1.0 - stop.offset;
            }
            stops.reverse();
        }
        Some((
            Paint::RadialGradient(Box::new(RadialGradient {
                fx: first_x,
                fy: first_y,
                fr: first_radius,
                cx: second_x,
                cy: second_y,
                radius: second_radius.max(1e-9),
                transform: pattern_matrix,
                stops,
            })),
            pattern_clip,
        ))
    }

    fn tiling_pattern_paint(
        &mut self,
        pattern_name: &[u8],
        pattern_color_space: &Object,
        operands: &[Object],
        opacity: f64,
    ) -> Option<(Paint, Option<PatternClip>)> {
        let Some((object_id, pattern)) =
            self.lookup_named_resource_with_id(b"Pattern", pattern_name)
        else {
            self.page.warn(format!(
                "PDF tiling pattern {} was not found",
                String::from_utf8_lossy(pattern_name)
            ));
            return None;
        };
        let Object::Stream(stream) = pattern else {
            self.page
                .warn("PDF tiling PatternType 1 has no content stream");
            return None;
        };
        let bbox = stream
            .dict
            .get(b"BBox")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, 4))
            .filter(|values| values.len() == 4)?;
        let x_step = stream
            .dict
            .get(b"XStep")
            .ok()
            .and_then(|value| number(Some(value)))?;
        let y_step = stream
            .dict
            .get(b"YStep")
            .ok()
            .and_then(|value| number(Some(value)))?;
        if x_step.abs() <= 1e-12 || y_step.abs() <= 1e-12 {
            self.page.warn("PDF tiling pattern has a zero XStep/YStep");
            return None;
        }
        let pattern_matrix = stream
            .dict
            .get(b"Matrix")
            .and_then(Object::as_array)
            .ok()
            .and_then(|values| matrix_operands(values))
            .unwrap_or(IDENTITY);
        let pattern_matrix = inverse_matrix(self.state.ctm)
            .map(|inverse| compose(inverse, compose(self.content_base_ctm, pattern_matrix)))
            .unwrap_or(pattern_matrix);
        let resources = stream
            .dict
            .get_deref(b"Resources", self.document)
            .and_then(Object::as_dict)
            .ok()
            .cloned();
        let paint_type = stream
            .dict
            .get(b"PaintType")
            .and_then(Object::as_i64)
            .unwrap_or(1);
        let base_color_space = (paint_type == 2)
            .then(|| match pattern_color_space {
                Object::Array(values)
                    if values.first().and_then(|value| value.as_name().ok())
                        == Some(b"Pattern") =>
                {
                    values.get(1)
                }
                _ => None,
            })
            .flatten();
        let components = numbers(operands, usize::MAX);
        let base_color = (paint_type == 2)
            .then(|| components_to_color(self.document, base_color_space, &components));
        let resource_key = object_id.map_or_else(
            || {
                pattern_name
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect()
            },
            |object_id| format!("{}-{}", object_id.0, object_id.1),
        );
        let transform_key = pattern_matrix
            .iter()
            .map(|value| format!("{:x}", value.to_bits()))
            .collect::<Vec<_>>()
            .join("-");
        let resource_key = format!("{resource_key}-{transform_key}");
        let id = base_color.as_ref().map_or_else(
            || format!("pdf-pattern-{resource_key}"),
            |color| {
                format!(
                    "pdf-pattern-{resource_key}-{}",
                    color.trim_start_matches('#')
                )
            },
        );
        if self.page.patterns.iter().any(|pattern| pattern.id == id) {
            return Some((
                Paint::PatternRef {
                    id,
                    opacity: opacity.clamp(0.0, 1.0),
                },
                None,
            ));
        }
        if let Some(object_id) = object_id
            && !self.visited_patterns.insert(object_id)
        {
            self.page.warn("cyclic PDF tiling pattern was skipped");
            return None;
        }
        let mut pattern_page = Page::new(
            self.page.number,
            x_step.abs().max(1.0),
            y_step.abs().max(1.0),
            "pdf-pattern",
        );
        let mut pattern_fonts = self.fonts.clone();
        pattern_fonts.extend(build_resource_font_decoders(
            self.document,
            resources.as_ref(),
            self.content_limit,
            &mut pattern_page,
        ));
        let mut pattern_state = GraphicsState::default();
        if let Some(base_color) = base_color {
            let paint = Paint::Solid {
                color: base_color,
                opacity: 1.0,
            };
            pattern_state.fill = paint.clone();
            pattern_state.stroke = paint;
            pattern_state.fill_color_space = base_color_space
                .cloned()
                .unwrap_or_else(|| Object::Name(b"DeviceGray".to_vec()));
            pattern_state.stroke_color_space = pattern_state.fill_color_space.clone();
        }
        let content = match stream.decompressed_content_with_limit(self.content_limit) {
            Ok(content) => content,
            Err(error) => {
                if let Some(object_id) = object_id {
                    self.visited_patterns.remove(&object_id);
                }
                self.page
                    .warn(format!("PDF tiling pattern could not be decoded: {error}"));
                return None;
            }
        };
        let decoded = match Content::decode(&content) {
            Ok(decoded) => decoded,
            Err(error) => {
                if let Some(object_id) = object_id {
                    self.visited_patterns.remove(&object_id);
                }
                self.page
                    .warn(format!("PDF tiling pattern content is invalid: {error}"));
                return None;
            }
        };
        let mut pattern_interpreter = Interpreter {
            document: self.document,
            page: &mut pattern_page,
            page_matrix: IDENTITY,
            page_id: self.page_id,
            fonts: pattern_fonts,
            resources: resources.into_iter().collect(),
            state: pattern_state,
            content_base_ctm: IDENTITY,
            stack: Vec::new(),
            path: PathBuilder::default(),
            pending_clip_rule: None,
            text: TextState::default(),
            node_counter: self.node_counter,
            clip_counter: self.clip_counter,
            mask_counter: self.mask_counter,
            compatibility_depth: self.compatibility_depth,
            visited_forms: self.visited_forms.clone(),
            visited_patterns: self.visited_patterns.clone(),
            mesh_output_count: Rc::clone(&self.mesh_output_count),
            content_limit: self.content_limit,
            outline_embedded_pdf_text: self.outline_embedded_pdf_text,
        };
        let interpretation = pattern_interpreter.interpret(&decoded.operations, 1);
        self.node_counter = pattern_interpreter.node_counter;
        self.clip_counter = pattern_interpreter.clip_counter;
        self.mask_counter = pattern_interpreter.mask_counter;
        drop(pattern_interpreter);
        if let Some(object_id) = object_id {
            self.visited_patterns.remove(&object_id);
        }
        if let Err(error) = interpretation {
            self.page.warn(format!(
                "PDF tiling pattern could not be interpreted: {error}"
            ));
            return None;
        }
        self.clip_counter += 1;
        let content_clip_id = format!(
            "pdf-pattern-content-clip-{}-{}",
            self.page.number, self.clip_counter
        );
        pattern_page.clips.push(crate::ir::ClipPath {
            id: content_clip_id.clone(),
            d: rectangle_path(bbox[0], bbox[1], bbox[2] - bbox[0], bbox[3] - bbox[1]),
            transform: IDENTITY,
            fill_rule: "nonzero".into(),
            parent_id: None,
            additional_paths: Vec::new(),
        });
        let mut nodes = pattern_page.nodes;
        if !nodes.is_empty() {
            nodes = vec![Node::Group {
                id: format!("{content_clip_id}-group"),
                nodes,
                transform: IDENTITY,
                opacity: 1.0,
                clip_id: Some(content_clip_id),
                meta: SourceMeta {
                    kind: "tiling-pattern-cell".into(),
                    ..SourceMeta::default()
                },
            }];
        }
        self.page.clips.extend(pattern_page.clips);
        self.page.masks.extend(pattern_page.masks);
        self.page.patterns.extend(pattern_page.patterns);
        for warning in pattern_page.warnings {
            self.page.warn(format!("tiling pattern: {warning}"));
        }
        self.page.patterns.push(TilingPatternDefinition {
            id: id.clone(),
            x: 0.0,
            y: 0.0,
            width: x_step.abs(),
            height: y_step.abs(),
            transform: pattern_matrix,
            nodes,
        });
        Some((
            Paint::PatternRef {
                id,
                opacity: opacity.clamp(0.0, 1.0),
            },
            None,
        ))
    }

    fn selected_pattern_clip(&mut self, fill: bool, stroke: bool) -> Option<PatternClip> {
        let fill_clip = fill.then(|| self.state.fill_pattern_clip.clone()).flatten();
        let stroke_clip = stroke
            .then(|| self.state.stroke_pattern_clip.clone())
            .flatten();
        if fill_clip.is_some() && stroke_clip.is_some() && fill_clip != stroke_clip {
            self.page.warn(
                "PDF fill and stroke use different shading-pattern BBoxes; one shared clip is used",
            );
        }
        fill_clip.or(stroke_clip)
    }

    fn install_pattern_bbox_clip(&mut self, pattern_clip: Option<PatternClip>) -> Option<String> {
        let Some(pattern_clip) = pattern_clip else {
            return self.state.clip_id.clone();
        };
        self.clip_counter += 1;
        let id = format!(
            "pdf-pattern-clip-{}-{}",
            self.page.number, self.clip_counter
        );
        self.page.clips.push(crate::ir::ClipPath {
            id: id.clone(),
            d: rectangle_path(
                pattern_clip.bbox[0],
                pattern_clip.bbox[1],
                pattern_clip.bbox[2] - pattern_clip.bbox[0],
                pattern_clip.bbox[3] - pattern_clip.bbox[1],
            ),
            transform: compose(
                self.page_matrix,
                compose(self.state.ctm, pattern_clip.matrix),
            ),
            fill_rule: "nonzero".into(),
            parent_id: self.state.clip_id.clone(),
            additional_paths: Vec::new(),
        });
        Some(id)
    }

    fn paint_path(&mut self, fill: bool, stroke: bool, close: bool, fill_rule: &str) {
        if close {
            self.path.command("Z", &[]);
        }
        if !self.path.has_content {
            return;
        }
        let d = self.path.take();
        self.install_clip(&d);
        let pattern_clip = self.selected_pattern_clip(fill, stroke);
        let clip_id = self.install_pattern_bbox_clip(pattern_clip);
        self.node_counter += 1;
        self.page.nodes.push(Node::Path {
            id: format!("pdf-path-{}-{}", self.page.number, self.node_counter),
            d,
            fill_rule: fill_rule.into(),
            fill: if fill {
                self.state.fill.clone()
            } else {
                Paint::None
            },
            stroke: Stroke {
                paint: if stroke {
                    self.state.stroke.clone()
                } else {
                    Paint::None
                },
                width: self.state.line_width,
                line_cap: self.state.line_cap,
                line_join: self.state.line_join,
                miter_limit: self.state.miter_limit,
                dash_array: self.state.dash_array.clone(),
                dash_offset: self.state.dash_offset,
            },
            transform: compose(self.page_matrix, self.state.ctm),
            clip_id,
            meta: SourceMeta {
                kind: "vector".into(),
                source_id: format!("page:{}:operator-path", self.page.number),
                blend_mode: self.state.blend_mode.clone(),
                mask_id: self.state.mask_id.clone().unwrap_or_default(),
                alpha_is_shape: self.state.alpha_is_shape,
                ..SourceMeta::default()
            },
        });
    }

    fn finish_clip_and_clear(&mut self) {
        if self.path.has_content {
            let d = self.path.take();
            self.install_clip(&d);
        } else {
            self.path.clear();
        }
    }

    fn install_clip(&mut self, d: &str) {
        let Some(rule) = self.pending_clip_rule.take() else {
            return;
        };
        let parent_id = self.state.clip_id.clone();
        self.clip_counter += 1;
        let id = format!("pdf-clip-{}-{}", self.page.number, self.clip_counter);
        self.page.clips.push(crate::ir::ClipPath {
            id: id.clone(),
            d: d.into(),
            transform: compose(self.page_matrix, self.state.ctm),
            fill_rule: rule,
            parent_id,
            additional_paths: Vec::new(),
        });
        self.state.clip_id = Some(id);
    }

    fn apply_pending_text_clips(&mut self) {
        let mut pending = std::mem::take(&mut self.text.pending_clips).into_iter();
        let Some(first) = pending.next() else {
            return;
        };
        let parent_id = self.state.clip_id.clone();
        self.clip_counter += 1;
        let id = format!("pdf-text-clip-{}-{}", self.page.number, self.clip_counter);
        self.page.clips.push(crate::ir::ClipPath {
            id: id.clone(),
            d: first.d,
            transform: first.transform,
            fill_rule: "nonzero".into(),
            parent_id,
            additional_paths: pending
                .map(|pending| crate::ir::ClipMember {
                    d: pending.d,
                    transform: pending.transform,
                    fill_rule: "nonzero".into(),
                })
                .collect(),
        });
        self.state.clip_id = Some(id);
    }

    fn move_text(&mut self, x: f64, y: f64) {
        let translation = [1.0, 0.0, 0.0, 1.0, x, y];
        self.text.line_matrix = compose(self.text.line_matrix, translation);
        self.text.text_matrix = self.text.line_matrix;
    }

    fn show_text(&mut self, bytes: &[u8]) {
        let has_fill = matches!(self.text.rendering_mode, 0 | 2 | 4 | 6);
        let has_stroke = matches!(self.text.rendering_mode, 1 | 2 | 5 | 6);
        let pattern_clip = self.selected_pattern_clip(has_fill, has_stroke);
        let paint_clip_id = self.install_pattern_bbox_clip(pattern_clip);
        let decoder = self.fonts.get(&self.text.font_name);
        let (text, replacement) = decoder.map_or_else(
            || (bytes.iter().map(|byte| char::from(*byte)).collect(), true),
            |decoder| decoder.decode(bytes),
        );
        if replacement {
            self.page.warn(format!(
                "font {} required replacement glyphs while decoding text",
                String::from_utf8_lossy(&self.text.font_name)
            ));
        }
        if self.text.rendering_mode == 3 {
            self.advance_text_bytes(bytes, &text);
            return;
        }
        if let Some(type3) = decoder.and_then(|decoder| decoder.type3.clone()) {
            if self.text.rendering_mode != 0 {
                self.page.warn(format!(
                    "PDF Type3 text rendering mode {} is approximated by its CharProc paint operators",
                    self.text.rendering_mode
                ));
            }
            self.render_type3_text(bytes, &text, &type3);
            self.advance_text_bytes(bytes, &text);
            return;
        }
        self.node_counter += 1;
        let mut font_family = decoder
            .map(|decoder| decoder.family.clone())
            .unwrap_or_else(|| "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into());
        let stable_symbol = stable_pdf_symbol_fallback(&font_family, &text);
        font_family = pdf_font_stack(&font_family);
        if stable_symbol.is_some() {
            font_family = "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into();
        }
        let compensation = [
            self.text.horizontal_scale,
            0.0,
            0.0,
            -1.0,
            0.0,
            self.text.rise,
        ];
        let transform = compose(
            self.page_matrix,
            compose(self.state.ctm, compose(self.text.text_matrix, compensation)),
        );
        let mut outline_failure = None;
        let explicit_fidelity_outline = decoder.is_some_and(|decoder| {
            !decoder.requires_outline
                && should_outline_pdf_text(
                    decoder.requires_outline,
                    decoder.font_data.is_some(),
                    self.outline_embedded_pdf_text,
                )
                && stable_symbol.is_none()
        });
        let outline_path = decoder
            .filter(|decoder| {
                should_outline_pdf_text(
                    decoder.requires_outline,
                    decoder.font_data.is_some(),
                    self.outline_embedded_pdf_text,
                ) && stable_symbol.is_none()
            })
            .and_then(|decoder| {
                match decoder.outline_path(
                    bytes,
                    self.text.character_spacing / self.text.font_size.max(1e-12),
                    self.text.word_spacing / self.text.font_size.max(1e-12),
                ) {
                    Ok(path) => Some(path),
                    Err(reason) => {
                        outline_failure = Some(reason);
                        None
                    }
                }
            });
        if let Some(path_data) = outline_path {
            if path_data.is_empty() {
                self.advance_text_bytes(bytes, &text);
                return;
            }
            let glyph_matrix = [
                self.text.font_size * self.text.horizontal_scale,
                0.0,
                0.0,
                self.text.font_size,
                0.0,
                self.text.rise,
            ];
            let glyph_space_scale =
                matrix_maximum_scale(compose(self.text.text_matrix, glyph_matrix)).max(1e-12);
            let glyph_transform = compose(
                self.page_matrix,
                compose(self.state.ctm, compose(self.text.text_matrix, glyph_matrix)),
            );
            if matches!(self.text.rendering_mode, 4..=7) {
                self.text.pending_clips.push(PendingTextClip {
                    d: path_data.clone(),
                    transform: glyph_transform,
                });
            }
            if self.text.rendering_mode == 7 {
                self.advance_text_bytes(bytes, &text);
                return;
            }
            self.page.nodes.push(Node::Path {
                id: format!(
                    "pdf-text-outline-{}-{}",
                    self.page.number, self.node_counter
                ),
                d: path_data,
                fill_rule: "nonzero".into(),
                fill: if has_fill {
                    self.state.fill.clone()
                } else {
                    Paint::None
                },
                stroke: Stroke {
                    paint: if has_stroke {
                        self.state.stroke.clone()
                    } else {
                        Paint::None
                    },
                    width: self.state.line_width / glyph_space_scale,
                    line_cap: self.state.line_cap,
                    line_join: self.state.line_join,
                    miter_limit: self.state.miter_limit,
                    dash_array: self
                        .state
                        .dash_array
                        .iter()
                        .map(|value| value / glyph_space_scale)
                        .collect(),
                    dash_offset: self.state.dash_offset / glyph_space_scale,
                },
                transform: glyph_transform,
                clip_id: paint_clip_id.clone(),
                meta: SourceMeta {
                    kind: "text-outline".into(),
                    source_id: format!(
                        "page:{}:font:{}",
                        self.page.number,
                        String::from_utf8_lossy(&self.text.font_name)
                    ),
                    semantic_role: "text".into(),
                    alt_text: text.clone(),
                    blend_mode: self.state.blend_mode.clone(),
                    mask_id: self.state.mask_id.clone().unwrap_or_default(),
                    alpha_is_shape: self.state.alpha_is_shape,
                    ..SourceMeta::default()
                },
            });
            if explicit_fidelity_outline {
                self.page.warn(format!(
                    "embedded font {} was outlined by explicit fidelity mode; verify the source font's outline and embedding rights",
                    decoder.map_or("unknown", |decoder| decoder.family.as_str())
                ));
            }
            self.advance_text_bytes(bytes, &text);
            return;
        }
        if decoder.is_some_and(|decoder| decoder.requires_outline) && stable_symbol.is_none() {
            self.page.warn(format!(
                "embedded custom font {} is emitted as editable fallback text: {}",
                decoder.map_or("unknown", |decoder| decoder.family.as_str()),
                outline_failure.unwrap_or("glyph outline recovery is pending")
            ));
        }
        if self.text.rendering_mode > 3 {
            self.page.warn(format!(
                "PDF text rendering mode {} requires an unavailable glyph-outline clip",
                self.text.rendering_mode
            ));
            if self.text.rendering_mode == 7 {
                self.advance_text_bytes(bytes, &text);
                return;
            }
        }
        let text_space_scale =
            matrix_maximum_scale(compose(self.text.text_matrix, compensation)).max(1e-12);
        let horizontal_scale = self.text.horizontal_scale.abs().max(1e-12);
        let glyph_width = decoder.map_or(bytes.len() as f64 * 0.5, |decoder| decoder.width(bytes));
        let character_count =
            decoder.map_or(bytes.len(), |decoder| decoder.code_count(bytes)) as f64;
        let spaces = text.chars().filter(|character| *character == ' ').count() as f64;
        let target_advance = glyph_width * self.text.font_size
            + (character_count * self.text.character_spacing + spaces * self.text.word_spacing)
                / horizontal_scale;
        self.page.nodes.push(Node::Text {
            id: format!("pdf-text-{}-{}", self.page.number, self.node_counter),
            x: 0.0,
            y: 0.0,
            runs: vec![TextRun {
                text: stable_symbol.unwrap_or(&text).to_owned(),
                font_family,
                font_size: self.text.font_size,
                bold: decoder.is_some_and(|decoder| decoder.bold),
                italic: decoder.is_some_and(|decoder| decoder.italic),
                fill: if has_fill {
                    self.state.fill.clone()
                } else {
                    Paint::None
                },
                baseline_shift: 0.0,
                glyph_x_offsets: decoder
                    .and_then(|decoder| {
                        decoder.glyph_x_offsets(
                            bytes,
                            self.text.font_size,
                            self.text.character_spacing,
                            self.text.word_spacing,
                            self.text.horizontal_scale,
                        )
                    })
                    .filter(|offsets| {
                        offsets.len() == stable_symbol.unwrap_or(&text).chars().count()
                    })
                    .unwrap_or_default(),
                target_advance: (target_advance.is_finite() && target_advance > 0.0)
                    .then_some(target_advance),
            }],
            anchor: TextAnchor::Start,
            transform,
            opacity: 1.0,
            stroke: Stroke {
                paint: if has_stroke {
                    self.state.stroke.clone()
                } else {
                    Paint::None
                },
                width: self.state.line_width / text_space_scale,
                line_cap: self.state.line_cap,
                line_join: self.state.line_join,
                miter_limit: self.state.miter_limit,
                dash_array: self
                    .state
                    .dash_array
                    .iter()
                    .map(|value| value / text_space_scale)
                    .collect(),
                dash_offset: self.state.dash_offset / text_space_scale,
            },
            clip_id: paint_clip_id,
            meta: SourceMeta {
                kind: "text".into(),
                source_id: format!(
                    "page:{}:font:{}",
                    self.page.number,
                    String::from_utf8_lossy(&self.text.font_name)
                ),
                semantic_role: "text".into(),
                alt_text: text.clone(),
                blend_mode: self.state.blend_mode.clone(),
                mask_id: self.state.mask_id.clone().unwrap_or_default(),
                alpha_is_shape: self.state.alpha_is_shape,
                ..SourceMeta::default()
            },
        });
        self.advance_text_bytes(bytes, &text);
    }

    fn render_type3_text(&mut self, bytes: &[u8], text: &str, font: &Type3Font) {
        let Some(decoder) = self.fonts.get(&self.text.font_name) else {
            return;
        };
        let widths = decoder.widths.clone();
        let default_width = decoder.default_width;
        let code_width = |code: u8| {
            widths
                .get(&u32::from(code))
                .copied()
                .unwrap_or(default_width)
                / 1_000.0
        };
        let mut x_offset = 0.0;
        let mut glyph_nodes = Vec::new();
        for code in bytes {
            let Some(stream) = font.glyphs.get(code) else {
                if !char::from(*code).is_whitespace() {
                    self.page.warn(format!(
                        "PDF Type3 font {} has no CharProc for code {code}",
                        String::from_utf8_lossy(&self.text.font_name)
                    ));
                }
                x_offset += code_width(*code);
                continue;
            };
            let glyph_matrix = [
                self.text.font_size * self.text.horizontal_scale,
                0.0,
                0.0,
                self.text.font_size,
                0.0,
                self.text.rise,
            ];
            let placement = [1.0, 0.0, 0.0, 1.0, x_offset, 0.0];
            let total = compose(
                self.page_matrix,
                compose(
                    self.state.ctm,
                    compose(
                        self.text.text_matrix,
                        compose(glyph_matrix, compose(placement, font.font_matrix)),
                    ),
                ),
            );
            let mut glyph_page = Page::new(
                self.page.number,
                self.page.width,
                self.page.height,
                "pdf-type3-glyph",
            );
            let mut glyph_fonts = self.fonts.clone();
            glyph_fonts.extend(build_resource_font_decoders(
                self.document,
                font.resources.as_ref(),
                self.content_limit,
                &mut glyph_page,
            ));
            let content = match stream.decompressed_content_with_limit(self.content_limit) {
                Ok(content) => content,
                Err(error) => {
                    self.page.warn(format!(
                        "PDF Type3 glyph stream could not be decoded: {error}"
                    ));
                    continue;
                }
            };
            let decoded = match Content::decode(&content) {
                Ok(decoded) => decoded,
                Err(error) => {
                    self.page
                        .warn(format!("PDF Type3 glyph content is invalid: {error}"));
                    continue;
                }
            };
            let mut initial_state = self.state.clone();
            initial_state.ctm = IDENTITY;
            initial_state.clip_id = None;
            initial_state.blend_mode = "normal".into();
            initial_state.mask_id = None;
            let mut glyph_interpreter = Interpreter {
                document: self.document,
                page: &mut glyph_page,
                page_matrix: total,
                page_id: self.page_id,
                fonts: glyph_fonts,
                resources: font.resources.clone().into_iter().collect(),
                state: initial_state,
                content_base_ctm: IDENTITY,
                stack: Vec::new(),
                path: PathBuilder::default(),
                pending_clip_rule: None,
                text: TextState::default(),
                node_counter: self.node_counter,
                clip_counter: self.clip_counter,
                mask_counter: self.mask_counter,
                compatibility_depth: self.compatibility_depth,
                visited_forms: self.visited_forms.clone(),
                visited_patterns: self.visited_patterns.clone(),
                mesh_output_count: Rc::clone(&self.mesh_output_count),
                content_limit: self.content_limit,
                outline_embedded_pdf_text: self.outline_embedded_pdf_text,
            };
            let interpretation = glyph_interpreter.interpret(&decoded.operations, 1);
            self.node_counter = glyph_interpreter.node_counter;
            self.clip_counter = glyph_interpreter.clip_counter;
            self.mask_counter = glyph_interpreter.mask_counter;
            drop(glyph_interpreter);
            if let Err(error) = interpretation {
                self.page
                    .warn(format!("PDF Type3 glyph could not be interpreted: {error}"));
            }
            glyph_nodes.append(&mut glyph_page.nodes);
            self.page.clips.append(&mut glyph_page.clips);
            self.page.masks.append(&mut glyph_page.masks);
            self.page.patterns.append(&mut glyph_page.patterns);
            for warning in glyph_page.warnings {
                self.page.warn(format!("Type3 glyph: {warning}"));
            }
            let word_spacing = if *code == b' ' {
                self.text.word_spacing / self.text.font_size.max(1e-12)
            } else {
                0.0
            };
            x_offset += code_width(*code)
                + self.text.character_spacing / self.text.font_size.max(1e-12)
                + word_spacing;
        }
        if glyph_nodes.is_empty() {
            return;
        }
        self.node_counter += 1;
        self.page.nodes.push(Node::Group {
            id: format!("pdf-type3-text-{}-{}", self.page.number, self.node_counter),
            nodes: glyph_nodes,
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: self.state.clip_id.clone(),
            meta: SourceMeta {
                kind: "type3-text".into(),
                source_id: format!(
                    "page:{}:font:{}",
                    self.page.number,
                    String::from_utf8_lossy(&self.text.font_name)
                ),
                semantic_role: "text".into(),
                alt_text: text.into(),
                blend_mode: self.state.blend_mode.clone(),
                mask_id: self.state.mask_id.clone().unwrap_or_default(),
                alpha_is_shape: self.state.alpha_is_shape,
                ..SourceMeta::default()
            },
        });
    }

    fn show_text_array(&mut self, operand: Option<&Object>) {
        let Some(array) = operand.and_then(|object| object.as_array().ok()) else {
            return;
        };
        for item in array {
            if let Some(bytes) = string_bytes(Some(item)) {
                self.show_text(bytes);
            } else if let Some(adjustment) = number(Some(item)) {
                self.advance_text(
                    "",
                    -adjustment / 1_000.0 * self.text.font_size * self.text.horizontal_scale,
                );
            }
        }
    }

    fn advance_text(&mut self, text: &str, extra: f64) {
        let character_count = text.chars().count() as f64;
        let spaces = text.chars().filter(|character| *character == ' ').count() as f64;
        let advance = character_count * self.text.font_size * 0.5 * self.text.horizontal_scale
            + character_count * self.text.character_spacing
            + spaces * self.text.word_spacing
            + extra;
        self.text.text_matrix = compose(self.text.text_matrix, [1.0, 0.0, 0.0, 1.0, advance, 0.0]);
    }

    fn advance_text_bytes(&mut self, bytes: &[u8], text: &str) {
        let decoder = self.fonts.get(&self.text.font_name);
        let glyph_width = decoder.map_or(bytes.len() as f64 * 0.5, |decoder| decoder.width(bytes));
        let character_count =
            decoder.map_or(bytes.len(), |decoder| decoder.code_count(bytes)) as f64;
        let spaces = text.chars().filter(|character| *character == ' ').count() as f64;
        let advance = glyph_width * self.text.font_size * self.text.horizontal_scale
            + character_count * self.text.character_spacing
            + spaces * self.text.word_spacing;
        self.text.text_matrix = compose(self.text.text_matrix, [1.0, 0.0, 0.0, 1.0, advance, 0.0]);
    }

    fn draw_xobject(&mut self, name: &[u8], depth: usize) -> Result<()> {
        let Some((object_id, stream)) = self.lookup_xobject(name) else {
            self.page.warn(format!(
                "PDF XObject {} was not found",
                String::from_utf8_lossy(name)
            ));
            return Ok(());
        };
        let subtype = stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or_default();
        if subtype == b"Image" {
            self.draw_image(&stream, name)?;
        } else if subtype == b"Form" {
            if let Some(object_id) = object_id
                && !self.visited_forms.insert(object_id)
            {
                return Err(Error::InvalidInput(format!(
                    "cyclic PDF Form XObject at {} {}",
                    object_id.0, object_id.1
                )));
            }
            let previous_state = self.state.clone();
            let previous_content_base_ctm = self.content_base_ctm;
            let form_matrix = stream
                .dict
                .get(b"Matrix")
                .and_then(Object::as_array)
                .ok()
                .and_then(|matrix| matrix_operands(matrix))
                .unwrap_or(IDENTITY);
            self.state.ctm = compose(self.state.ctm, form_matrix);
            self.content_base_ctm = self.state.ctm;
            let group = stream
                .dict
                .get_deref(b"Group", self.document)
                .and_then(Object::as_dict)
                .ok()
                .cloned();
            let node_start = self.page.nodes.len();
            if group.is_some() {
                self.state.fill_alpha = 1.0;
                self.state.stroke_alpha = 1.0;
                self.state.blend_mode = "normal".into();
                self.state.mask_id = None;
                set_paint_opacity(&mut self.state.fill, 1.0);
                set_paint_opacity(&mut self.state.stroke, 1.0);
            }
            let form_resources = stream
                .dict
                .get_deref(b"Resources", self.document)
                .and_then(Object::as_dict)
                .ok()
                .cloned();
            let form_fonts = build_resource_font_decoders(
                self.document,
                form_resources.as_ref(),
                self.content_limit,
                self.page,
            );
            let mut previous_fonts = Vec::with_capacity(form_fonts.len());
            for (font_name, decoder) in form_fonts {
                let previous = self.fonts.insert(font_name.clone(), decoder);
                previous_fonts.push((font_name, previous));
            }
            let mut pushed_resources = false;
            if let Some(resources) = form_resources {
                self.resources.push(resources.clone());
                pushed_resources = true;
            }
            let content = stream.decompressed_content_with_limit(self.content_limit)?;
            let decoded = Content::decode(&content)?;
            let interpret_result = self.interpret(&decoded.operations, depth + 1);
            if pushed_resources {
                self.resources.pop();
            }
            for (font_name, previous) in previous_fonts {
                if let Some(previous) = previous {
                    self.fonts.insert(font_name, previous);
                } else {
                    self.fonts.remove(&font_name);
                }
            }
            self.content_base_ctm = previous_content_base_ctm;
            interpret_result?;
            self.state = previous_state.clone();
            if let Some(group) = group {
                let isolated = group.get(b"I").and_then(Object::as_bool).unwrap_or(false);
                let knockout = group.get(b"K").and_then(Object::as_bool).unwrap_or(false);
                let mut children = self.page.nodes.split_off(node_start);
                let knockout_is_source_over_equivalent =
                    knockout && children_are_opaque_normal(&children);
                if knockout
                    && !knockout_is_source_over_equivalent
                    && !apply_path_knockout_masks(self.page, &mut children, &mut self.mask_counter)
                {
                    self.page.warn(
                        "PDF knockout transparency Form group contains non-path effects that cannot be masked natively",
                    );
                }
                let clip_id = stream
                    .dict
                    .get(b"BBox")
                    .and_then(Object::as_array)
                    .ok()
                    .map(|bbox| numbers(bbox, 4))
                    .filter(|bbox| bbox.len() == 4)
                    .map(|bbox| {
                        self.clip_counter += 1;
                        let id = format!("pdf-clip-{}-{}", self.page.number, self.clip_counter);
                        self.page.clips.push(crate::ir::ClipPath {
                            id: id.clone(),
                            d: rectangle_path(
                                bbox[0],
                                bbox[1],
                                bbox[2] - bbox[0],
                                bbox[3] - bbox[1],
                            ),
                            transform: compose(
                                self.page_matrix,
                                compose(previous_state.ctm, form_matrix),
                            ),
                            fill_rule: "nonzero".into(),
                            parent_id: None,
                            additional_paths: Vec::new(),
                        });
                        id
                    });
                self.node_counter += 1;
                self.page.nodes.push(Node::Group {
                    id: format!("pdf-group-{}-{}", self.page.number, self.node_counter),
                    nodes: children,
                    transform: IDENTITY,
                    opacity: previous_state.fill_alpha,
                    clip_id,
                    meta: SourceMeta {
                        kind: "transparency-group".into(),
                        source_id: format!(
                            "page:{}:xobject:{}",
                            self.page.number,
                            String::from_utf8_lossy(name)
                        ),
                        blend_mode: previous_state.blend_mode.clone(),
                        mask_id: previous_state.mask_id.clone().unwrap_or_default(),
                        alpha_is_shape: previous_state.alpha_is_shape,
                        isolation: isolated,
                        ..SourceMeta::default()
                    },
                });
            }
            if let Some(object_id) = object_id {
                self.visited_forms.remove(&object_id);
            }
        } else {
            self.page.warn(format!(
                "PDF XObject subtype {} is not supported",
                String::from_utf8_lossy(subtype)
            ));
        }
        Ok(())
    }

    fn draw_image(&mut self, stream: &Stream, name: &[u8]) -> Result<()> {
        let width = stream
            .dict
            .get(b"Width")
            .and_then(Object::as_i64)
            .unwrap_or(1)
            .max(1) as usize;
        let height = stream
            .dict
            .get(b"Height")
            .and_then(Object::as_i64)
            .unwrap_or(1)
            .max(1) as usize;
        let pixel_count = width.checked_mul(height).ok_or_else(|| {
            Error::LimitExceeded(format!(
                "PDF image {} dimensions overflow",
                String::from_utf8_lossy(name)
            ))
        })?;
        if width > u32::MAX as usize
            || height > u32::MAX as usize
            || pixel_count > self.content_limit / 4
        {
            return Err(Error::LimitExceeded(format!(
                "PDF image {} expands to {}x{} pixels; RGBA limit is {} bytes",
                String::from_utf8_lossy(name),
                width,
                height,
                self.content_limit
            )));
        }
        let filters = stream.filters().unwrap_or_default();
        let interpolate = stream
            .dict
            .get(b"Interpolate")
            .and_then(Object::as_bool)
            .unwrap_or(false);
        let has_dct = filters.iter().any(|filter| *filter == b"DCTDecode");
        let jpeg_color_space = stream
            .dict
            .get(b"ColorSpace")
            .ok()
            .map(|object| self.resolve_color_space_object(object));
        let jpeg_passthrough = has_dct
            && stream.dict.get(b"SMask").is_err()
            && stream.dict.get(b"Decode").is_err()
            && jpeg_color_space.as_ref().is_none_or(|color_space| {
                matches!(
                    color_space,
                    Object::Name(value)
                        if value == b"DeviceGray"
                            || value == b"G"
                            || value == b"DeviceRGB"
                            || value == b"RGB"
                )
            });
        let (mime, bytes) = if jpeg_passthrough {
            ("image/jpeg", stream.content.clone())
        } else if filters.iter().any(|filter| *filter == b"JPXDecode") {
            ("image/jp2", stream.content.clone())
        } else {
            let ccitt = filters.iter().any(|filter| *filter == b"CCITTFaxDecode");
            let pixels = if has_dct {
                decode_pdf_jpeg_content(stream, width, height, self.content_limit)?
            } else if ccitt {
                ccitt_spec_from_stream(stream, width, height)
                    .decode_ccitt(&stream.content)
                    .ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "PDF CCITT image {} could not be decoded",
                            String::from_utf8_lossy(name)
                        ))
                    })?
            } else {
                stream.decompressed_content_with_limit(self.content_limit)?
            };
            let bits = if ccitt || has_dct {
                8
            } else {
                stream
                    .dict
                    .get(b"BitsPerComponent")
                    .and_then(Object::as_i64)
                    .unwrap_or_else(|_| {
                        if stream
                            .dict
                            .get(b"ImageMask")
                            .and_then(Object::as_bool)
                            .unwrap_or(false)
                        {
                            1
                        } else {
                            8
                        }
                    })
            };
            if !matches!(bits, 1 | 2 | 4 | 8 | 16) {
                self.page.warn(format!(
                    "PDF image {} uses {bits}-bit samples and was skipped",
                    String::from_utf8_lossy(name)
                ));
                return Ok(());
            }
            let image_mask = stream
                .dict
                .get(b"ImageMask")
                .and_then(Object::as_bool)
                .unwrap_or(false);
            let (color_type, output_pixels) = if image_mask {
                let Some(decoded) = decode_stencil_image(
                    stream,
                    &pixels,
                    width,
                    height,
                    bits as usize,
                    &self.state.fill,
                ) else {
                    self.page.warn(format!(
                        "PDF stencil image {} has invalid packed samples and was skipped",
                        String::from_utf8_lossy(name)
                    ));
                    return Ok(());
                };
                (png::ColorType::Rgba, decoded)
            } else {
                let color_space = stream
                    .dict
                    .get(b"ColorSpace")
                    .ok()
                    .map(|object| self.resolve_color_space_object(object));
                let components = pdf_color_component_count(self.document, color_space.as_ref());
                let mut jpeg_cmyk_stream = stream.clone();
                let normalization_stream = if has_dct
                    && matches!(
                        color_space.as_ref(),
                        Some(Object::Name(value)) if value == b"DeviceCMYK" || value == b"CMYK"
                    ) {
                    // jpeg-decoder already resolves Adobe's inverted CMYK
                    // convention. Applying the common PDF Decode [1 0 ...]
                    // array again would invert the samples a second time.
                    jpeg_cmyk_stream.dict.remove(b"Decode");
                    &jpeg_cmyk_stream
                } else {
                    stream
                };
                let Some(pixels) = normalize_image_samples(
                    normalization_stream,
                    &pixels,
                    width,
                    height,
                    bits as usize,
                    components,
                    color_space.as_ref(),
                ) else {
                    self.page.warn(format!(
                        "PDF image {} has invalid packed samples and was skipped",
                        String::from_utf8_lossy(name)
                    ));
                    return Ok(());
                };
                self.decode_image_samples(stream, pixels, width, height, name)?
            };
            if has_dct
                && width <= u16::MAX as usize
                && height <= u16::MAX as usize
                && matches!(color_type, png::ColorType::Grayscale | png::ColorType::Rgb)
            {
                let mut encoded = Vec::new();
                let jpeg_color_type = if color_type == png::ColorType::Grayscale {
                    jpeg_encoder::ColorType::Luma
                } else {
                    jpeg_encoder::ColorType::Rgb
                };
                jpeg_encoder::Encoder::new(&mut encoded, 92)
                    .encode(&output_pixels, width as u16, height as u16, jpeg_color_type)
                    .map_err(|error| {
                        Error::InvalidInput(format!("cannot encode normalized PDF JPEG: {error}"))
                    })?;
                ("image/jpeg", encoded)
            } else {
                let mut encoded = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut encoded, width as u32, height as u32);
                    encoder.set_color(color_type);
                    encoder.set_depth(png::BitDepth::Eight);
                    let mut writer = encoder.write_header().map_err(|error| {
                        Error::InvalidInput(format!("cannot encode PDF image as PNG: {error}"))
                    })?;
                    writer.write_image_data(&output_pixels).map_err(|error| {
                        Error::InvalidInput(format!("cannot encode PDF image data: {error}"))
                    })?;
                }
                ("image/png", encoded)
            }
        };
        self.node_counter += 1;
        let image_flip = [1.0, 0.0, 0.0, -1.0, 0.0, 1.0];
        self.page.nodes.push(Node::Image {
            id: format!("pdf-image-{}-{}", self.page.number, self.node_counter),
            href: format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ),
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            transform: compose(self.page_matrix, compose(self.state.ctm, image_flip)),
            opacity: 1.0,
            clip_id: self.state.clip_id.clone(),
            meta: SourceMeta {
                kind: "image".into(),
                source_id: format!(
                    "page:{}:xobject:{}",
                    self.page.number,
                    String::from_utf8_lossy(name)
                ),
                blend_mode: self.state.blend_mode.clone(),
                mask_id: self.state.mask_id.clone().unwrap_or_default(),
                alpha_is_shape: self.state.alpha_is_shape,
                image_rendering: if interpolate {
                    "auto".into()
                } else {
                    "pixelated".into()
                },
                ..SourceMeta::default()
            },
        });
        Ok(())
    }

    fn decode_image_samples(
        &mut self,
        stream: &Stream,
        pixels: Vec<u8>,
        width: usize,
        height: usize,
        name: &[u8],
    ) -> Result<(png::ColorType, Vec<u8>)> {
        let pixel_count = width.saturating_mul(height);
        let color_space = stream
            .dict
            .get(b"ColorSpace")
            .ok()
            .map(|object| self.resolve_color_space_object(object));
        let (mut color_type, mut output) = match color_space.as_ref() {
            Some(Object::Name(value)) if value == b"DeviceGray" || value == b"G" => {
                (png::ColorType::Grayscale, pixels)
            }
            Some(Object::Name(value)) if value == b"DeviceCMYK" => {
                (png::ColorType::Rgb, cmyk_samples_to_rgb(&pixels))
            }
            Some(Object::Array(array))
                if array.first().and_then(|value| value.as_name().ok()) == Some(b"Indexed") =>
            {
                let palette = array
                    .get(3)
                    .and_then(|value| {
                        self.document
                            .dereference(value)
                            .ok()
                            .map(|(_, value)| value)
                    })
                    .and_then(object_bytes)
                    .unwrap_or_default();
                let components = array
                    .get(1)
                    .and_then(|value| {
                        self.document
                            .dereference(value)
                            .ok()
                            .map(|(_, value)| value)
                    })
                    .map_or(3, color_components);
                if palette.is_empty() || components == 0 {
                    self.page.warn(format!(
                        "PDF indexed image {} has no usable palette",
                        String::from_utf8_lossy(name)
                    ));
                    return Ok((png::ColorType::Grayscale, pixels));
                }
                let mut rgb = Vec::with_capacity(pixel_count.saturating_mul(3));
                for index in pixels {
                    let offset = usize::from(index).saturating_mul(components);
                    let sample = palette.get(offset..offset.saturating_add(components));
                    match (components, sample) {
                        (1, Some(sample)) => {
                            rgb.extend_from_slice(&[sample[0], sample[0], sample[0]]);
                        }
                        (3, Some(sample)) => rgb.extend_from_slice(sample),
                        (4, Some(sample)) => rgb.extend_from_slice(&cmyk_samples_to_rgb(sample)),
                        _ => rgb.extend_from_slice(&[0, 0, 0]),
                    }
                }
                (png::ColorType::Rgb, rgb)
            }
            Some(Object::Array(array))
                if array.first().and_then(|value| value.as_name().ok()) == Some(b"ICCBased") =>
            {
                let components = array
                    .get(1)
                    .and_then(|value| {
                        self.document
                            .dereference(value)
                            .ok()
                            .map(|(_, value)| value)
                    })
                    .and_then(|value| value.as_stream().ok())
                    .and_then(|profile| profile.dict.get(b"N").and_then(Object::as_i64).ok())
                    .unwrap_or(3);
                match components {
                    1 => (png::ColorType::Grayscale, pixels),
                    4 => (png::ColorType::Rgb, cmyk_samples_to_rgb(&pixels)),
                    _ => (png::ColorType::Rgb, pixels),
                }
            }
            Some(Object::Array(array))
                if matches!(
                    array.first().and_then(|value| value.as_name().ok()),
                    Some(b"Separation" | b"DeviceN" | b"Lab" | b"CalGray" | b"CalRGB")
                ) =>
            {
                let components = pdf_color_component_count(self.document, color_space.as_ref());
                if components == 0 || pixels.len() != pixel_count.saturating_mul(components) {
                    self.page.warn(format!(
                        "PDF image {} has incompatible color-space samples",
                        String::from_utf8_lossy(name)
                    ));
                    return Ok((png::ColorType::Rgb, pixels));
                }
                let mut rgb = Vec::with_capacity(pixel_count.saturating_mul(3));
                let is_lab = array.first().and_then(|value| value.as_name().ok()) == Some(b"Lab");
                let explicit_decode = stream
                    .dict
                    .get(b"Decode")
                    .and_then(Object::as_array)
                    .ok()
                    .map(|values| numbers(values, usize::MAX));
                let lab_range = array
                    .get(1)
                    .and_then(|value| {
                        self.document
                            .dereference(value)
                            .ok()
                            .map(|(_, value)| value)
                    })
                    .and_then(|value| value.as_dict().ok())
                    .and_then(|dictionary| dictionary.get(b"Range").and_then(Object::as_array).ok())
                    .map(|values| numbers(values, 4))
                    .filter(|values| values.len() == 4)
                    .unwrap_or_else(|| vec![-100.0, 100.0, -100.0, 100.0]);
                for sample in pixels.chunks_exact(components) {
                    let values = sample
                        .iter()
                        .enumerate()
                        .map(|(component, value)| {
                            let normalized = f64::from(*value) / 255.0;
                            if !is_lab {
                                return normalized;
                            }
                            let (minimum, maximum) = explicit_decode
                                .as_ref()
                                .and_then(|decode| {
                                    Some((
                                        *decode.get(component * 2)?,
                                        *decode.get(component * 2 + 1)?,
                                    ))
                                })
                                .unwrap_or_else(|| match component {
                                    0 => (0.0, 100.0),
                                    1 => (lab_range[0], lab_range[1]),
                                    _ => (lab_range[2], lab_range[3]),
                                });
                            minimum + normalized * (maximum - minimum)
                        })
                        .collect::<Vec<_>>();
                    let converted =
                        components_to_rgb(self.document, color_space.as_ref(), &values, 0);
                    rgb.extend(converted.map(rgb_to_bytes).unwrap_or([0, 0, 0]));
                }
                (png::ColorType::Rgb, rgb)
            }
            Some(Object::Name(value)) if value == b"DeviceRGB" || value == b"RGB" => {
                (png::ColorType::Rgb, pixels)
            }
            _ => {
                let components = pixels.len().checked_div(pixel_count.max(1)).unwrap_or(0);
                self.page.warn(format!(
                    "PDF image {} color space was inferred from {components} sample component(s)",
                    String::from_utf8_lossy(name)
                ));
                match components {
                    1 => (png::ColorType::Grayscale, pixels),
                    4 => (png::ColorType::Rgb, cmyk_samples_to_rgb(&pixels)),
                    _ => (png::ColorType::Rgb, pixels),
                }
            }
        };
        let components = match color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::Rgb => 3,
            _ => 0,
        };
        let expected = pixel_count.saturating_mul(components);
        if output.len() != expected {
            return Err(Error::InvalidInput(format!(
                "PDF image {} decoded to {} bytes; expected {expected}",
                String::from_utf8_lossy(name),
                output.len()
            )));
        }
        if let Ok(mask_object) = stream.dict.get(b"SMask")
            && let Ok((_, mask_object)) = self.document.dereference(mask_object)
            && let Ok(mask) = mask_object.as_stream()
        {
            let mask_width = mask
                .dict
                .get(b"Width")
                .and_then(Object::as_i64)
                .unwrap_or(width as i64)
                .max(1) as usize;
            let mask_height = mask
                .dict
                .get(b"Height")
                .and_then(Object::as_i64)
                .unwrap_or(height as i64)
                .max(1) as usize;
            let mask_bits = mask
                .dict
                .get(b"BitsPerComponent")
                .and_then(Object::as_i64)
                .unwrap_or(8);
            let alpha =
                decode_soft_mask_content(mask, mask_width, mask_height, self.content_limit)?;
            let alpha = if matches!(mask_bits, 1 | 2 | 4 | 8 | 16) {
                normalize_image_samples(
                    mask,
                    &alpha,
                    mask_width,
                    mask_height,
                    mask_bits as usize,
                    1,
                    Some(&Object::Name(b"DeviceGray".to_vec())),
                )
                .unwrap_or(alpha)
            } else {
                alpha
            };
            if mask_width == width && mask_height == height && alpha.len() == pixel_count {
                output = add_alpha(&output, components, &alpha);
                color_type = if components == 1 {
                    png::ColorType::GrayscaleAlpha
                } else {
                    png::ColorType::Rgba
                };
            } else {
                self.page.warn(format!(
                    "PDF image {} soft mask dimensions do not match the source image",
                    String::from_utf8_lossy(name)
                ));
            }
        }
        Ok((color_type, output))
    }

    fn lookup_xobject(&self, name: &[u8]) -> Option<(Option<ObjectId>, Stream)> {
        for resources in self
            .resources
            .iter()
            .rev()
            .chain(page_resource_dicts(self.document, self.page_id))
        {
            let Ok(xobjects) = resources
                .get_deref(b"XObject", self.document)
                .and_then(Object::as_dict)
            else {
                continue;
            };
            if let Ok(object) = xobjects.get(name) {
                let object_id = object.as_reference().ok();
                if let Ok((_, resolved)) = self.document.dereference(object)
                    && let Ok(stream) = resolved.as_stream()
                {
                    return Some((object_id, stream.clone()));
                }
            }
        }
        None
    }

    fn draw_shading(&mut self, name: &[u8]) -> Result<()> {
        let Some(shading) = self.lookup_named_resource(b"Shading", name) else {
            self.page.warn(format!(
                "PDF shading {} was not found",
                String::from_utf8_lossy(name)
            ));
            return Ok(());
        };
        let dictionary = match &shading {
            Object::Dictionary(dictionary) => dictionary,
            Object::Stream(stream) => &stream.dict,
            _ => {
                self.page.warn(format!(
                    "PDF shading {} is not a dictionary or stream",
                    String::from_utf8_lossy(name)
                ));
                return Ok(());
            }
        };
        let shading_type = dictionary
            .get(b"ShadingType")
            .and_then(Object::as_i64)
            .unwrap_or(0);
        if shading_type == 1 {
            return self.draw_function_shading(dictionary, name);
        }
        if matches!(shading_type, 4..=7) {
            return self.draw_mesh_shading(&shading, dictionary, shading_type, name);
        }
        if !matches!(shading_type, 2 | 3) {
            if shading_type == 0 {
                self.page
                    .warn("PDF shading resource has no valid /ShadingType");
            } else {
                self.page.warn(format!(
                    "PDF shading type {shading_type} is detected; tessellation is pending"
                ));
            }
            return Ok(());
        }
        let coordinates = dictionary
            .get(b"Coords")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, 6))
            .unwrap_or_default();
        let required = if shading_type == 2 { 4 } else { 6 };
        if coordinates.len() != required {
            self.page.warn(format!(
                "PDF shading {} has invalid /Coords",
                String::from_utf8_lossy(name)
            ));
            return Ok(());
        }
        let Some(function) = dictionary.get(b"Function").ok() else {
            self.page.warn(format!(
                "PDF shading {} has no /Function",
                String::from_utf8_lossy(name)
            ));
            return Ok(());
        };
        let domain = dictionary
            .get(b"Domain")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, 2))
            .filter(|values| values.len() == 2)
            .unwrap_or_else(|| vec![0.0, 1.0]);
        let color_space = dictionary.get(b"ColorSpace").ok();
        let extend = dictionary
            .get(b"Extend")
            .and_then(Object::as_array)
            .ok()
            .map(|values| {
                [
                    values
                        .first()
                        .and_then(|value| value.as_bool().ok())
                        .unwrap_or(false),
                    values
                        .get(1)
                        .and_then(|value| value.as_bool().ok())
                        .unwrap_or(false),
                ]
            })
            .unwrap_or([false, false]);
        let mut stops = Vec::with_capacity(33);
        for index in 0..=32 {
            let offset = f64::from(index) / 32.0;
            let input = domain[0] + (domain[1] - domain[0]) * offset;
            let Some(components) = evaluate_pdf_function(self.document, function, input, 0) else {
                self.page.warn(format!(
                    "PDF shading {} uses an unsupported function",
                    String::from_utf8_lossy(name)
                ));
                return Ok(());
            };
            stops.push(GradientStop {
                offset,
                color: components_to_color(self.document, color_space, &components),
                opacity: self.state.fill_alpha,
            });
        }
        apply_gradient_extension(&mut stops, extend);
        let transform = compose(self.page_matrix, self.state.ctm);
        let paint = if shading_type == 2 {
            let (x1, y1, x2, y2) = transformed_axial_axis(
                transform,
                coordinates[0],
                coordinates[1],
                coordinates[2],
                coordinates[3],
            )
            .unwrap_or_else(|| {
                let first = transform_point(transform, coordinates[0], coordinates[1]);
                let second = transform_point(transform, coordinates[2], coordinates[3]);
                (first.0, first.1, second.0, second.1)
            });
            Paint::LinearGradient(Box::new(LinearGradient {
                x1,
                y1,
                x2,
                y2,
                stops,
            }))
        } else {
            let mut first_radius = coordinates[2].max(0.0);
            let mut second_radius = coordinates[5].max(0.0);
            let mut first_x = coordinates[0];
            let mut first_y = coordinates[1];
            let mut second_x = coordinates[3];
            let mut second_y = coordinates[4];
            let center_distance = (second_x - first_x).hypot(second_y - first_y);
            if center_distance + first_radius.min(second_radius)
                > first_radius.max(second_radius) + 1e-9
            {
                return self.draw_noncontained_radial(
                    &coordinates,
                    function,
                    &domain,
                    color_space,
                    extend,
                    transform,
                    name,
                );
            }
            let mut stops = stops;
            if first_radius > second_radius {
                std::mem::swap(&mut first_radius, &mut second_radius);
                std::mem::swap(&mut first_x, &mut second_x);
                std::mem::swap(&mut first_y, &mut second_y);
                for stop in &mut stops {
                    stop.offset = 1.0 - stop.offset;
                }
                stops.reverse();
            }
            Paint::RadialGradient(Box::new(RadialGradient {
                fx: first_x,
                fy: first_y,
                fr: first_radius,
                cx: second_x,
                cy: second_y,
                radius: second_radius.max(1e-9),
                transform,
                stops,
            }))
        };
        let mut clip_id = self.state.clip_id.clone();
        if let Ok(bbox) = dictionary.get(b"BBox").and_then(Object::as_array) {
            let bbox = numbers(bbox, 4);
            if bbox.len() == 4 {
                let parent_id = clip_id.clone();
                self.clip_counter += 1;
                let id = format!("pdf-clip-{}-{}", self.page.number, self.clip_counter);
                self.page.clips.push(crate::ir::ClipPath {
                    id: id.clone(),
                    d: rectangle_path(bbox[0], bbox[1], bbox[2] - bbox[0], bbox[3] - bbox[1]),
                    transform,
                    fill_rule: "nonzero".into(),
                    parent_id,
                    additional_paths: Vec::new(),
                });
                clip_id = Some(id);
            }
        }
        self.node_counter += 1;
        self.page.nodes.push(Node::Path {
            id: format!("pdf-shading-{}-{}", self.page.number, self.node_counter),
            d: rectangle_path(0.0, 0.0, self.page.width, self.page.height),
            fill_rule: "nonzero".into(),
            fill: paint,
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id,
            meta: SourceMeta {
                kind: "gradient".into(),
                source_id: format!(
                    "page:{}:shading:{}",
                    self.page.number,
                    String::from_utf8_lossy(name)
                ),
                blend_mode: self.state.blend_mode.clone(),
                mask_id: self.state.mask_id.clone().unwrap_or_default(),
                alpha_is_shape: self.state.alpha_is_shape,
                ..SourceMeta::default()
            },
        });
        Ok(())
    }

    fn draw_function_shading(&mut self, dictionary: &Dictionary, name: &[u8]) -> Result<()> {
        let Some(function) = dictionary.get(b"Function").ok() else {
            self.page.warn(format!(
                "PDF function shading {} has no /Function",
                String::from_utf8_lossy(name)
            ));
            return Ok(());
        };
        let domain = dictionary
            .get(b"Domain")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, 4))
            .filter(|values| values.len() == 4)
            .unwrap_or_else(|| vec![0.0, 1.0, 0.0, 1.0]);
        let shading_matrix = dictionary
            .get(b"Matrix")
            .and_then(Object::as_array)
            .ok()
            .and_then(|values| matrix_operands(values))
            .unwrap_or(IDENTITY);
        let transform = compose(compose(self.page_matrix, self.state.ctm), shading_matrix);
        let field = FunctionShadingField {
            document: self.document,
            function,
            transform,
        };
        let root = FunctionShadingBounds {
            x0: domain[0],
            x1: domain[1],
            y0: domain[2],
            y1: domain[3],
        };
        const MAX_CELLS: usize = 100_000;
        let mut cells = Vec::new();
        if !adaptive_function_shading_cells(&field, root, 0, &mut cells, MAX_CELLS) {
            self.page.warn(format!(
                "PDF function shading {} uses an unsupported function",
                String::from_utf8_lossy(name)
            ));
            return Ok(());
        }
        if cells.len() >= MAX_CELLS {
            return Err(Error::LimitExceeded(format!(
                "PDF function shading exceeds {MAX_CELLS} vector cells"
            )));
        }
        let color_space = dictionary.get(b"ColorSpace").ok();
        let mut clip_id = self.state.clip_id.clone();
        if let Ok(bbox) = dictionary.get(b"BBox").and_then(Object::as_array) {
            let bbox = numbers(bbox, 4);
            if bbox.len() == 4 {
                self.clip_counter += 1;
                let id = format!("pdf-clip-{}-{}", self.page.number, self.clip_counter);
                self.page.clips.push(crate::ir::ClipPath {
                    id: id.clone(),
                    d: rectangle_path(bbox[0], bbox[1], bbox[2] - bbox[0], bbox[3] - bbox[1]),
                    transform,
                    fill_rule: "nonzero".into(),
                    parent_id: clip_id,
                    additional_paths: Vec::new(),
                });
                clip_id = Some(id);
            }
        }
        for (index, cell) in cells.into_iter().enumerate() {
            self.node_counter += 1;
            self.page.nodes.push(Node::Path {
                id: format!("pdf-function-cell-{}-{}", self.page.number, index + 1),
                d: quadrilateral_path(cell.points),
                fill_rule: "nonzero".into(),
                fill: Paint::Solid {
                    color: components_to_color(self.document, color_space, &cell.components),
                    opacity: self.state.fill_alpha,
                },
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: clip_id.clone(),
                meta: SourceMeta {
                    kind: "function-shading-cell".into(),
                    source_id: format!(
                        "page:{}:shading:{}",
                        self.page.number,
                        String::from_utf8_lossy(name)
                    ),
                    blend_mode: self.state.blend_mode.clone(),
                    mask_id: self.state.mask_id.clone().unwrap_or_default(),
                    alpha_is_shape: self.state.alpha_is_shape,
                    shape_rendering: "crispEdges".into(),
                    ..SourceMeta::default()
                },
            });
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_noncontained_radial(
        &mut self,
        coordinates: &[f64],
        function: &Object,
        domain: &[f64],
        color_space: Option<&Object>,
        extend: [bool; 2],
        transform: Matrix,
        name: &[u8],
    ) -> Result<()> {
        let Some(inverse) = inverse_matrix(transform) else {
            self.page
                .warn("PDF noncontained radial shading has a singular transform");
            return Ok(());
        };
        let field = NoncontainedRadialField {
            document: self.document,
            function,
            coordinates: [
                coordinates[0],
                coordinates[1],
                coordinates[2],
                coordinates[3],
                coordinates[4],
                coordinates[5],
            ],
            domain: [domain[0], domain[1]],
            extend,
            inverse,
        };
        const MAX_CELLS: usize = 100_000;
        let mut cells = Vec::new();
        let columns = 10usize;
        let rows = 5usize;
        let cell_width = self.page.width / columns as f64;
        let cell_height = self.page.height / rows as f64;
        for row in 0..rows {
            for column in 0..columns {
                adaptive_radial_cells(
                    &field,
                    RadialBounds {
                        x: column as f64 * cell_width,
                        y: row as f64 * cell_height,
                        width: cell_width,
                        height: cell_height,
                    },
                    0,
                    &mut cells,
                    MAX_CELLS,
                );
            }
        }
        if cells.len() >= MAX_CELLS {
            return Err(Error::LimitExceeded(format!(
                "PDF noncontained radial shading exceeds {MAX_CELLS} vector cells"
            )));
        }
        for (index, cell) in cells.into_iter().enumerate() {
            self.node_counter += 1;
            self.page.nodes.push(Node::Path {
                id: format!("pdf-radial-cell-{}-{}", self.page.number, index + 1),
                d: rectangle_path(
                    cell.bounds.x,
                    cell.bounds.y,
                    cell.bounds.width,
                    cell.bounds.height,
                ),
                fill_rule: "nonzero".into(),
                fill: Paint::Solid {
                    color: components_to_color(self.document, color_space, &cell.components),
                    opacity: self.state.fill_alpha * cell.coverage,
                },
                stroke: Stroke::default(),
                transform: IDENTITY,
                clip_id: self.state.clip_id.clone(),
                meta: SourceMeta {
                    kind: "radial-field-cell".into(),
                    source_id: format!(
                        "page:{}:shading:{}",
                        self.page.number,
                        String::from_utf8_lossy(name)
                    ),
                    blend_mode: self.state.blend_mode.clone(),
                    mask_id: self.state.mask_id.clone().unwrap_or_default(),
                    alpha_is_shape: self.state.alpha_is_shape,
                    shape_rendering: "crispEdges".into(),
                    ..SourceMeta::default()
                },
            });
        }
        Ok(())
    }

    fn draw_mesh_shading(
        &mut self,
        shading: &Object,
        dictionary: &Dictionary,
        shading_type: i64,
        name: &[u8],
    ) -> Result<()> {
        let Object::Stream(stream) = shading else {
            self.page.warn(format!(
                "PDF mesh shading {} has no stream data",
                String::from_utf8_lossy(name)
            ));
            return Ok(());
        };
        let data = stream.decompressed_content_with_limit(self.content_limit)?;
        let bits_per_coordinate = dictionary
            .get(b"BitsPerCoordinate")
            .and_then(Object::as_i64)
            .unwrap_or(0);
        let bits_per_component = dictionary
            .get(b"BitsPerComponent")
            .and_then(Object::as_i64)
            .unwrap_or(0);
        let bits_per_flag = dictionary
            .get(b"BitsPerFlag")
            .and_then(Object::as_i64)
            .unwrap_or(0);
        if bits_per_coordinate <= 0
            || bits_per_component <= 0
            || bits_per_coordinate > 32
            || bits_per_component > 16
        {
            self.page.warn("PDF mesh shading has invalid bit widths");
            return Ok(());
        }
        let color_space = dictionary.get(b"ColorSpace").ok();
        let component_count = pdf_color_component_count(self.document, color_space);
        let decode = dictionary
            .get(b"Decode")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, usize::MAX))
            .unwrap_or_default();
        let Some(decode) = complete_mesh_decode_ranges(&decode, component_count) else {
            self.page
                .warn("PDF mesh shading /Decode array is incomplete");
            return Ok(());
        };
        let vertices_per_row = dictionary
            .get(b"VerticesPerRow")
            .and_then(Object::as_i64)
            .unwrap_or(0)
            .max(0) as usize;
        let remaining_page_budget =
            MAX_PAGE_MESH_TRIANGLES.saturating_sub(self.mesh_output_count.get());
        if remaining_page_budget == 0 {
            return Ok(());
        }
        let max_output_triangles = remaining_page_budget.min(3_000);
        let (triangles, subdivisions) = if matches!(shading_type, 6 | 7) {
            let (patches, has_reused_patch) = parse_patch_meshes(
                &data,
                shading_type,
                bits_per_coordinate as usize,
                bits_per_component as usize,
                bits_per_flag.max(0) as usize,
                component_count,
                &decode,
            );
            if has_reused_patch {
                self.page.warn(
                    "PDF patch mesh reuse flags 1-3 are detected; only independent flag-0 patches are rendered",
                );
            }
            let divisions = ((max_output_triangles / (patches.len().max(1) * 2)) as f64)
                .sqrt()
                .floor()
                .clamp(1.0, 64.0) as usize;
            (
                patches
                    .iter()
                    .flat_map(|patch| tessellate_patch(patch, divisions))
                    .collect(),
                1,
            )
        } else {
            let triangles = parse_mesh_triangles(
                &data,
                shading_type,
                bits_per_coordinate as usize,
                bits_per_component as usize,
                bits_per_flag.max(0) as usize,
                component_count,
                &decode,
                vertices_per_row,
            );
            let divisions = ((max_output_triangles / triangles.len().max(1)) as f64)
                .sqrt()
                .floor()
                .clamp(1.0, 64.0) as usize;
            (triangles, divisions)
        };
        if triangles.is_empty() {
            self.page.warn(format!(
                "PDF mesh shading {} produced no triangles",
                String::from_utf8_lossy(name)
            ));
            return Ok(());
        }
        let transform = compose(self.page_matrix, self.state.ctm);
        let mut output_count = 0usize;
        'triangles: for (triangle_index, triangle) in triangles.iter().enumerate() {
            for micro in subdivide_mesh_triangle(triangle, subdivisions) {
                if output_count >= max_output_triangles
                    || self.mesh_output_count.get() >= MAX_PAGE_MESH_TRIANGLES
                {
                    break 'triangles;
                }
                output_count += 1;
                self.mesh_output_count
                    .set(self.mesh_output_count.get().saturating_add(1));
                let color =
                    components_to_color(self.document, color_space, &micro.average_components());
                self.node_counter += 1;
                self.page.nodes.push(Node::Path {
                    id: format!(
                        "pdf-mesh-{}-{}-{}",
                        self.page.number, triangle_index, output_count
                    ),
                    d: micro.path_data(),
                    fill_rule: "nonzero".into(),
                    fill: Paint::Solid {
                        color: color.clone(),
                        opacity: self.state.fill_alpha,
                    },
                    stroke: Stroke::default(),
                    transform,
                    clip_id: self.state.clip_id.clone(),
                    meta: SourceMeta {
                        kind: "mesh-triangle".into(),
                        source_id: format!(
                            "page:{}:shading:{}",
                            self.page.number,
                            String::from_utf8_lossy(name)
                        ),
                        blend_mode: self.state.blend_mode.clone(),
                        mask_id: self.state.mask_id.clone().unwrap_or_default(),
                        alpha_is_shape: self.state.alpha_is_shape,
                        shape_rendering: "crispEdges".into(),
                        ..SourceMeta::default()
                    },
                });
            }
        }
        Ok(())
    }

    fn lookup_named_resource(&self, category: &[u8], name: &[u8]) -> Option<Object> {
        self.lookup_named_resource_with_id(category, name)
            .map(|(_, object)| object)
    }

    fn lookup_named_resource_with_id(
        &self,
        category: &[u8],
        name: &[u8],
    ) -> Option<(Option<ObjectId>, Object)> {
        for resources in self
            .resources
            .iter()
            .rev()
            .chain(page_resource_dicts(self.document, self.page_id))
        {
            let Ok(values) = resources
                .get_deref(category, self.document)
                .and_then(Object::as_dict)
            else {
                continue;
            };
            if let Ok(object) = values.get(name)
                && let Ok((_, resolved)) = self.document.dereference(object)
            {
                return Some((object.as_reference().ok(), resolved.clone()));
            }
        }
        None
    }

    fn apply_ext_gstate(&mut self, name: &[u8]) -> Result<()> {
        let mut soft_mask = None::<Object>;
        for resources in self
            .resources
            .iter()
            .rev()
            .chain(page_resource_dicts(self.document, self.page_id))
        {
            let Ok(states) = resources
                .get_deref(b"ExtGState", self.document)
                .and_then(Object::as_dict)
            else {
                continue;
            };
            let Ok(value) = states.get(name) else {
                continue;
            };
            let Ok((_, value)) = self.document.dereference(value) else {
                continue;
            };
            let Ok(dictionary) = value.as_dict() else {
                continue;
            };
            if let Ok(alpha) = dictionary.get(b"ca").and_then(Object::as_float) {
                self.state.fill_alpha = f64::from(alpha).clamp(0.0, 1.0);
                set_paint_opacity(&mut self.state.fill, self.state.fill_alpha);
            }
            if let Ok(alpha) = dictionary.get(b"CA").and_then(Object::as_float) {
                self.state.stroke_alpha = f64::from(alpha).clamp(0.0, 1.0);
                set_paint_opacity(&mut self.state.stroke, self.state.stroke_alpha);
            }
            if let Some(width) = number(dictionary.get(b"LW").ok()) {
                self.state.line_width = width.max(0.0);
            }
            if let Some(cap) = integer(dictionary.get(b"LC").ok()) {
                self.state.line_cap = match cap {
                    1 => LineCap::Round,
                    2 => LineCap::Square,
                    _ => LineCap::Butt,
                };
            }
            if let Some(join) = integer(dictionary.get(b"LJ").ok()) {
                self.state.line_join = match join {
                    1 => LineJoin::Round,
                    2 => LineJoin::Bevel,
                    _ => LineJoin::Miter,
                };
            }
            if let Some(limit) = number(dictionary.get(b"ML").ok()) {
                self.state.miter_limit = limit.max(1.0);
            }
            if let Ok(dash) = dictionary.get(b"D").and_then(Object::as_array) {
                self.state.dash_array = dash
                    .first()
                    .and_then(|object| object.as_array().ok())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| number(Some(value)))
                            .collect()
                    })
                    .unwrap_or_default();
                self.state.dash_offset = number(dash.get(1)).unwrap_or(0.0);
            }
            if let Ok(alpha_is_shape) = dictionary.get(b"AIS").and_then(Object::as_bool) {
                self.state.alpha_is_shape = alpha_is_shape;
            }
            if let Ok(blend) = dictionary.get(b"BM") {
                let blend = match blend {
                    Object::Name(name) => Some(name.as_slice()),
                    Object::Array(values) => values.first().and_then(|value| value.as_name().ok()),
                    _ => None,
                };
                if let Some(blend) = blend {
                    self.state.blend_mode = pdf_blend_mode(blend);
                }
            }
            soft_mask = dictionary.get(b"SMask").ok().cloned();
            break;
        }
        if let Some(soft_mask) = soft_mask {
            self.apply_soft_mask(&soft_mask)?;
        }
        Ok(())
    }

    fn apply_soft_mask(&mut self, object: &Object) -> Result<()> {
        let (_, object) = self.document.dereference(object)?;
        if object.as_name().ok() == Some(b"None") {
            self.state.mask_id = None;
            return Ok(());
        }
        let dictionary = object.as_dict()?;
        let mask_type = match dictionary
            .get(b"S")
            .and_then(Object::as_name)
            .unwrap_or(b"Luminosity")
        {
            b"Alpha" => "alpha",
            _ => "luminance",
        };
        let group_object = dictionary
            .get(b"G")
            .map_err(|_| Error::InvalidInput("PDF soft mask dictionary has no /G form".into()))?;
        let (group_id, group_object) = self.document.dereference(group_object)?;
        let group_stream = group_object.as_stream()?.clone();
        if group_stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .unwrap_or_default()
            != b"Form"
        {
            return Err(Error::InvalidInput(
                "PDF soft mask /G is not a Form XObject".into(),
            ));
        }
        let form_matrix = group_stream
            .dict
            .get(b"Matrix")
            .and_then(Object::as_array)
            .ok()
            .and_then(|values| matrix_operands(values))
            .unwrap_or(IDENTITY);
        let resources = group_stream
            .dict
            .get_deref(b"Resources", self.document)
            .and_then(Object::as_dict)
            .ok()
            .cloned();
        let group_dictionary = group_stream
            .dict
            .get_deref(b"Group", self.document)
            .and_then(Object::as_dict)
            .ok();
        let group_color_space = group_dictionary
            .and_then(|group| group.get(b"CS").ok())
            .cloned();
        let form_bbox = group_stream
            .dict
            .get(b"BBox")
            .and_then(Object::as_array)
            .ok()
            .map(|values| numbers(values, 4))
            .filter(|values| values.len() == 4);
        let content = group_stream.decompressed_content_with_limit(self.content_limit)?;
        let decoded = Content::decode(&content)?;
        let mut mask_page = Page::new(
            self.page.number,
            self.page.width,
            self.page.height,
            "pdf-soft-mask",
        );
        let mut mask_fonts = self.fonts.clone();
        mask_fonts.extend(build_resource_font_decoders(
            self.document,
            resources.as_ref(),
            self.content_limit,
            &mut mask_page,
        ));
        let mut mask_interpreter = Interpreter {
            document: self.document,
            page: &mut mask_page,
            page_matrix: self.page_matrix,
            page_id: self.page_id,
            fonts: mask_fonts,
            resources: resources.into_iter().collect(),
            state: GraphicsState {
                ctm: compose(self.state.ctm, form_matrix),
                ..GraphicsState::default()
            },
            content_base_ctm: compose(self.state.ctm, form_matrix),
            stack: Vec::new(),
            path: PathBuilder::default(),
            pending_clip_rule: None,
            text: TextState::default(),
            node_counter: self.node_counter,
            clip_counter: self.clip_counter,
            mask_counter: self.mask_counter,
            compatibility_depth: self.compatibility_depth,
            visited_forms: self.visited_forms.clone(),
            visited_patterns: self.visited_patterns.clone(),
            mesh_output_count: Rc::clone(&self.mesh_output_count),
            content_limit: self.content_limit,
            outline_embedded_pdf_text: self.outline_embedded_pdf_text,
        };
        if let Some(group_id) = group_id {
            mask_interpreter.visited_forms.insert(group_id);
        }
        mask_interpreter.interpret(&decoded.operations, 1)?;
        self.node_counter = mask_interpreter.node_counter;
        self.clip_counter = mask_interpreter.clip_counter;
        self.mask_counter = mask_interpreter.mask_counter;
        drop(mask_interpreter);
        self.mask_counter += 1;
        let mask_id = format!("pdf-mask-{}-{}", self.page.number, self.mask_counter);
        let mask_transform = compose(self.page_matrix, compose(self.state.ctm, form_matrix));
        let mut mask_nodes = mask_page.nodes;
        if mask_type == "luminance"
            && let Ok(backdrop) = dictionary.get(b"BC").and_then(Object::as_array)
        {
            let components = numbers(backdrop, usize::MAX);
            let color = components_to_color(self.document, group_color_space.as_ref(), &components);
            let bbox = form_bbox
                .as_ref()
                .map_or([0.0, 0.0, self.page.width, self.page.height], |bbox| {
                    [bbox[0], bbox[1], bbox[2], bbox[3]]
                });
            mask_nodes.insert(
                0,
                Node::Path {
                    id: format!("{mask_id}-backdrop"),
                    d: rectangle_path(bbox[0], bbox[1], bbox[2] - bbox[0], bbox[3] - bbox[1]),
                    fill_rule: "nonzero".into(),
                    fill: Paint::solid(color),
                    stroke: Stroke::default(),
                    transform: mask_transform,
                    clip_id: None,
                    meta: SourceMeta {
                        kind: "mask-backdrop".into(),
                        ..SourceMeta::default()
                    },
                },
            );
        }
        if let Some(bbox) = form_bbox {
            self.clip_counter += 1;
            let clip_id = format!("pdf-clip-{}-{}", self.page.number, self.clip_counter);
            mask_page.clips.push(crate::ir::ClipPath {
                id: clip_id.clone(),
                d: rectangle_path(bbox[0], bbox[1], bbox[2] - bbox[0], bbox[3] - bbox[1]),
                transform: mask_transform,
                fill_rule: "nonzero".into(),
                parent_id: None,
                additional_paths: Vec::new(),
            });
            mask_nodes = vec![Node::Group {
                id: format!("{mask_id}-group"),
                nodes: mask_nodes,
                transform: IDENTITY,
                opacity: 1.0,
                clip_id: Some(clip_id),
                meta: SourceMeta {
                    kind: "soft-mask-group".into(),
                    ..SourceMeta::default()
                },
            }];
        }
        self.page.clips.extend(mask_page.clips);
        self.page.masks.extend(mask_page.masks);
        let transfer_values = dictionary.get(b"TR").ok().map_or_else(
            || Some(Vec::new()),
            |transfer| soft_mask_transfer_values(self.document, transfer),
        );
        if dictionary.get(b"TR").is_ok() && transfer_values.is_none() {
            self.page
                .warn("PDF soft-mask transfer function /TR is unsupported");
        }
        self.page.masks.push(crate::ir::MaskDefinition {
            id: mask_id.clone(),
            mask_type: mask_type.into(),
            nodes: mask_nodes,
            transfer_values: transfer_values.unwrap_or_default(),
        });
        for warning in mask_page.warnings {
            self.page.warn(format!("soft mask: {warning}"));
        }
        self.state.mask_id = Some(mask_id);
        Ok(())
    }
}

fn decode_pdf_jpeg_content(
    stream: &Stream,
    expected_width: usize,
    expected_height: usize,
    content_limit: usize,
) -> Result<Vec<u8>> {
    if stream.content.len() > content_limit {
        return Err(Error::LimitExceeded(format!(
            "PDF JPEG stream exceeds {content_limit} bytes"
        )));
    }
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(stream.content.as_slice()));
    decoder
        .read_info()
        .map_err(|error| Error::InvalidInput(format!("cannot read PDF JPEG metadata: {error}")))?;
    let info = decoder
        .info()
        .ok_or_else(|| Error::InvalidInput("PDF JPEG has no image metadata".into()))?;
    let width = usize::from(info.width);
    let height = usize::from(info.height);
    if width != expected_width || height != expected_height {
        return Err(Error::InvalidInput(format!(
            "PDF JPEG is {width}x{height}; expected {expected_width}x{expected_height}"
        )));
    }
    let decoded_bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(info.pixel_format.pixel_bytes()))
        .ok_or_else(|| Error::LimitExceeded("PDF JPEG decoded size overflow".into()))?;
    if decoded_bytes > content_limit {
        return Err(Error::LimitExceeded(format!(
            "PDF JPEG expands to {decoded_bytes} bytes; limit is {content_limit}"
        )));
    }
    let pixels = decoder
        .decode()
        .map_err(|error| Error::InvalidInput(format!("cannot decode PDF JPEG: {error}")))?;
    Ok(match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 | jpeg_decoder::PixelFormat::RGB24 => pixels,
        // Four-component JPEG streams in PDF use Adobe's inverted CMYK
        // convention. jpeg-decoder exposes those inverted samples, while PDF
        // color-space evaluation expects ordinary ink coverage values.
        jpeg_decoder::PixelFormat::CMYK32 => {
            pixels.into_iter().map(|sample| 255 - sample).collect()
        }
        jpeg_decoder::PixelFormat::L16 => pixels.chunks_exact(2).map(|sample| sample[0]).collect(),
    })
}

fn decode_soft_mask_content(
    mask: &Stream,
    expected_width: usize,
    expected_height: usize,
    content_limit: usize,
) -> Result<Vec<u8>> {
    let filters = mask.filters().unwrap_or_default();
    if !filters.iter().any(|filter| *filter == b"DCTDecode") {
        return Ok(mask.decompressed_content_with_limit(content_limit)?);
    }
    if mask.content.len() > content_limit {
        return Err(Error::LimitExceeded(format!(
            "PDF JPEG soft mask stream exceeds {content_limit} bytes"
        )));
    }
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(mask.content.as_slice()));
    decoder.read_info().map_err(|error| {
        Error::InvalidInput(format!("cannot read PDF JPEG soft mask metadata: {error}"))
    })?;
    let info = decoder
        .info()
        .ok_or_else(|| Error::InvalidInput("PDF JPEG soft mask has no image metadata".into()))?;
    let width = usize::from(info.width);
    let height = usize::from(info.height);
    if width != expected_width || height != expected_height {
        return Err(Error::InvalidInput(format!(
            "PDF JPEG soft mask is {width}x{height}; expected {expected_width}x{expected_height}"
        )));
    }
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| Error::LimitExceeded("PDF JPEG soft mask dimensions overflow".into()))?;
    let decoded_bytes = pixel_count
        .checked_mul(info.pixel_format.pixel_bytes())
        .ok_or_else(|| Error::LimitExceeded("PDF JPEG soft mask size overflow".into()))?;
    if decoded_bytes > content_limit {
        return Err(Error::LimitExceeded(format!(
            "PDF JPEG soft mask expands to {decoded_bytes} bytes; limit is {content_limit}"
        )));
    }
    let pixels = decoder.decode().map_err(|error| {
        Error::InvalidInput(format!("cannot decode PDF JPEG soft mask: {error}"))
    })?;
    let alpha = match info.pixel_format {
        jpeg_decoder::PixelFormat::L8 => pixels,
        jpeg_decoder::PixelFormat::L16 => pixels.chunks_exact(2).map(|sample| sample[0]).collect(),
        jpeg_decoder::PixelFormat::RGB24 => pixels
            .chunks_exact(3)
            .map(|sample| {
                ((u16::from(sample[0]) * 54
                    + u16::from(sample[1]) * 183
                    + u16::from(sample[2]) * 19)
                    >> 8) as u8
            })
            .collect(),
        jpeg_decoder::PixelFormat::CMYK32 => cmyk_samples_to_rgb(&pixels)
            .chunks_exact(3)
            .map(|sample| {
                ((u16::from(sample[0]) * 54
                    + u16::from(sample[1]) * 183
                    + u16::from(sample[2]) * 19)
                    >> 8) as u8
            })
            .collect(),
    };
    if alpha.len() != pixel_count {
        return Err(Error::InvalidInput(format!(
            "PDF JPEG soft mask decoded to {} samples; expected {pixel_count}",
            alpha.len()
        )));
    }
    Ok(alpha)
}

fn build_font_decoders(
    document: &Document,
    page_id: ObjectId,
    content_limit: usize,
    page: &mut Page,
) -> Result<HashMap<Vec<u8>, FontDecoder>> {
    let mut result = HashMap::new();
    for (name, dictionary) in document.get_page_fonts(page_id)? {
        result.insert(
            name.clone(),
            build_font_decoder(document, &name, dictionary, content_limit, page),
        );
    }
    Ok(result)
}

fn build_resource_font_decoders(
    document: &Document,
    resources: Option<&Dictionary>,
    content_limit: usize,
    page: &mut Page,
) -> HashMap<Vec<u8>, FontDecoder> {
    let Some(fonts) = resources.and_then(|resources| {
        resources
            .get_deref(b"Font", document)
            .and_then(Object::as_dict)
            .ok()
    }) else {
        return HashMap::new();
    };
    fonts
        .iter()
        .filter_map(|(name, object)| {
            document
                .dereference(object)
                .ok()
                .and_then(|(_, object)| object.as_dict().ok())
                .map(|dictionary| {
                    (
                        name.clone(),
                        build_font_decoder(document, name, dictionary, content_limit, page),
                    )
                })
        })
        .collect()
}

fn build_font_decoder(
    document: &Document,
    name: &[u8],
    dictionary: &Dictionary,
    content_limit: usize,
    page: &mut Page,
) -> FontDecoder {
    let family = dictionary
        .get(b"BaseFont")
        .and_then(Object::as_name)
        .map(clean_font_name)
        .unwrap_or_else(|_| "Arial, 'Hiragino Sans', 'Yu Gothic', sans-serif".into());
    let lowercase_family = family.to_ascii_lowercase();
    let bold = lowercase_family.contains("bold") || lowercase_family.contains("black");
    let italic = lowercase_family.contains("italic") || lowercase_family.contains("oblique");
    let subtype = dictionary
        .get(b"Subtype")
        .and_then(Object::as_name)
        .unwrap_or_default();
    let descriptor = pdf_font_descriptor(document, dictionary, subtype);
    let embedded = descriptor.is_some_and(|descriptor| {
        descriptor.get(b"FontFile").is_ok()
            || descriptor.get(b"FontFile2").is_ok()
            || descriptor.get(b"FontFile3").is_ok()
    });
    let font_data = descriptor.and_then(|descriptor| {
        [
            b"FontFile".as_slice(),
            b"FontFile2".as_slice(),
            b"FontFile3".as_slice(),
        ]
        .into_iter()
        .find_map(|key| {
            descriptor
                .get_deref(key, document)
                .and_then(Object::as_stream)
                .ok()
                .and_then(|stream| stream.decompressed_content_with_limit(content_limit).ok())
        })
        .map(Arc::<[u8]>::from)
    });
    let requires_outline = embedded && !is_browser_font_family(&family);
    let glyph_names = pdf_encoding_glyph_names(document, dictionary);
    let fallback_kind = if subtype == b"Type0" {
        FontFallback::Utf16Be
    } else {
        FontFallback::OneByte
    };
    let mut unicode_map = dictionary
        .get_deref(b"ToUnicode", document)
        .and_then(Object::as_stream)
        .ok()
        .and_then(|stream| stream.decompressed_content_with_limit(content_limit).ok())
        .map(|bytes| parse_to_unicode_cmap(&bytes))
        .unwrap_or_default();
    if unicode_map.is_empty()
        && subtype != b"Type0"
        && let Ok(encoding) = dictionary.get_font_encoding(document)
    {
        for code in 0u8..=u8::MAX {
            if let Ok(value) = encoding.bytes_to_string(&[code])
                && !value.is_empty()
            {
                unicode_map.insert(vec![code], value);
            }
        }
    }
    let mut code_lengths = unicode_map.keys().map(Vec::len).collect::<Vec<_>>();
    code_lengths.sort_unstable_by(|left, right| right.cmp(left));
    code_lengths.dedup();
    let (widths, default_width) = font_widths(document, dictionary, subtype);
    let type3 = (subtype == b"Type3")
        .then(|| build_type3_font(document, dictionary, &glyph_names))
        .flatten()
        .map(Arc::new);
    if unicode_map.is_empty() && subtype == b"Type0" {
        page.warn(format!(
            "Type0 font {} has no usable ToUnicode map; text may require replacement glyphs",
            String::from_utf8_lossy(name)
        ));
    }
    FontDecoder {
        family,
        bold,
        italic,
        requires_outline,
        font_data,
        glyph_names,
        unicode_map,
        code_lengths,
        fallback_kind,
        widths,
        default_width,
        type3,
    }
}

fn build_type3_font(
    document: &Document,
    dictionary: &Dictionary,
    glyph_names: &HashMap<u8, String>,
) -> Option<Type3Font> {
    let char_procedures = dictionary
        .get_deref(b"CharProcs", document)
        .and_then(Object::as_dict)
        .ok()?;
    let mut glyphs = HashMap::new();
    for (code, name) in glyph_names {
        if let Ok(object) = char_procedures.get(name.as_bytes())
            && let Ok((_, object)) = document.dereference(object)
            && let Ok(stream) = object.as_stream()
        {
            glyphs.insert(*code, stream.clone());
        }
    }
    if glyphs.is_empty() {
        return None;
    }
    let font_matrix = dictionary
        .get(b"FontMatrix")
        .and_then(Object::as_array)
        .ok()
        .and_then(|values| matrix_operands(values))
        .unwrap_or([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]);
    let resources = dictionary
        .get_deref(b"Resources", document)
        .and_then(Object::as_dict)
        .ok()
        .cloned();
    Some(Type3Font {
        font_matrix,
        resources,
        glyphs,
    })
}

fn font_widths(
    document: &Document,
    dictionary: &Dictionary,
    subtype: &[u8],
) -> (HashMap<u32, f64>, f64) {
    if subtype == b"Type0" {
        let descendant = dictionary
            .get_deref(b"DescendantFonts", document)
            .and_then(Object::as_array)
            .ok()
            .and_then(|array| array.first())
            .and_then(|object| document.dereference(object).ok().map(|(_, value)| value))
            .and_then(|object| object.as_dict().ok());
        let Some(descendant) = descendant else {
            return (HashMap::new(), 1_000.0);
        };
        let default_width = descendant
            .get(b"DW")
            .ok()
            .and_then(|value| number(Some(value)))
            .unwrap_or(1_000.0);
        let Some(array) = descendant
            .get_deref(b"W", document)
            .and_then(Object::as_array)
            .ok()
        else {
            return (HashMap::new(), default_width);
        };
        let mut widths = HashMap::new();
        let mut position = 0usize;
        while position < array.len() {
            let Some(first_code) =
                integer(array.get(position)).and_then(|value| u32::try_from(value).ok())
            else {
                break;
            };
            position += 1;
            let Some(next) = array.get(position) else {
                break;
            };
            if let Ok(values) = next.as_array() {
                for (offset, value) in values.iter().enumerate() {
                    if let Some(width) = number(Some(value)) {
                        widths.insert(first_code.saturating_add(offset as u32), width);
                    }
                }
                position += 1;
            } else {
                let Some(last_code) =
                    integer(Some(next)).and_then(|value| u32::try_from(value).ok())
                else {
                    break;
                };
                let Some(width) = array
                    .get(position + 1)
                    .and_then(|value| number(Some(value)))
                else {
                    break;
                };
                if last_code >= first_code && last_code - first_code <= 65_536 {
                    for code in first_code..=last_code {
                        widths.insert(code, width);
                    }
                }
                position += 2;
            }
        }
        return (widths, default_width);
    }
    let first = dictionary
        .get(b"FirstChar")
        .ok()
        .and_then(|value| integer(Some(value)))
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let widths = dictionary
        .get_deref(b"Widths", document)
        .and_then(Object::as_array)
        .ok()
        .map(|array| {
            array
                .iter()
                .enumerate()
                .filter_map(|(offset, value)| {
                    number(Some(value)).map(|width| (first.saturating_add(offset as u32), width))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let default_width = dictionary
        .get_deref(b"FontDescriptor", document)
        .and_then(Object::as_dict)
        .and_then(|descriptor| descriptor.get(b"MissingWidth"))
        .ok()
        .and_then(|value| number(Some(value)))
        .unwrap_or(500.0);
    (widths, default_width)
}

fn pdf_font_descriptor<'a>(
    document: &'a Document,
    dictionary: &'a Dictionary,
    subtype: &[u8],
) -> Option<&'a Dictionary> {
    if subtype == b"Type0" {
        return dictionary
            .get_deref(b"DescendantFonts", document)
            .and_then(Object::as_array)
            .ok()
            .and_then(|array| array.first())
            .and_then(|object| document.dereference(object).ok().map(|(_, value)| value))
            .and_then(|object| object.as_dict().ok())
            .and_then(|descendant| {
                descendant
                    .get_deref(b"FontDescriptor", document)
                    .and_then(Object::as_dict)
                    .ok()
            });
    }
    dictionary
        .get_deref(b"FontDescriptor", document)
        .and_then(Object::as_dict)
        .ok()
}

fn pdf_encoding_glyph_names(document: &Document, dictionary: &Dictionary) -> HashMap<u8, String> {
    let Some(encoding) = dictionary
        .get(b"Encoding")
        .ok()
        .and_then(|value| document.dereference(value).ok().map(|(_, value)| value))
        .cloned()
    else {
        return HashMap::new();
    };
    let base_name = match &encoding {
        Object::Name(name) => name.as_slice(),
        Object::Dictionary(dictionary) => dictionary
            .get(b"BaseEncoding")
            .and_then(Object::as_name)
            .unwrap_or(b"StandardEncoding"),
        _ => b"StandardEncoding",
    };
    let mut result = pdf_named_encoding(base_name)
        .iter()
        .enumerate()
        .filter(|(_, name)| **name != ".notdef")
        .map(|(code, name)| (code as u8, (*name).to_owned()))
        .collect::<HashMap<_, _>>();
    let Some(differences) = encoding.as_dict().ok().and_then(|dictionary| {
        dictionary
            .get(b"Differences")
            .and_then(Object::as_array)
            .ok()
    }) else {
        return result;
    };
    let mut code = 0u16;
    for item in differences {
        match item {
            Object::Integer(value) => {
                code = u16::try_from(*value).unwrap_or(0).min(255);
            }
            Object::Name(name) => {
                result.insert(code as u8, String::from_utf8_lossy(name).into_owned());
                code = code.saturating_add(1).min(255);
            }
            _ => {}
        }
    }
    result
}

fn pdf_named_encoding(name: &[u8]) -> &'static [&'static str; 256] {
    match name {
        b"MacRomanEncoding" => &stet_fonts::encoding::MACROMAN_ENCODING,
        b"WinAnsiEncoding" => &stet_fonts::encoding::WINANSI_ENCODING,
        b"SymbolEncoding" => &stet_fonts::encoding::SYMBOL_ENCODING,
        b"ZapfDingbatsEncoding" => &stet_fonts::encoding::ZAPFDINGBATS_ENCODING,
        _ => &stet_fonts::encoding::STANDARD_ENCODING,
    }
}

fn parse_to_unicode_cmap(bytes: &[u8]) -> HashMap<Vec<u8>, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut result = HashMap::new();
    let mut mode = "";
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with("beginbfchar") {
            mode = "bfchar";
            continue;
        }
        if trimmed.ends_with("endbfchar") {
            mode = "";
            continue;
        }
        if trimmed.ends_with("beginbfrange") {
            mode = "bfrange";
            continue;
        }
        if trimmed.ends_with("endbfrange") {
            mode = "";
            continue;
        }
        let tokens = hex_tokens(trimmed);
        if mode == "bfchar" && tokens.len() >= 2 {
            if let (Some(source), Some(target)) =
                (hex_bytes(&tokens[0]), unicode_from_hex(&tokens[1]))
            {
                result.insert(source, target);
            }
        } else if mode == "bfrange" && tokens.len() >= 3 {
            let (Some(start), Some(end)) = (hex_bytes(&tokens[0]), hex_bytes(&tokens[1])) else {
                continue;
            };
            if start.len() != end.len() || start.len() > 4 {
                continue;
            }
            let start_value = bytes_to_u32(&start);
            let end_value = bytes_to_u32(&end);
            if end_value < start_value || end_value - start_value > 65_536 {
                continue;
            }
            if tokens.len() == 3 {
                if let Some(target_bytes) = hex_bytes(&tokens[2]) {
                    let target_value = bytes_to_u32(&target_bytes);
                    for offset in 0..=end_value - start_value {
                        let source = u32_to_bytes(start_value + offset, start.len());
                        let target = u32_to_bytes(target_value + offset, target_bytes.len());
                        if let Some(unicode) = unicode_from_bytes(&target) {
                            result.insert(source, unicode);
                        }
                    }
                }
            } else {
                for (offset, token) in tokens.iter().skip(2).enumerate() {
                    if start_value + offset as u32 > end_value {
                        break;
                    }
                    if let Some(unicode) = unicode_from_hex(token) {
                        result.insert(
                            u32_to_bytes(start_value + offset as u32, start.len()),
                            unicode,
                        );
                    }
                }
            }
        }
    }
    result
}

fn hex_tokens(line: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = None;
    for (index, character) in line.char_indices() {
        if character == '<' {
            start = Some(index + 1);
        } else if character == '>'
            && let Some(begin) = start.take()
        {
            result.push(line[begin..index].to_owned());
        }
    }
    result
}

fn hex_bytes(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect()
}

fn unicode_from_hex(value: &str) -> Option<String> {
    unicode_from_bytes(&hex_bytes(value)?)
}

fn unicode_from_bytes(bytes: &[u8]) -> Option<String> {
    if !bytes.len().is_multiple_of(2) {
        return String::from_utf8(bytes.to_vec()).ok();
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
    Some(
        char::decode_utf16(units)
            .map(|item| item.unwrap_or('\u{fffd}'))
            .collect(),
    )
}

fn bytes_to_u32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |value, byte| {
        value.saturating_mul(256).saturating_add(u32::from(*byte))
    })
}

fn u32_to_bytes(value: u32, length: usize) -> Vec<u8> {
    value.to_be_bytes()[4usize.saturating_sub(length)..].to_vec()
}

fn evaluate_pdf_function(
    document: &Document,
    object: &Object,
    input: f64,
    depth: usize,
) -> Option<Vec<f64>> {
    evaluate_pdf_function_inputs(document, object, &[input], depth)
}

fn soft_mask_transfer_values(document: &Document, object: &Object) -> Option<Vec<f64>> {
    let (_, object) = document.dereference(object).ok()?;
    if matches!(object, Object::Name(name) if matches!(name.as_slice(), b"Identity" | b"Default")) {
        return Some(Vec::new());
    }
    // Keep the table compact: librsvg and several Office SVG consumers fall
    // back to identity for very large feComponentTransfer tables.
    (0..=32)
        .map(|index| {
            evaluate_pdf_function(document, object, index as f64 / 32.0, 0)
                .and_then(|values| values.first().copied())
                .filter(|value| value.is_finite())
                .map(|value| value.clamp(0.0, 1.0))
        })
        .collect()
}

fn evaluate_pdf_function_inputs(
    document: &Document,
    object: &Object,
    inputs: &[f64],
    depth: usize,
) -> Option<Vec<f64>> {
    if depth > 16 {
        return None;
    }
    let (_, object) = document.dereference(object).ok()?;
    if let Object::Array(functions) = object {
        return functions
            .iter()
            .map(|function| {
                evaluate_pdf_function_inputs(document, function, inputs, depth + 1)
                    .and_then(|values| values.first().copied())
            })
            .collect();
    }
    let dictionary = match object {
        Object::Dictionary(dictionary) => dictionary,
        Object::Stream(stream) => &stream.dict,
        _ => return None,
    };
    let domain = dictionary
        .get(b"Domain")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, usize::MAX))
        .filter(|values| values.len() >= inputs.len() * 2)?;
    let inputs = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            input.clamp(
                domain[index * 2].min(domain[index * 2 + 1]),
                domain[index * 2].max(domain[index * 2 + 1]),
            )
        })
        .collect::<Vec<_>>();
    let mut output = match dictionary
        .get(b"FunctionType")
        .and_then(Object::as_i64)
        .ok()?
    {
        0 => {
            let Object::Stream(stream) = object else {
                return None;
            };
            evaluate_sampled_function(stream, dictionary, &domain, &inputs)?
        }
        2 => {
            let input = *inputs.first()?;
            let c0 = dictionary
                .get(b"C0")
                .and_then(Object::as_array)
                .ok()
                .map(|values| numbers(values, usize::MAX))
                .unwrap_or_else(|| vec![0.0]);
            let c1 = dictionary
                .get(b"C1")
                .and_then(Object::as_array)
                .ok()
                .map(|values| numbers(values, usize::MAX))
                .unwrap_or_else(|| vec![1.0]);
            let exponent = dictionary
                .get(b"N")
                .ok()
                .and_then(|value| number(Some(value)))
                .unwrap_or(1.0);
            let denominator = domain[1] - domain[0];
            let normalized = if denominator.abs() <= 1e-15 {
                0.0
            } else {
                ((input - domain[0]) / denominator).clamp(0.0, 1.0)
            };
            let factor = normalized.powf(exponent);
            let count = c0.len().max(c1.len());
            (0..count)
                .map(|index| {
                    let first = c0.get(index).copied().unwrap_or(0.0);
                    let second = c1.get(index).copied().unwrap_or(first);
                    first + factor * (second - first)
                })
                .collect()
        }
        3 => {
            let input = *inputs.first()?;
            let functions = dictionary
                .get(b"Functions")
                .and_then(Object::as_array)
                .ok()?;
            if functions.is_empty() {
                return None;
            }
            let bounds = dictionary
                .get(b"Bounds")
                .and_then(Object::as_array)
                .ok()
                .map(|values| numbers(values, usize::MAX))
                .unwrap_or_default();
            let encode = dictionary
                .get(b"Encode")
                .and_then(Object::as_array)
                .ok()
                .map(|values| numbers(values, usize::MAX))
                .unwrap_or_else(|| vec![0.0, 1.0]);
            let segment = bounds
                .iter()
                .position(|bound| input < *bound)
                .unwrap_or(functions.len() - 1)
                .min(functions.len() - 1);
            let lower = if segment == 0 {
                domain[0]
            } else {
                bounds.get(segment - 1).copied().unwrap_or(domain[0])
            };
            let upper = bounds.get(segment).copied().unwrap_or(domain[1]);
            let encoded_start = encode.get(segment * 2).copied().unwrap_or(0.0);
            let encoded_end = encode.get(segment * 2 + 1).copied().unwrap_or(1.0);
            let mapped = if (upper - lower).abs() <= 1e-15 {
                encoded_start
            } else {
                encoded_start + (input - lower) / (upper - lower) * (encoded_end - encoded_start)
            };
            evaluate_pdf_function_inputs(document, &functions[segment], &[mapped], depth + 1)?
        }
        4 => {
            let Object::Stream(stream) = object else {
                return None;
            };
            const MAX_CALCULATOR_BYTES: usize = 1024 * 1024;
            let source = stream
                .decompressed_content_with_limit(MAX_CALCULATOR_BYTES)
                .ok()?;
            evaluate_calculator_function(&source, &inputs)?
        }
        _ => return None,
    };
    if let Ok(range) = dictionary.get(b"Range").and_then(Object::as_array) {
        let range = numbers(range, usize::MAX);
        for (index, value) in output.iter_mut().enumerate() {
            if let (Some(minimum), Some(maximum)) = (range.get(index * 2), range.get(index * 2 + 1))
            {
                *value = value.clamp(minimum.min(*maximum), minimum.max(*maximum));
            }
        }
    }
    Some(output)
}

fn evaluate_sampled_function(
    stream: &Stream,
    dictionary: &Dictionary,
    domain: &[f64],
    inputs: &[f64],
) -> Option<Vec<f64>> {
    if dictionary
        .get(b"Order")
        .ok()
        .and_then(|value| integer(Some(value)))
        .unwrap_or(1)
        != 1
    {
        return None;
    }
    let sizes = dictionary
        .get(b"Size")
        .and_then(Object::as_array)
        .ok()?
        .iter()
        .map(|value| usize::try_from(integer(Some(value))?).ok())
        .collect::<Option<Vec<_>>>()?;
    if sizes.len() != inputs.len() || sizes.is_empty() || sizes.len() > 8 || sizes.contains(&0) {
        return None;
    }
    let bits_per_sample = dictionary
        .get(b"BitsPerSample")
        .ok()
        .and_then(|value| integer(Some(value)))
        .and_then(|value| usize::try_from(value).ok())?;
    if !matches!(bits_per_sample, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) {
        return None;
    }
    let range = dictionary
        .get(b"Range")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, usize::MAX))?;
    if range.is_empty() || !range.len().is_multiple_of(2) {
        return None;
    }
    let output_count = range.len() / 2;
    let encode = dictionary
        .get(b"Encode")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, usize::MAX))
        .filter(|values| values.len() >= inputs.len() * 2)
        .unwrap_or_else(|| {
            sizes
                .iter()
                .flat_map(|size| [0.0, size.saturating_sub(1) as f64])
                .collect()
        });
    let decode = dictionary
        .get(b"Decode")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, usize::MAX))
        .filter(|values| values.len() >= output_count * 2)
        .unwrap_or_else(|| range.clone());
    let mut encoded = Vec::with_capacity(inputs.len());
    for index in 0..inputs.len() {
        let denominator = domain[index * 2 + 1] - domain[index * 2];
        let value = if denominator.abs() <= 1e-15 {
            encode[index * 2]
        } else {
            encode[index * 2]
                + (inputs[index] - domain[index * 2]) / denominator
                    * (encode[index * 2 + 1] - encode[index * 2])
        };
        encoded.push(value.clamp(0.0, sizes[index].saturating_sub(1) as f64));
    }
    let sample_count = sizes
        .iter()
        .try_fold(1usize, |count, size| count.checked_mul(*size))?;
    let required_bits = sample_count
        .checked_mul(output_count)?
        .checked_mul(bits_per_sample)?;
    const MAX_SAMPLED_FUNCTION_BYTES: usize = 64 * 1024 * 1024;
    let source = stream
        .decompressed_content_with_limit(MAX_SAMPLED_FUNCTION_BYTES)
        .ok()?;
    if required_bits > source.len().checked_mul(8)? {
        return None;
    }
    let largest_sample = (1u64 << bits_per_sample) - 1;
    let corner_count = 1usize.checked_shl(u32::try_from(inputs.len()).ok()?)?;
    let mut output = vec![0.0; output_count];
    for corner in 0..corner_count {
        let mut table_index = 0usize;
        let mut stride = 1usize;
        let mut weight = 1.0;
        for dimension in 0..inputs.len() {
            let lower = encoded[dimension].floor() as usize;
            let upper = (lower + 1).min(sizes[dimension] - 1);
            let fraction = encoded[dimension] - lower as f64;
            let use_upper = corner & (1usize << dimension) != 0;
            let coordinate = if use_upper { upper } else { lower };
            weight *= if use_upper { fraction } else { 1.0 - fraction };
            table_index = table_index.checked_add(coordinate.checked_mul(stride)?)?;
            stride = stride.checked_mul(sizes[dimension])?;
        }
        if weight <= 0.0 {
            continue;
        }
        for component in 0..output_count {
            let sample_index = table_index
                .checked_mul(output_count)?
                .checked_add(component)?;
            let raw = read_packed_sample(&source, sample_index, bits_per_sample)?;
            let normalized = raw as f64 / largest_sample as f64;
            let decoded = decode[component * 2]
                + normalized * (decode[component * 2 + 1] - decode[component * 2]);
            output[component] += decoded * weight;
        }
    }
    Some(output)
}

fn read_packed_sample(data: &[u8], sample_index: usize, bits_per_sample: usize) -> Option<u64> {
    let bit_start = sample_index.checked_mul(bits_per_sample)?;
    let bit_end = bit_start.checked_add(bits_per_sample)?;
    if bit_end > data.len().checked_mul(8)? {
        return None;
    }
    let mut value = 0u64;
    for bit_position in bit_start..bit_end {
        let byte = data[bit_position / 8];
        let bit = (byte >> (7 - bit_position % 8)) & 1;
        value = (value << 1) | u64::from(bit);
    }
    Some(value)
}

#[derive(Clone, Debug)]
enum CalculatorToken {
    Number(f64),
    Boolean(bool),
    Procedure(Vec<CalculatorToken>),
    Operator(String),
}

#[derive(Clone, Debug)]
enum CalculatorValue {
    Number(f64),
    Boolean(bool),
    Procedure(Vec<CalculatorToken>),
}

fn evaluate_calculator_function(source: &[u8], inputs: &[f64]) -> Option<Vec<f64>> {
    let source = std::str::from_utf8(source).ok()?;
    let mut position = 0usize;
    let tokens = parse_calculator_tokens(source, &mut position, false)?;
    let tokens = match tokens.as_slice() {
        [CalculatorToken::Procedure(body)] => body.as_slice(),
        _ => tokens.as_slice(),
    };
    let mut stack = inputs
        .iter()
        .copied()
        .map(CalculatorValue::Number)
        .collect::<Vec<_>>();
    let mut operations = 0usize;
    execute_calculator_tokens(tokens, &mut stack, &mut operations, 0)?;
    stack
        .into_iter()
        .map(|value| match value {
            CalculatorValue::Number(value) if value.is_finite() => Some(value),
            _ => None,
        })
        .collect()
}

fn parse_calculator_tokens(
    source: &str,
    position: &mut usize,
    procedure: bool,
) -> Option<Vec<CalculatorToken>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    while *position < bytes.len() {
        while *position < bytes.len() {
            if bytes[*position] == b'%' {
                while *position < bytes.len() && !matches!(bytes[*position], b'\n' | b'\r') {
                    *position += 1;
                }
            } else if bytes[*position].is_ascii_whitespace() {
                *position += 1;
            } else {
                break;
            }
        }
        if *position >= bytes.len() {
            return (!procedure).then_some(tokens);
        }
        match bytes[*position] {
            b'{' => {
                *position += 1;
                tokens.push(CalculatorToken::Procedure(parse_calculator_tokens(
                    source, position, true,
                )?));
            }
            b'}' => {
                *position += 1;
                return procedure.then_some(tokens);
            }
            _ => {
                let start = *position;
                while *position < bytes.len()
                    && !bytes[*position].is_ascii_whitespace()
                    && !matches!(bytes[*position], b'{' | b'}' | b'%')
                {
                    *position += 1;
                }
                if start == *position {
                    return None;
                }
                let word = &source[start..*position];
                let token = if word == "true" {
                    CalculatorToken::Boolean(true)
                } else if word == "false" {
                    CalculatorToken::Boolean(false)
                } else if let Ok(value) = word.parse::<f64>() {
                    CalculatorToken::Number(value)
                } else {
                    CalculatorToken::Operator(word.to_owned())
                };
                tokens.push(token);
            }
        }
    }
    (!procedure).then_some(tokens)
}

fn execute_calculator_tokens(
    tokens: &[CalculatorToken],
    stack: &mut Vec<CalculatorValue>,
    operations: &mut usize,
    depth: usize,
) -> Option<()> {
    const MAX_OPERATIONS: usize = 100_000;
    const MAX_STACK: usize = 4_096;
    if depth > 64 {
        return None;
    }
    for token in tokens {
        *operations += 1;
        if *operations > MAX_OPERATIONS || stack.len() > MAX_STACK {
            return None;
        }
        match token {
            CalculatorToken::Number(value) => stack.push(CalculatorValue::Number(*value)),
            CalculatorToken::Boolean(value) => stack.push(CalculatorValue::Boolean(*value)),
            CalculatorToken::Procedure(body) => {
                stack.push(CalculatorValue::Procedure(body.clone()));
            }
            CalculatorToken::Operator(operator) => {
                execute_calculator_operator(operator, stack, operations, depth)?;
            }
        }
    }
    Some(())
}

fn calculator_number(stack: &mut Vec<CalculatorValue>) -> Option<f64> {
    match stack.pop()? {
        CalculatorValue::Number(value) => Some(value),
        _ => None,
    }
}

fn calculator_boolean(stack: &mut Vec<CalculatorValue>) -> Option<bool> {
    match stack.pop()? {
        CalculatorValue::Boolean(value) => Some(value),
        _ => None,
    }
}

fn calculator_procedure(stack: &mut Vec<CalculatorValue>) -> Option<Vec<CalculatorToken>> {
    match stack.pop()? {
        CalculatorValue::Procedure(value) => Some(value),
        _ => None,
    }
}

fn calculator_integer(value: f64) -> Option<i64> {
    (value.is_finite() && value >= i64::MIN as f64 && value <= i64::MAX as f64)
        .then_some(value.trunc() as i64)
}

fn execute_calculator_operator(
    operator: &str,
    stack: &mut Vec<CalculatorValue>,
    operations: &mut usize,
    depth: usize,
) -> Option<()> {
    let unary_number = |stack: &mut Vec<CalculatorValue>, function: fn(f64) -> f64| {
        let value = calculator_number(stack)?;
        let result = function(value);
        result
            .is_finite()
            .then(|| stack.push(CalculatorValue::Number(result)))
    };
    let binary_number = |stack: &mut Vec<CalculatorValue>, function: fn(f64, f64) -> f64| {
        let second = calculator_number(stack)?;
        let first = calculator_number(stack)?;
        let result = function(first, second);
        result
            .is_finite()
            .then(|| stack.push(CalculatorValue::Number(result)))
    };
    match operator {
        "abs" => unary_number(stack, f64::abs)?,
        "add" => binary_number(stack, |a, b| a + b)?,
        "atan" => {
            let denominator = calculator_number(stack)?;
            let numerator = calculator_number(stack)?;
            stack.push(CalculatorValue::Number(
                numerator.atan2(denominator).to_degrees().rem_euclid(360.0),
            ));
        }
        "ceiling" => unary_number(stack, f64::ceil)?,
        "cos" => unary_number(stack, |value| value.to_radians().cos())?,
        "cvi" | "truncate" => unary_number(stack, f64::trunc)?,
        "cvr" => unary_number(stack, |value| value)?,
        "div" => binary_number(stack, |a, b| a / b)?,
        "exp" => binary_number(stack, f64::powf)?,
        "floor" => unary_number(stack, f64::floor)?,
        "idiv" => {
            let second = calculator_integer(calculator_number(stack)?)?;
            let first = calculator_integer(calculator_number(stack)?)?;
            stack.push(CalculatorValue::Number((first.checked_div(second)?) as f64));
        }
        "ln" => unary_number(stack, f64::ln)?,
        "log" => unary_number(stack, f64::log10)?,
        "mod" => {
            let second = calculator_integer(calculator_number(stack)?)?;
            let first = calculator_integer(calculator_number(stack)?)?;
            stack.push(CalculatorValue::Number((first.checked_rem(second)?) as f64));
        }
        "mul" => binary_number(stack, |a, b| a * b)?,
        "neg" => unary_number(stack, |value| -value)?,
        "round" => unary_number(stack, f64::round)?,
        "sin" => unary_number(stack, |value| value.to_radians().sin())?,
        "sqrt" => unary_number(stack, f64::sqrt)?,
        "sub" => binary_number(stack, |a, b| a - b)?,
        "eq" | "ne" => {
            let second = stack.pop()?;
            let first = stack.pop()?;
            let equal = match (first, second) {
                (CalculatorValue::Number(a), CalculatorValue::Number(b)) => a == b,
                (CalculatorValue::Boolean(a), CalculatorValue::Boolean(b)) => a == b,
                _ => false,
            };
            stack.push(CalculatorValue::Boolean(if operator == "eq" {
                equal
            } else {
                !equal
            }));
        }
        "ge" | "gt" | "le" | "lt" => {
            let second = calculator_number(stack)?;
            let first = calculator_number(stack)?;
            let value = match operator {
                "ge" => first >= second,
                "gt" => first > second,
                "le" => first <= second,
                _ => first < second,
            };
            stack.push(CalculatorValue::Boolean(value));
        }
        "and" | "or" | "xor" => {
            let second = stack.pop()?;
            let first = stack.pop()?;
            let value = match (first, second) {
                (CalculatorValue::Boolean(a), CalculatorValue::Boolean(b)) => {
                    CalculatorValue::Boolean(match operator {
                        "and" => a && b,
                        "or" => a || b,
                        _ => a ^ b,
                    })
                }
                (CalculatorValue::Number(a), CalculatorValue::Number(b)) => {
                    let a = calculator_integer(a)?;
                    let b = calculator_integer(b)?;
                    CalculatorValue::Number(match operator {
                        "and" => a & b,
                        "or" => a | b,
                        _ => a ^ b,
                    } as f64)
                }
                _ => return None,
            };
            stack.push(value);
        }
        "bitshift" => {
            let shift = calculator_integer(calculator_number(stack)?)?;
            let value = calculator_integer(calculator_number(stack)?)?;
            let result = if shift >= 0 {
                value.checked_shl(u32::try_from(shift).ok()?)?
            } else {
                value.checked_shr(u32::try_from(-shift).ok()?)?
            };
            stack.push(CalculatorValue::Number(result as f64));
        }
        "not" => match stack.pop()? {
            CalculatorValue::Boolean(value) => stack.push(CalculatorValue::Boolean(!value)),
            CalculatorValue::Number(value) => stack.push(CalculatorValue::Number(
                (!calculator_integer(value)?) as f64,
            )),
            _ => return None,
        },
        "copy" => {
            let count = usize::try_from(calculator_integer(calculator_number(stack)?)?).ok()?;
            if count > stack.len() || stack.len() + count > 4_096 {
                return None;
            }
            stack.extend_from_within(stack.len() - count..);
        }
        "dup" => stack.push(stack.last()?.clone()),
        "exch" => {
            let length = stack.len();
            if length < 2 {
                return None;
            }
            stack.swap(length - 1, length - 2);
        }
        "index" => {
            let index = usize::try_from(calculator_integer(calculator_number(stack)?)?).ok()?;
            let value = stack.get(stack.len().checked_sub(index + 1)?)?.clone();
            stack.push(value);
        }
        "pop" => {
            stack.pop()?;
        }
        "roll" => {
            let amount = calculator_integer(calculator_number(stack)?)?;
            let count = usize::try_from(calculator_integer(calculator_number(stack)?)?).ok()?;
            if count > stack.len() || count == 0 {
                return None;
            }
            let start = stack.len() - count;
            stack[start..].rotate_right(amount.rem_euclid(count as i64) as usize);
        }
        "if" => {
            let procedure = calculator_procedure(stack)?;
            if calculator_boolean(stack)? {
                execute_calculator_tokens(&procedure, stack, operations, depth + 1)?;
            }
        }
        "ifelse" => {
            let alternative = calculator_procedure(stack)?;
            let consequent = calculator_procedure(stack)?;
            let procedure = if calculator_boolean(stack)? {
                consequent
            } else {
                alternative
            };
            execute_calculator_tokens(&procedure, stack, operations, depth + 1)?;
        }
        _ => return None,
    }
    Some(())
}

fn apply_gradient_extension(stops: &mut Vec<GradientStop>, extend: [bool; 2]) {
    apply_gradient_extension_with_background(stops, extend, None, 0.0);
}

fn apply_gradient_extension_with_background(
    stops: &mut Vec<GradientStop>,
    extend: [bool; 2],
    background: Option<&str>,
    background_opacity: f64,
) {
    const EDGE_EPSILON: f64 = 1e-6;
    if !extend[0]
        && let Some(first) = stops.first().cloned()
    {
        if let Some(background) = background {
            stops[0].color = background.into();
            stops[0].opacity = background_opacity;
        } else {
            stops[0].opacity = 0.0;
        }
        stops.insert(
            1,
            GradientStop {
                offset: EDGE_EPSILON,
                ..first
            },
        );
    }
    if !extend[1]
        && let Some(last) = stops.last().cloned()
    {
        if let Some(last_stop) = stops.last_mut() {
            if let Some(background) = background {
                last_stop.color = background.into();
                last_stop.opacity = background_opacity;
            } else {
                last_stop.opacity = 0.0;
            }
        }
        stops.insert(
            stops.len().saturating_sub(1),
            GradientStop {
                offset: 1.0 - EDGE_EPSILON,
                ..last
            },
        );
    }
}

#[derive(Clone, Copy)]
struct FunctionShadingBounds {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
}

struct FunctionShadingCell {
    points: [(f64, f64); 4],
    components: Vec<f64>,
}

struct FunctionShadingField<'a> {
    document: &'a Document,
    function: &'a Object,
    transform: Matrix,
}

impl FunctionShadingField<'_> {
    fn sample(&self, x: f64, y: f64) -> Option<Vec<f64>> {
        evaluate_pdf_function_inputs(self.document, self.function, &[x, y], 0)
    }

    fn point(&self, x: f64, y: f64) -> (f64, f64) {
        transform_point(self.transform, x, y)
    }
}

fn adaptive_function_shading_cells(
    field: &FunctionShadingField<'_>,
    bounds: FunctionShadingBounds,
    depth: usize,
    output: &mut Vec<FunctionShadingCell>,
    maximum: usize,
) -> bool {
    if output.len() >= maximum {
        return true;
    }
    let middle_x = (bounds.x0 + bounds.x1) * 0.5;
    let middle_y = (bounds.y0 + bounds.y1) * 0.5;
    let coordinates = [
        (bounds.x0, bounds.y0),
        (middle_x, bounds.y0),
        (bounds.x1, bounds.y0),
        (bounds.x0, middle_y),
        (middle_x, middle_y),
        (bounds.x1, middle_y),
        (bounds.x0, bounds.y1),
        (middle_x, bounds.y1),
        (bounds.x1, bounds.y1),
    ];
    let Some(samples) = coordinates
        .iter()
        .map(|(x, y)| field.sample(*x, *y))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let component_count = samples.iter().map(Vec::len).min().unwrap_or(0);
    if component_count == 0 {
        return false;
    }
    let maximum_range = (0..component_count)
        .map(|component| {
            let minimum = samples
                .iter()
                .map(|sample| sample[component])
                .fold(f64::INFINITY, f64::min);
            let maximum = samples
                .iter()
                .map(|sample| sample[component])
                .fold(f64::NEG_INFINITY, f64::max);
            maximum - minimum
        })
        .fold(0.0_f64, f64::max);
    let interpolation_error = (0..component_count)
        .map(|component| {
            let bilinear = (samples[0][component]
                + samples[2][component]
                + samples[6][component]
                + samples[8][component])
                * 0.25;
            (samples[4][component] - bilinear).abs()
        })
        .fold(0.0_f64, f64::max);
    let u_change = (0..component_count)
        .flat_map(|component| {
            [
                (samples[0][component] - samples[2][component]).abs(),
                (samples[3][component] - samples[5][component]).abs(),
                (samples[6][component] - samples[8][component]).abs(),
            ]
        })
        .fold(0.0_f64, f64::max);
    let v_change = (0..component_count)
        .flat_map(|component| {
            [
                (samples[0][component] - samples[6][component]).abs(),
                (samples[1][component] - samples[7][component]).abs(),
                (samples[2][component] - samples[8][component]).abs(),
            ]
        })
        .fold(0.0_f64, f64::max);
    let points = [
        field.point(bounds.x0, bounds.y0),
        field.point(bounds.x1, bounds.y0),
        field.point(bounds.x1, bounds.y1),
        field.point(bounds.x0, bounds.y1),
    ];
    let u_length = (points[1].0 - points[0].0)
        .hypot(points[1].1 - points[0].1)
        .max((points[2].0 - points[3].0).hypot(points[2].1 - points[3].1));
    let v_length = (points[3].0 - points[0].0)
        .hypot(points[3].1 - points[0].1)
        .max((points[2].0 - points[1].0).hypot(points[2].1 - points[1].1));
    const RANGE_TOLERANCE: f64 = 2.0 / 255.0;
    const INTERPOLATION_TOLERANCE: f64 = 0.75 / 255.0;
    let at_pixel_limit = u_length <= 0.75 && v_length <= 0.75;
    if depth >= 18
        || at_pixel_limit
        || (maximum_range <= RANGE_TOLERANCE && interpolation_error <= INTERPOLATION_TOLERANCE)
    {
        output.push(FunctionShadingCell {
            points,
            components: samples[4][..component_count].to_vec(),
        });
        return true;
    }
    let split_u = if u_change == v_change {
        u_length >= v_length
    } else {
        u_change >= v_change
    };
    if split_u {
        adaptive_function_shading_cells(
            field,
            FunctionShadingBounds {
                x1: middle_x,
                ..bounds
            },
            depth + 1,
            output,
            maximum,
        ) && adaptive_function_shading_cells(
            field,
            FunctionShadingBounds {
                x0: middle_x,
                ..bounds
            },
            depth + 1,
            output,
            maximum,
        )
    } else {
        adaptive_function_shading_cells(
            field,
            FunctionShadingBounds {
                y1: middle_y,
                ..bounds
            },
            depth + 1,
            output,
            maximum,
        ) && adaptive_function_shading_cells(
            field,
            FunctionShadingBounds {
                y0: middle_y,
                ..bounds
            },
            depth + 1,
            output,
            maximum,
        )
    }
}

#[derive(Clone, Copy)]
struct RadialBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

struct RadialCell {
    bounds: RadialBounds,
    components: Vec<f64>,
    coverage: f64,
}

struct NoncontainedRadialField<'a> {
    document: &'a Document,
    function: &'a Object,
    coordinates: [f64; 6],
    domain: [f64; 2],
    extend: [bool; 2],
    inverse: Matrix,
}

impl NoncontainedRadialField<'_> {
    fn sample(&self, output_x: f64, output_y: f64) -> Option<Vec<f64>> {
        let (x, y) = transform_point(self.inverse, output_x, output_y);
        let [x0, y0, radius0, x1, y1, radius1] = self.coordinates;
        let delta_x = x1 - x0;
        let delta_y = y1 - y0;
        let delta_radius = radius1 - radius0;
        let point_x = x - x0;
        let point_y = y - y0;
        let a = delta_x * delta_x + delta_y * delta_y - delta_radius * delta_radius;
        let b = -2.0 * (point_x * delta_x + point_y * delta_y + radius0 * delta_radius);
        let c = point_x * point_x + point_y * point_y - radius0 * radius0;
        let mut roots = Vec::with_capacity(2);
        if a.abs() <= 1e-15 {
            if b.abs() > 1e-15 {
                roots.push(-c / b);
            }
        } else {
            let discriminant = b * b - 4.0 * a * c;
            if discriminant >= -1e-12 {
                let root = discriminant.max(0.0).sqrt();
                roots.push((-b - root) / (2.0 * a));
                roots.push((-b + root) / (2.0 * a));
            }
        }
        let parameter = roots
            .iter()
            .copied()
            .filter(|value| {
                (*value >= -1e-9 && *value <= 1.0 + 1e-9)
                    || (*value < 0.0 && self.extend[0])
                    || (*value > 1.0 && self.extend[1])
            })
            .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))?;
        let parameter = parameter.clamp(0.0, 1.0);
        let function_input = self.domain[0] + parameter * (self.domain[1] - self.domain[0]);
        evaluate_pdf_function(self.document, self.function, function_input, 0)
    }
}

fn adaptive_radial_cells(
    field: &NoncontainedRadialField<'_>,
    bounds: RadialBounds,
    depth: usize,
    output: &mut Vec<RadialCell>,
    maximum: usize,
) {
    if output.len() >= maximum {
        return;
    }
    let sample_points = [
        (0.0, 0.0),
        (0.5, 0.0),
        (1.0, 0.0),
        (0.0, 0.5),
        (0.5, 0.5),
        (1.0, 0.5),
        (0.0, 1.0),
        (0.5, 1.0),
        (1.0, 1.0),
    ];
    let samples = sample_points
        .iter()
        .map(|(x, y)| field.sample(bounds.x + x * bounds.width, bounds.y + y * bounds.height))
        .collect::<Vec<_>>();
    let opaque = samples
        .iter()
        .filter_map(Option::as_ref)
        .collect::<Vec<_>>();
    if opaque.is_empty() {
        return;
    }
    let component_count = opaque[0].len();
    let mut maximum_error = 0.0_f64;
    for component in 0..component_count {
        let minimum = opaque
            .iter()
            .map(|sample| sample.get(component).copied().unwrap_or(0.0))
            .fold(f64::INFINITY, f64::min);
        let maximum = opaque
            .iter()
            .map(|sample| sample.get(component).copied().unwrap_or(0.0))
            .fold(f64::NEG_INFINITY, f64::max);
        maximum_error = maximum_error.max(maximum - minimum);
    }
    let mixed_coverage = opaque.len() != samples.len();
    if depth < 5 && (mixed_coverage || maximum_error > 2.0 / 255.0) {
        let half_width = bounds.width / 2.0;
        let half_height = bounds.height / 2.0;
        for (x_offset, y_offset) in [
            (0.0, 0.0),
            (half_width, 0.0),
            (0.0, half_height),
            (half_width, half_height),
        ] {
            adaptive_radial_cells(
                field,
                RadialBounds {
                    x: bounds.x + x_offset,
                    y: bounds.y + y_offset,
                    width: half_width,
                    height: half_height,
                },
                depth + 1,
                output,
                maximum,
            );
        }
        return;
    }
    let components = (0..component_count)
        .map(|component| {
            opaque
                .iter()
                .map(|sample| sample.get(component).copied().unwrap_or(0.0))
                .sum::<f64>()
                / opaque.len() as f64
        })
        .collect();
    output.push(RadialCell {
        bounds,
        components,
        coverage: opaque.len() as f64 / samples.len() as f64,
    });
}

fn inverse_matrix(matrix: Matrix) -> Option<Matrix> {
    let [a, b, c, d, e, f] = matrix;
    let determinant = a * d - b * c;
    if determinant.abs() <= 1e-15 {
        return None;
    }
    Some([
        d / determinant,
        -b / determinant,
        -c / determinant,
        a / determinant,
        (c * f - d * e) / determinant,
        (b * e - a * f) / determinant,
    ])
}

fn complete_mesh_decode_ranges(decode: &[f64], component_count: usize) -> Option<Vec<f64>> {
    let required = 4usize.checked_add(component_count.checked_mul(2)?)?;
    if decode.len() >= required {
        return Some(decode[..required].to_vec());
    }
    if decode.len() < 6 || !(decode.len() - 4).is_multiple_of(2) {
        return None;
    }
    let mut completed = decode.to_vec();
    let last_pair = [decode[decode.len() - 2], decode[decode.len() - 1]];
    while completed.len() < required {
        completed.extend_from_slice(&last_pair);
    }
    Some(completed)
}

#[derive(Clone, Debug)]
struct MeshVertex {
    x: f64,
    y: f64,
    components: Vec<f64>,
}

#[derive(Clone, Debug)]
struct MeshTriangle([MeshVertex; 3]);

impl MeshTriangle {
    fn average_components(&self) -> Vec<f64> {
        let count = self.0[0].components.len();
        (0..count)
            .map(|index| {
                self.0
                    .iter()
                    .map(|vertex| vertex.components.get(index).copied().unwrap_or(0.0))
                    .sum::<f64>()
                    / 3.0
            })
            .collect()
    }

    fn path_data(&self) -> String {
        format!(
            "M {} {} L {} {} L {} {} Z",
            fmt(self.0[0].x),
            fmt(self.0[0].y),
            fmt(self.0[1].x),
            fmt(self.0[1].y),
            fmt(self.0[2].x),
            fmt(self.0[2].y)
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_mesh_triangles(
    data: &[u8],
    shading_type: i64,
    coordinate_bits: usize,
    component_bits: usize,
    flag_bits: usize,
    component_count: usize,
    decode: &[f64],
    vertices_per_row: usize,
) -> Vec<MeshTriangle> {
    let mut reader = MeshBitReader::new(data);
    if shading_type == 4 {
        let mut triangles = Vec::new();
        let mut previous = None::<MeshTriangle>;
        while let Some((flag, vertex)) = read_mesh_vertex(
            &mut reader,
            flag_bits,
            coordinate_bits,
            component_bits,
            component_count,
            decode,
        ) {
            let triangle = if flag == 0 || previous.is_none() {
                let Some((_, second)) = read_mesh_vertex(
                    &mut reader,
                    flag_bits,
                    coordinate_bits,
                    component_bits,
                    component_count,
                    decode,
                ) else {
                    break;
                };
                let Some((_, third)) = read_mesh_vertex(
                    &mut reader,
                    flag_bits,
                    coordinate_bits,
                    component_bits,
                    component_count,
                    decode,
                ) else {
                    break;
                };
                MeshTriangle([vertex, second, third])
            } else {
                let Some(previous) = previous.as_ref() else {
                    continue;
                };
                if flag == 1 {
                    MeshTriangle([previous.0[1].clone(), previous.0[2].clone(), vertex])
                } else {
                    MeshTriangle([previous.0[0].clone(), previous.0[2].clone(), vertex])
                }
            };
            previous = Some(triangle.clone());
            triangles.push(triangle);
        }
        return triangles;
    }
    if shading_type == 5 && vertices_per_row >= 2 {
        let mut vertices = Vec::new();
        while let Some((_, vertex)) = read_mesh_vertex(
            &mut reader,
            0,
            coordinate_bits,
            component_bits,
            component_count,
            decode,
        ) {
            vertices.push(vertex);
        }
        let rows = vertices.len() / vertices_per_row;
        let mut triangles = Vec::new();
        for row in 0..rows.saturating_sub(1) {
            for column in 0..vertices_per_row - 1 {
                let upper_left = vertices[row * vertices_per_row + column].clone();
                let upper_right = vertices[row * vertices_per_row + column + 1].clone();
                let lower_left = vertices[(row + 1) * vertices_per_row + column].clone();
                let lower_right = vertices[(row + 1) * vertices_per_row + column + 1].clone();
                triangles.push(MeshTriangle([
                    upper_left,
                    upper_right.clone(),
                    lower_left.clone(),
                ]));
                triangles.push(MeshTriangle([upper_right, lower_right, lower_left]));
            }
        }
        return triangles;
    }
    Vec::new()
}

fn read_mesh_vertex(
    reader: &mut MeshBitReader<'_>,
    flag_bits: usize,
    coordinate_bits: usize,
    component_bits: usize,
    component_count: usize,
    decode: &[f64],
) -> Option<(u32, MeshVertex)> {
    let flag = if flag_bits == 0 {
        0
    } else {
        reader.read(flag_bits)? as u32
    };
    let x = decode_mesh_value(
        reader.read(coordinate_bits)?,
        coordinate_bits,
        decode[0],
        decode[1],
    );
    let y = decode_mesh_value(
        reader.read(coordinate_bits)?,
        coordinate_bits,
        decode[2],
        decode[3],
    );
    let mut components = Vec::with_capacity(component_count);
    for index in 0..component_count {
        components.push(decode_mesh_value(
            reader.read(component_bits)?,
            component_bits,
            decode[4 + index * 2],
            decode[5 + index * 2],
        ));
    }
    Some((flag, MeshVertex { x, y, components }))
}

fn decode_mesh_value(raw: u64, bits: usize, minimum: f64, maximum: f64) -> f64 {
    let largest = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    if largest == 0 {
        minimum
    } else {
        minimum + raw as f64 / largest as f64 * (maximum - minimum)
    }
}

fn subdivide_mesh_triangle(triangle: &MeshTriangle, divisions: usize) -> Vec<MeshTriangle> {
    let mut output = Vec::with_capacity(divisions * divisions);
    for first in 0..divisions {
        for second in 0..divisions - first {
            let a = mesh_barycentric_vertex(triangle, first, second, divisions);
            let b = mesh_barycentric_vertex(triangle, first + 1, second, divisions);
            let c = mesh_barycentric_vertex(triangle, first, second + 1, divisions);
            output.push(MeshTriangle([a, b.clone(), c.clone()]));
            if first + second + 1 < divisions {
                let d = mesh_barycentric_vertex(triangle, first + 1, second + 1, divisions);
                output.push(MeshTriangle([b, d, c]));
            }
        }
    }
    output
}

fn mesh_barycentric_vertex(
    triangle: &MeshTriangle,
    first: usize,
    second: usize,
    divisions: usize,
) -> MeshVertex {
    let first_weight = first as f64 / divisions as f64;
    let second_weight = second as f64 / divisions as f64;
    let third_weight = 1.0 - first_weight - second_weight;
    let weights = [third_weight, first_weight, second_weight];
    let component_count = triangle.0[0].components.len();
    MeshVertex {
        x: triangle
            .0
            .iter()
            .zip(weights)
            .map(|(vertex, weight)| vertex.x * weight)
            .sum(),
        y: triangle
            .0
            .iter()
            .zip(weights)
            .map(|(vertex, weight)| vertex.y * weight)
            .sum(),
        components: (0..component_count)
            .map(|index| {
                triangle
                    .0
                    .iter()
                    .zip(weights)
                    .map(|(vertex, weight)| {
                        vertex.components.get(index).copied().unwrap_or(0.0) * weight
                    })
                    .sum()
            })
            .collect(),
    }
}

#[derive(Clone, Debug)]
struct PatchMesh {
    shading_type: i64,
    points: Vec<(f64, f64)>,
    colors: [Vec<f64>; 4],
}

#[allow(clippy::too_many_arguments)]
fn parse_patch_meshes(
    data: &[u8],
    shading_type: i64,
    coordinate_bits: usize,
    component_bits: usize,
    flag_bits: usize,
    component_count: usize,
    decode: &[f64],
) -> (Vec<PatchMesh>, bool) {
    let mut reader = MeshBitReader::new(data);
    let mut patches = Vec::new();
    let mut has_reused_patch = false;
    let point_count = if shading_type == 6 { 12 } else { 16 };
    let mut previous = None::<PatchMesh>;
    loop {
        let Some(flag) = reader.read(flag_bits.max(1)) else {
            break;
        };
        if flag > 3 || (flag != 0 && previous.is_none()) {
            has_reused_patch = true;
            break;
        }
        let mut points = if flag == 0 {
            Vec::with_capacity(point_count)
        } else {
            let previous = previous.as_ref().expect("checked above");
            patch_shared_edge(previous, flag as usize).to_vec()
        };
        let mut complete = true;
        for _ in points.len()..point_count {
            let Some(raw_x) = reader.read(coordinate_bits) else {
                complete = false;
                break;
            };
            let Some(raw_y) = reader.read(coordinate_bits) else {
                complete = false;
                break;
            };
            points.push((
                decode_mesh_value(raw_x, coordinate_bits, decode[0], decode[1]),
                decode_mesh_value(raw_y, coordinate_bits, decode[2], decode[3]),
            ));
        }
        if !complete {
            break;
        }
        let mut colors: [Vec<f64>; 4] = if flag == 0 {
            std::array::from_fn(|_| Vec::new())
        } else {
            let previous = previous.as_ref().expect("checked above");
            let (first, second) = patch_shared_colors(previous, flag as usize);
            [first.clone(), second.clone(), Vec::new(), Vec::new()]
        };
        let color_start = if flag == 0 { 0 } else { 2 };
        for color in &mut colors[color_start..] {
            for component in 0..component_count {
                let Some(raw) = reader.read(component_bits) else {
                    complete = false;
                    break;
                };
                color.push(decode_mesh_value(
                    raw,
                    component_bits,
                    decode[4 + component * 2],
                    decode[5 + component * 2],
                ));
            }
        }
        if !complete {
            break;
        }
        let patch = PatchMesh {
            shading_type,
            points,
            colors,
        };
        previous = Some(patch.clone());
        patches.push(patch);
    }
    (patches, has_reused_patch)
}

fn patch_shared_edge(patch: &PatchMesh, flag: usize) -> [(f64, f64); 4] {
    match flag {
        1 => [
            patch.points[3],
            patch.points[4],
            patch.points[5],
            patch.points[6],
        ],
        2 => [
            patch.points[6],
            patch.points[7],
            patch.points[8],
            patch.points[9],
        ],
        _ => [
            patch.points[9],
            patch.points[10],
            patch.points[11],
            patch.points[0],
        ],
    }
}

fn patch_shared_colors(patch: &PatchMesh, flag: usize) -> (&Vec<f64>, &Vec<f64>) {
    match flag {
        1 => (&patch.colors[1], &patch.colors[2]),
        2 => (&patch.colors[2], &patch.colors[3]),
        _ => (&patch.colors[3], &patch.colors[0]),
    }
}

fn tessellate_patch(patch: &PatchMesh, divisions: usize) -> Vec<MeshTriangle> {
    let mut triangles = Vec::with_capacity(divisions * divisions * 2);
    for u_index in 0..divisions {
        for v_index in 0..divisions {
            let u0 = u_index as f64 / divisions as f64;
            let u1 = (u_index + 1) as f64 / divisions as f64;
            let v0 = v_index as f64 / divisions as f64;
            let v1 = (v_index + 1) as f64 / divisions as f64;
            let lower_left = patch_vertex(patch, u0, v0);
            let lower_right = patch_vertex(patch, u1, v0);
            let upper_left = patch_vertex(patch, u0, v1);
            let upper_right = patch_vertex(patch, u1, v1);
            triangles.push(MeshTriangle([
                lower_left,
                lower_right.clone(),
                upper_left.clone(),
            ]));
            triangles.push(MeshTriangle([lower_right, upper_right, upper_left]));
        }
    }
    triangles
}

fn patch_vertex(patch: &PatchMesh, u: f64, v: f64) -> MeshVertex {
    let (x, y) = if patch.shading_type == 7 {
        tensor_patch_point(&patch.points, u, v)
    } else {
        coons_patch_point(&patch.points, u, v)
    };
    let component_count = patch.colors[0].len();
    let weights = [(1.0 - u) * (1.0 - v), (1.0 - u) * v, u * v, u * (1.0 - v)];
    MeshVertex {
        x,
        y,
        components: (0..component_count)
            .map(|index| {
                patch
                    .colors
                    .iter()
                    .zip(weights)
                    .map(|(color, weight)| color.get(index).copied().unwrap_or(0.0) * weight)
                    .sum()
            })
            .collect(),
    }
}

fn tensor_patch_point(points: &[(f64, f64)], u: f64, v: f64) -> (f64, f64) {
    if points.len() < 16 {
        return (0.0, 0.0);
    }
    let grid = [
        [points[0], points[1], points[2], points[3]],
        [points[11], points[12], points[13], points[4]],
        [points[10], points[15], points[14], points[5]],
        [points[9], points[8], points[7], points[6]],
    ];
    let u_basis = cubic_bernstein(u);
    let v_basis = cubic_bernstein(v);
    let mut x = 0.0;
    let mut y = 0.0;
    for u_index in 0..4 {
        for v_index in 0..4 {
            let weight = u_basis[u_index] * v_basis[v_index];
            x += grid[u_index][v_index].0 * weight;
            y += grid[u_index][v_index].1 * weight;
        }
    }
    (x, y)
}

fn coons_patch_point(points: &[(f64, f64)], u: f64, v: f64) -> (f64, f64) {
    if points.len() < 12 {
        return (0.0, 0.0);
    }
    let left = cubic_bezier_point([points[0], points[1], points[2], points[3]], v);
    let top = cubic_bezier_point([points[3], points[4], points[5], points[6]], u);
    let right = cubic_bezier_point([points[9], points[8], points[7], points[6]], v);
    let bottom = cubic_bezier_point([points[0], points[11], points[10], points[9]], u);
    let bottom_left = points[0];
    let top_left = points[3];
    let top_right = points[6];
    let bottom_right = points[9];
    let bilinear = (
        bottom_left.0 * (1.0 - u) * (1.0 - v)
            + top_left.0 * (1.0 - u) * v
            + top_right.0 * u * v
            + bottom_right.0 * u * (1.0 - v),
        bottom_left.1 * (1.0 - u) * (1.0 - v)
            + top_left.1 * (1.0 - u) * v
            + top_right.1 * u * v
            + bottom_right.1 * u * (1.0 - v),
    );
    (
        (1.0 - u) * left.0 + u * right.0 + (1.0 - v) * bottom.0 + v * top.0 - bilinear.0,
        (1.0 - u) * left.1 + u * right.1 + (1.0 - v) * bottom.1 + v * top.1 - bilinear.1,
    )
}

fn cubic_bezier_point(points: [(f64, f64); 4], value: f64) -> (f64, f64) {
    let basis = cubic_bernstein(value);
    (
        points
            .iter()
            .zip(basis)
            .map(|(point, weight)| point.0 * weight)
            .sum(),
        points
            .iter()
            .zip(basis)
            .map(|(point, weight)| point.1 * weight)
            .sum(),
    )
}

fn cubic_bernstein(value: f64) -> [f64; 4] {
    let inverse = 1.0 - value;
    [
        inverse * inverse * inverse,
        3.0 * value * inverse * inverse,
        3.0 * value * value * inverse,
        value * value * value,
    ]
}

struct MeshBitReader<'a> {
    data: &'a [u8],
    bit_position: usize,
}

impl<'a> MeshBitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_position: 0,
        }
    }

    fn read(&mut self, bits: usize) -> Option<u64> {
        if bits == 0 || bits > 64 || self.bit_position + bits > self.data.len() * 8 {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..bits {
            let byte = self.data[self.bit_position / 8];
            let bit = (byte >> (7 - self.bit_position % 8)) & 1;
            value = (value << 1) | u64::from(bit);
            self.bit_position += 1;
        }
        Some(value)
    }
}

fn pdf_color_component_count(document: &Document, color_space: Option<&Object>) -> usize {
    let color_space =
        color_space.and_then(|value| document.dereference(value).ok().map(|(_, value)| value));
    match color_space {
        Some(Object::Name(name)) if name == b"DeviceGray" || name == b"G" => 1,
        Some(Object::Name(name)) if name == b"DeviceCMYK" => 4,
        Some(Object::Array(array)) => match array.first().and_then(|value| value.as_name().ok()) {
            Some(b"Indexed") => 1,
            Some(b"Separation") => 1,
            Some(b"DeviceN") => array
                .get(1)
                .and_then(|value| value.as_array().ok())
                .map_or(1, Vec::len),
            Some(b"ICCBased") => array
                .get(1)
                .and_then(|value| document.dereference(value).ok().map(|(_, value)| value))
                .and_then(|value| value.as_stream().ok())
                .and_then(|stream| stream.dict.get(b"N").and_then(Object::as_i64).ok())
                .unwrap_or(3)
                .max(1) as usize,
            Some(b"CalGray") => 1,
            Some(b"CalRGB" | b"Lab") => 3,
            Some(b"Pattern") => array
                .get(1)
                .map(|base| pdf_color_component_count(document, Some(base)))
                .unwrap_or(0),
            _ => 3,
        },
        _ => 3,
    }
}

fn pdf_color_space_is_pattern(document: &Document, color_space: &Object) -> bool {
    let color_space = document
        .dereference(color_space)
        .ok()
        .map(|(_, value)| value)
        .unwrap_or(color_space);
    matches!(color_space, Object::Name(name) if name == b"Pattern")
        || matches!(
            color_space,
            Object::Array(array)
                if array.first().and_then(|value| value.as_name().ok()) == Some(b"Pattern")
        )
}

fn components_to_color(
    document: &Document,
    color_space: Option<&Object>,
    components: &[f64],
) -> String {
    let [red, green, blue] = components_to_rgb(document, color_space, components, 0)
        .unwrap_or_else(|| default_rgb_components(components));
    let [red, green, blue] = rgb_to_bytes([red, green, blue]);
    format!("#{red:02X}{green:02X}{blue:02X}")
}

fn components_to_rgb(
    document: &Document,
    color_space: Option<&Object>,
    components: &[f64],
    depth: usize,
) -> Option<[f64; 3]> {
    if depth > 16 {
        return None;
    }
    let color_space =
        color_space.and_then(|value| document.dereference(value).ok().map(|(_, value)| value));
    match color_space {
        Some(Object::Name(name)) if name == b"DeviceGray" || name == b"G" => {
            let gray = components.first().copied().unwrap_or(0.0);
            Some([gray, gray, gray])
        }
        Some(Object::Name(name)) if name == b"DeviceCMYK" || name == b"CMYK" => {
            Some(cmyk_components_to_rgb(components))
        }
        Some(Object::Name(name)) if name == b"DeviceRGB" || name == b"RGB" => {
            Some(default_rgb_components(components))
        }
        Some(Object::Array(array)) => {
            let kind = array.first().and_then(|value| value.as_name().ok())?;
            match kind {
                b"Separation" => {
                    let alternate = array.get(2)?;
                    let function = array.get(3)?;
                    let converted = evaluate_pdf_function_inputs(
                        document,
                        function,
                        &components[..components.len().min(1)],
                        0,
                    )?;
                    components_to_rgb(document, Some(alternate), &converted, depth + 1)
                }
                b"DeviceN" => {
                    let alternate = array.get(2)?;
                    let function = array.get(3)?;
                    let converted =
                        evaluate_pdf_function_inputs(document, function, components, 0)?;
                    components_to_rgb(document, Some(alternate), &converted, depth + 1)
                }
                b"Indexed" => {
                    let base = array.get(1)?;
                    let maximum = integer(array.get(2)).unwrap_or(255).clamp(0, 255) as usize;
                    let lookup = object_bytes(array.get(3)?)?;
                    let count = pdf_color_component_count(document, Some(base));
                    let index = components
                        .first()
                        .copied()
                        .unwrap_or(0.0)
                        .round()
                        .clamp(0.0, maximum as f64) as usize;
                    let offset = index.checked_mul(count)?;
                    let sample = lookup.get(offset..offset.checked_add(count)?)?;
                    let sample = sample
                        .iter()
                        .map(|value| f64::from(*value) / 255.0)
                        .collect::<Vec<_>>();
                    components_to_rgb(document, Some(base), &sample, depth + 1)
                }
                b"ICCBased" => {
                    let profile = array
                        .get(1)
                        .and_then(|value| document.dereference(value).ok().map(|(_, value)| value))
                        .and_then(|value| value.as_stream().ok())?;
                    if let Ok(alternate) = profile.dict.get(b"Alternate") {
                        return components_to_rgb(document, Some(alternate), components, depth + 1);
                    }
                    match profile.dict.get(b"N").and_then(Object::as_i64).unwrap_or(3) {
                        1 => {
                            let gray = components.first().copied().unwrap_or(0.0);
                            Some([gray, gray, gray])
                        }
                        4 => Some(cmyk_components_to_rgb(components)),
                        _ => Some(default_rgb_components(components)),
                    }
                }
                b"CalGray" => {
                    let dictionary = array.get(1).and_then(|value| value.as_dict().ok());
                    let gamma = dictionary
                        .and_then(|value| value.get(b"Gamma").ok())
                        .and_then(|value| number(Some(value)))
                        .unwrap_or(1.0);
                    let gray = components
                        .first()
                        .copied()
                        .unwrap_or(0.0)
                        .clamp(0.0, 1.0)
                        .powf(gamma);
                    Some([gray, gray, gray])
                }
                b"CalRGB" => cal_rgb_to_srgb(array.get(1), components),
                b"Lab" => lab_to_srgb(array.get(1), components),
                b"Pattern" => array.get(1).and_then(|base| {
                    components_to_rgb(document, Some(base), components, depth + 1)
                }),
                _ => Some(default_rgb_components(components)),
            }
        }
        _ => Some(default_rgb_components(components)),
    }
}

fn default_rgb_components(components: &[f64]) -> [f64; 3] {
    let red = components.first().copied().unwrap_or(0.0);
    [
        red,
        components.get(1).copied().unwrap_or(red),
        components.get(2).copied().unwrap_or(red),
    ]
}

fn cmyk_components_to_rgb(components: &[f64]) -> [f64; 3] {
    let cyan = components.first().copied().unwrap_or(0.0);
    let magenta = components.get(1).copied().unwrap_or(0.0);
    let yellow = components.get(2).copied().unwrap_or(0.0);
    let black = components.get(3).copied().unwrap_or(0.0);
    [
        1.0 - (cyan + black).min(1.0),
        1.0 - (magenta + black).min(1.0),
        1.0 - (yellow + black).min(1.0),
    ]
}

fn rgb_to_bytes(rgb: [f64; 3]) -> [u8; 3] {
    rgb.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn cal_rgb_to_srgb(parameters: Option<&Object>, components: &[f64]) -> Option<[f64; 3]> {
    let dictionary = parameters.and_then(|value| value.as_dict().ok())?;
    let gamma = dictionary
        .get(b"Gamma")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, 3))
        .filter(|values| values.len() == 3)
        .unwrap_or_else(|| vec![1.0, 1.0, 1.0]);
    let matrix = dictionary
        .get(b"Matrix")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, 9))
        .filter(|values| values.len() == 9)
        .unwrap_or_else(|| vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let adjusted = [
        components
            .first()
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
            .powf(gamma[0]),
        components
            .get(1)
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
            .powf(gamma[1]),
        components
            .get(2)
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
            .powf(gamma[2]),
    ];
    xyz_to_srgb([
        matrix[0] * adjusted[0] + matrix[3] * adjusted[1] + matrix[6] * adjusted[2],
        matrix[1] * adjusted[0] + matrix[4] * adjusted[1] + matrix[7] * adjusted[2],
        matrix[2] * adjusted[0] + matrix[5] * adjusted[1] + matrix[8] * adjusted[2],
    ])
}

fn lab_to_srgb(parameters: Option<&Object>, components: &[f64]) -> Option<[f64; 3]> {
    let dictionary = parameters.and_then(|value| value.as_dict().ok())?;
    let white = dictionary
        .get(b"WhitePoint")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, 3))
        .filter(|values| values.len() == 3)
        .unwrap_or_else(|| vec![0.95047, 1.0, 1.08883]);
    let lightness = components.first().copied().unwrap_or(0.0).clamp(0.0, 100.0);
    let a = components.get(1).copied().unwrap_or(0.0);
    let b = components.get(2).copied().unwrap_or(0.0);
    let fy = (lightness + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inverse = |value: f64| {
        let cube = value * value * value;
        if cube > 216.0 / 24_389.0 {
            cube
        } else {
            (116.0 * value - 16.0) / 903.3
        }
    };
    xyz_to_srgb([
        white[0] * inverse(fx),
        white[1] * inverse(fy),
        white[2] * inverse(fz),
    ])
}

fn xyz_to_srgb(xyz: [f64; 3]) -> Option<[f64; 3]> {
    if xyz.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let linear = [
        3.2406 * xyz[0] - 1.5372 * xyz[1] - 0.4986 * xyz[2],
        -0.9689 * xyz[0] + 1.8758 * xyz[1] + 0.0415 * xyz[2],
        0.0557 * xyz[0] - 0.2040 * xyz[1] + 1.0570 * xyz[2],
    ];
    Some(linear.map(|value| {
        if value <= 0.003_130_8 {
            12.92 * value
        } else {
            1.055 * value.max(0.0).powf(1.0 / 2.4) - 0.055
        }
    }))
}

fn transformed_axial_axis(
    matrix: Matrix,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
) -> Option<(f64, f64, f64, f64)> {
    let vector_x = x1 - x0;
    let vector_y = y1 - y0;
    let source_length_squared = vector_x * vector_x + vector_y * vector_y;
    if source_length_squared <= 1e-18 {
        return None;
    }
    let [a, b, c, d, _, _] = matrix;
    let determinant = a * d - b * c;
    if determinant.abs() <= 1e-15 {
        return None;
    }
    let gradient_x = (d * vector_x - b * vector_y) / (determinant * source_length_squared);
    let gradient_y = (-c * vector_x + a * vector_y) / (determinant * source_length_squared);
    let gradient_length_squared = gradient_x * gradient_x + gradient_y * gradient_y;
    if gradient_length_squared <= 1e-24 {
        return None;
    }
    let transformed_vector_x = gradient_x / gradient_length_squared;
    let transformed_vector_y = gradient_y / gradient_length_squared;
    let start = transform_point(matrix, x0, y0);
    Some((
        start.0,
        start.1,
        start.0 + transformed_vector_x,
        start.1 + transformed_vector_y,
    ))
}

fn matrix_maximum_scale(matrix: Matrix) -> f64 {
    let [a, b, c, d, _, _] = matrix;
    let first_squared = a * a + b * b;
    let second_squared = c * c + d * d;
    let cross = a * c + b * d;
    let discriminant = (first_squared - second_squared).hypot(2.0 * cross);
    ((first_squared + second_squared + discriminant) / 2.0)
        .max(0.0)
        .sqrt()
}

fn rectangle_path(x: f64, y: f64, width: f64, height: f64) -> String {
    format!(
        "M {} {} H {} V {} H {} Z",
        fmt(x),
        fmt(y),
        fmt(x + width),
        fmt(y + height),
        fmt(x)
    )
}

fn quadrilateral_path(points: [(f64, f64); 4]) -> String {
    format!(
        "M {} {} L {} {} L {} {} L {} {} Z",
        fmt(points[0].0),
        fmt(points[0].1),
        fmt(points[1].0),
        fmt(points[1].1),
        fmt(points[2].0),
        fmt(points[2].1),
        fmt(points[3].0),
        fmt(points[3].1),
    )
}

fn inherited_page_array(
    document: &Document,
    mut page_id: ObjectId,
    key: &[u8],
) -> Option<[f64; 4]> {
    for _ in 0..64 {
        let page = document.get_dictionary(page_id).ok()?;
        if let Ok(array) = page.get(key).and_then(Object::as_array) {
            let values = array
                .iter()
                .filter_map(|object| number(Some(object)))
                .collect::<Vec<_>>();
            if values.len() >= 4 {
                return Some([values[0], values[1], values[2], values[3]]);
            }
        }
        page_id = page.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

fn inherited_page_number(document: &Document, mut page_id: ObjectId, key: &[u8]) -> Option<f64> {
    for _ in 0..64 {
        let page = document.get_dictionary(page_id).ok()?;
        if let Ok(value) = page.get(key)
            && let Some(number) = number(Some(value))
        {
            return Some(number);
        }
        page_id = page.get(b"Parent").and_then(Object::as_reference).ok()?;
    }
    None
}

fn page_user_unit(document: &Document, page_id: ObjectId) -> Result<f64> {
    let page = document.get_dictionary(page_id)?;
    let Ok(value) = page.get(b"UserUnit") else {
        return Ok(1.0);
    };
    let (_, value) = document.dereference(value)?;
    let Some(value) = number(Some(value)) else {
        return Err(Error::InvalidInput(
            "PDF page UserUnit is not numeric".into(),
        ));
    };
    if !value.is_finite() || value <= 0.0 || value > 75_000.0 {
        return Err(Error::InvalidInput(format!(
            "PDF page UserUnit {value} is outside the supported positive range"
        )));
    }
    Ok(value)
}

fn page_resource_dicts(
    document: &Document,
    mut page_id: ObjectId,
) -> impl Iterator<Item = &Dictionary> {
    let mut result = Vec::new();
    for _ in 0..64 {
        let Ok(page) = document.get_dictionary(page_id) else {
            break;
        };
        if let Ok(resources) = page
            .get_deref(b"Resources", document)
            .and_then(Object::as_dict)
        {
            result.push(resources);
        }
        let Ok(parent) = page.get(b"Parent").and_then(Object::as_reference) else {
            break;
        };
        page_id = parent;
    }
    result.into_iter()
}

fn page_matrix(bounds: [f64; 4], rotation: i32) -> Matrix {
    let [left, bottom, right, top] = bounds;
    match rotation {
        90 => [0.0, 1.0, 1.0, 0.0, -bottom, -left],
        180 => [-1.0, 0.0, 0.0, 1.0, right, -bottom],
        270 => [0.0, -1.0, -1.0, 0.0, top, right],
        _ => [1.0, 0.0, 0.0, -1.0, -left, top],
    }
}

fn matrix_operands(operands: &[Object]) -> Option<Matrix> {
    let values = numbers(operands, 6);
    (values.len() == 6).then(|| {
        [
            values[0], values[1], values[2], values[3], values[4], values[5],
        ]
    })
}

fn numbers(operands: &[Object], maximum: usize) -> Vec<f64> {
    operands
        .iter()
        .take(maximum)
        .filter_map(|object| number(Some(object)))
        .collect()
}

fn number(object: Option<&Object>) -> Option<f64> {
    match object? {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(f64::from(*value)),
        _ => None,
    }
}

fn integer(object: Option<&Object>) -> Option<i64> {
    match object? {
        Object::Integer(value) => Some(*value),
        Object::Real(value) => Some(value.round() as i64),
        _ => None,
    }
}

fn name(object: Option<&Object>) -> Option<&[u8]> {
    object?.as_name().ok()
}

fn string_bytes(object: Option<&Object>) -> Option<&[u8]> {
    object?.as_str().ok()
}

fn gray_paint(gray: f64, opacity: f64) -> Paint {
    let channel = (gray.clamp(0.0, 1.0) * 255.0).round() as u8;
    Paint::Solid {
        color: format!("#{channel:02X}{channel:02X}{channel:02X}"),
        opacity,
    }
}

fn rgb_paint(values: &[f64], opacity: f64) -> Paint {
    let value = |index: usize| {
        (values.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8
    };
    Paint::Solid {
        color: format!("#{:02X}{:02X}{:02X}", value(0), value(1), value(2)),
        opacity,
    }
}

fn cmyk_paint(values: &[f64], opacity: f64) -> Paint {
    let component = |index: usize| values.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0);
    let k = component(3);
    let red = ((1.0 - (component(0) + k).min(1.0)) * 255.0).round() as u8;
    let green = ((1.0 - (component(1) + k).min(1.0)) * 255.0).round() as u8;
    let blue = ((1.0 - (component(2) + k).min(1.0)) * 255.0).round() as u8;
    Paint::Solid {
        color: format!("#{red:02X}{green:02X}{blue:02X}"),
        opacity,
    }
}

fn set_paint_opacity(paint: &mut Paint, opacity: f64) {
    match paint {
        Paint::Solid {
            opacity: current, ..
        }
        | Paint::PatternRef {
            opacity: current, ..
        } => *current = opacity,
        Paint::LinearGradient(gradient) => {
            for stop in &mut gradient.stops {
                stop.opacity = opacity;
            }
        }
        Paint::RadialGradient(gradient) => {
            for stop in &mut gradient.stops {
                stop.opacity = opacity;
            }
        }
        Paint::None => {}
    }
}

fn pdf_blend_mode(name: &[u8]) -> String {
    match name {
        b"Normal" | b"Compatible" => "normal",
        b"Multiply" => "multiply",
        b"Screen" => "screen",
        b"Overlay" => "overlay",
        b"Darken" => "darken",
        b"Lighten" => "lighten",
        b"ColorDodge" => "color-dodge",
        b"ColorBurn" => "color-burn",
        b"HardLight" => "hard-light",
        b"SoftLight" => "soft-light",
        b"Difference" => "difference",
        b"Exclusion" => "exclusion",
        b"Hue" => "hue",
        b"Saturation" => "saturation",
        b"Color" => "color",
        b"Luminosity" => "luminosity",
        _ => "normal",
    }
    .into()
}

fn children_are_opaque_normal(nodes: &[Node]) -> bool {
    !nodes.is_empty() && nodes.iter().all(node_is_opaque_normal)
}

fn node_is_opaque_normal(node: &Node) -> bool {
    let meta = match node {
        Node::Path { meta, .. }
        | Node::Text { meta, .. }
        | Node::Image { meta, .. }
        | Node::Group { meta, .. } => meta,
    };
    if !meta.mask_id.is_empty() || (!meta.blend_mode.is_empty() && meta.blend_mode != "normal") {
        return false;
    }
    match node {
        Node::Path { fill, stroke, .. } => paint_is_opaque(fill) && paint_is_opaque(&stroke.paint),
        Node::Text { runs, opacity, .. } => {
            *opacity >= 1.0 - 1e-12 && runs.iter().all(|run| paint_is_opaque(&run.fill))
        }
        Node::Image { .. } => false,
        Node::Group { nodes, opacity, .. } => {
            *opacity >= 1.0 - 1e-12 && children_are_opaque_normal(nodes)
        }
    }
}

fn paint_is_opaque(paint: &Paint) -> bool {
    match paint {
        Paint::None => true,
        Paint::Solid { opacity, .. } => *opacity >= 1.0 - 1e-12,
        Paint::LinearGradient(gradient) => gradient
            .stops
            .iter()
            .all(|stop| stop.opacity >= 1.0 - 1e-12),
        Paint::RadialGradient(gradient) => gradient
            .stops
            .iter()
            .all(|stop| stop.opacity >= 1.0 - 1e-12),
        Paint::PatternRef { opacity, .. } => *opacity >= 1.0 - 1e-12,
    }
}

fn apply_path_knockout_masks(
    page: &mut Page,
    children: &mut [Node],
    mask_counter: &mut usize,
) -> bool {
    if children.len() < 2 || !children.iter().all(knockout_node_supported) {
        return false;
    }
    for index in 0..children.len() - 1 {
        let mut mask_nodes = vec![Node::Path {
            id: format!("pdf-knockout-backdrop-{}-{index}", page.number),
            d: rectangle_path(0.0, 0.0, page.width, page.height),
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#FFFFFF"),
            stroke: Stroke::default(),
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: "knockout-mask-backdrop".into(),
                ..SourceMeta::default()
            },
        }];
        mask_nodes.extend(
            children[index + 1..]
                .iter()
                .filter_map(blackened_knockout_node),
        );
        *mask_counter += 1;
        let mask_id = format!("pdf-knockout-mask-{}-{}", page.number, mask_counter);
        page.masks.push(crate::ir::MaskDefinition {
            id: mask_id.clone(),
            mask_type: "luminance".into(),
            nodes: mask_nodes,
            transfer_values: Vec::new(),
        });
        node_meta_mut(&mut children[index]).mask_id = mask_id;
    }
    true
}

fn knockout_node_supported(node: &Node) -> bool {
    let meta = match node {
        Node::Path { meta, .. } | Node::Group { meta, .. } => meta,
        _ => return false,
    };
    if !meta.mask_id.is_empty() || (!meta.blend_mode.is_empty() && meta.blend_mode != "normal") {
        return false;
    }
    match node {
        Node::Path { .. } => true,
        Node::Group { nodes, .. } => !nodes.is_empty() && nodes.iter().all(knockout_node_supported),
        _ => false,
    }
}

fn blackened_knockout_node(node: &Node) -> Option<Node> {
    match node {
        Node::Path {
            id,
            d,
            fill_rule,
            fill,
            stroke,
            transform,
            clip_id,
            meta,
        } => Some(Node::Path {
            id: format!("{id}-knockout-shape"),
            d: d.clone(),
            fill_rule: fill_rule.clone(),
            fill: black_shape_paint(fill, meta.alpha_is_shape),
            stroke: Stroke {
                paint: black_shape_paint(&stroke.paint, meta.alpha_is_shape),
                width: stroke.width,
                line_cap: stroke.line_cap,
                line_join: stroke.line_join,
                miter_limit: stroke.miter_limit,
                dash_array: stroke.dash_array.clone(),
                dash_offset: stroke.dash_offset,
            },
            transform: *transform,
            clip_id: clip_id.clone(),
            meta: SourceMeta {
                kind: "knockout-shape".into(),
                ..SourceMeta::default()
            },
        }),
        Node::Group {
            id,
            nodes,
            transform,
            opacity,
            clip_id,
            meta,
        } => Some(Node::Group {
            id: format!("{id}-knockout-shape"),
            nodes: nodes.iter().filter_map(blackened_knockout_node).collect(),
            transform: *transform,
            opacity: if meta.alpha_is_shape { *opacity } else { 1.0 },
            clip_id: clip_id.clone(),
            meta: SourceMeta {
                kind: "knockout-shape-group".into(),
                ..SourceMeta::default()
            },
        }),
        _ => None,
    }
}

fn black_shape_paint(paint: &Paint, preserve_alpha: bool) -> Paint {
    match paint {
        Paint::None => Paint::None,
        Paint::Solid { opacity, .. } => Paint::Solid {
            color: "#000000".into(),
            opacity: if preserve_alpha { *opacity } else { 1.0 },
        },
        Paint::LinearGradient(gradient) => {
            let mut gradient = (**gradient).clone();
            for stop in &mut gradient.stops {
                stop.color = "#000000".into();
                if !preserve_alpha {
                    stop.opacity = 1.0;
                }
            }
            Paint::LinearGradient(Box::new(gradient))
        }
        Paint::RadialGradient(gradient) => {
            let mut gradient = (**gradient).clone();
            for stop in &mut gradient.stops {
                stop.color = "#000000".into();
                if !preserve_alpha {
                    stop.opacity = 1.0;
                }
            }
            Paint::RadialGradient(Box::new(gradient))
        }
        Paint::PatternRef { opacity, .. } => Paint::Solid {
            color: "#000000".into(),
            opacity: if preserve_alpha { *opacity } else { 1.0 },
        },
    }
}

fn node_meta_mut(node: &mut Node) -> &mut SourceMeta {
    match node {
        Node::Path { meta, .. }
        | Node::Text { meta, .. }
        | Node::Image { meta, .. }
        | Node::Group { meta, .. } => meta,
    }
}

fn normalize_inline_image(mut stream: Stream) -> Stream {
    let keys: [(&[u8], &[u8]); 9] = [
        (b"W", b"Width"),
        (b"H", b"Height"),
        (b"BPC", b"BitsPerComponent"),
        (b"CS", b"ColorSpace"),
        (b"D", b"Decode"),
        (b"DP", b"DecodeParms"),
        (b"F", b"Filter"),
        (b"I", b"Interpolate"),
        (b"IM", b"ImageMask"),
    ];
    for (short, long) in keys {
        if !stream.dict.has(long)
            && let Ok(value) = stream.dict.get(short).cloned()
        {
            stream.dict.set(long, value);
        }
    }
    if let Ok(Object::Name(name)) = stream.dict.get_mut(b"ColorSpace") {
        *name = match name.as_slice() {
            b"G" => b"DeviceGray".to_vec(),
            b"RGB" => b"DeviceRGB".to_vec(),
            b"CMYK" => b"DeviceCMYK".to_vec(),
            _ => name.clone(),
        };
    }
    stream.dict.set("Subtype", Object::Name(b"Image".to_vec()));
    stream
}

fn object_bytes(object: &Object) -> Option<Vec<u8>> {
    match object {
        Object::String(bytes, _) => Some(bytes.clone()),
        Object::Stream(stream) => stream
            .decompressed_content()
            .ok()
            .or_else(|| Some(stream.content.clone())),
        _ => None,
    }
}

fn color_components(object: &Object) -> usize {
    match object {
        Object::Name(name) if name == b"DeviceGray" || name == b"G" => 1,
        Object::Name(name) if name == b"DeviceCMYK" => 4,
        Object::Name(_) => 3,
        Object::Array(array) => match array.first().and_then(|value| value.as_name().ok()) {
            Some(b"DeviceGray") => 1,
            Some(b"DeviceCMYK") => 4,
            _ => 3,
        },
        _ => 3,
    }
}

fn cmyk_samples_to_rgb(samples: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(samples.len() / 4 * 3);
    for sample in samples.chunks_exact(4) {
        let cyan = f64::from(sample[0]) / 255.0;
        let magenta = f64::from(sample[1]) / 255.0;
        let yellow = f64::from(sample[2]) / 255.0;
        let black = f64::from(sample[3]) / 255.0;
        rgb.extend_from_slice(&[
            ((1.0 - (cyan + black).min(1.0)) * 255.0).round() as u8,
            ((1.0 - (magenta + black).min(1.0)) * 255.0).round() as u8,
            ((1.0 - (yellow + black).min(1.0)) * 255.0).round() as u8,
        ]);
    }
    rgb
}

fn normalize_image_samples(
    stream: &Stream,
    samples: &[u8],
    width: usize,
    height: usize,
    bits_per_component: usize,
    components: usize,
    color_space: Option<&Object>,
) -> Option<Vec<u8>> {
    if components == 0 {
        return None;
    }
    let samples_per_row = width.checked_mul(components)?;
    let row_bits = samples_per_row.checked_mul(bits_per_component)?;
    let row_bytes = row_bits.div_ceil(8);
    if samples.len() < row_bytes.checked_mul(height)? {
        return None;
    }
    let decode = stream
        .dict
        .get(b"Decode")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, usize::MAX));
    let indexed_maximum = match color_space {
        Some(Object::Array(values))
            if values.first().and_then(|value| value.as_name().ok()) == Some(b"Indexed") =>
        {
            integer(values.get(2)).map(|value| value.max(0) as f64)
        }
        _ => None,
    };
    let is_lab = matches!(
        color_space,
        Some(Object::Array(values))
            if values.first().and_then(|value| value.as_name().ok()) == Some(b"Lab")
    );
    let largest = (1u64 << bits_per_component) - 1;
    let mut output = Vec::with_capacity(width.checked_mul(height)?.checked_mul(components)?);
    for row in 0..height {
        let row_bit_start = row.checked_mul(row_bytes)?.checked_mul(8)?;
        for sample_index in 0..samples_per_row {
            let raw = read_bits_at(
                samples,
                row_bit_start.checked_add(sample_index.checked_mul(bits_per_component)?)?,
                bits_per_component,
            )?;
            let component = sample_index % components;
            let default_end = indexed_maximum.unwrap_or(1.0);
            let minimum = decode
                .as_ref()
                .and_then(|values| values.get(component * 2))
                .copied()
                .unwrap_or(0.0);
            let maximum = decode
                .as_ref()
                .and_then(|values| values.get(component * 2 + 1))
                .copied()
                .unwrap_or(default_end);
            let decoded = minimum + raw as f64 / largest as f64 * (maximum - minimum);
            output.push(if is_lab {
                (raw as f64 / largest as f64 * 255.0).round() as u8
            } else if let Some(indexed_maximum) = indexed_maximum {
                decoded.clamp(0.0, indexed_maximum).round() as u8
            } else {
                (decoded.clamp(0.0, 1.0) * 255.0).round() as u8
            });
        }
    }
    Some(output)
}

fn decode_stencil_image(
    stream: &Stream,
    samples: &[u8],
    width: usize,
    height: usize,
    bits_per_component: usize,
    paint: &Paint,
) -> Option<Vec<u8>> {
    let row_bits = width.checked_mul(bits_per_component)?;
    let row_bytes = row_bits.div_ceil(8);
    if samples.len() < row_bytes.checked_mul(height)? {
        return None;
    }
    let (color, opacity) = match paint {
        Paint::Solid { color, opacity } => {
            (parse_pdf_hex_color(color).unwrap_or([0, 0, 0]), *opacity)
        }
        _ => ([0, 0, 0], 1.0),
    };
    let decode = stream
        .dict
        .get(b"Decode")
        .and_then(Object::as_array)
        .ok()
        .map(|values| numbers(values, 2))
        .filter(|values| values.len() == 2)
        .unwrap_or_else(|| vec![0.0, 1.0]);
    let largest = (1u64 << bits_per_component) - 1;
    let mut output = Vec::with_capacity(width.checked_mul(height)?.checked_mul(4)?);
    for row in 0..height {
        let row_bit_start = row.checked_mul(row_bytes)?.checked_mul(8)?;
        for column in 0..width {
            let raw = read_bits_at(
                samples,
                row_bit_start.checked_add(column.checked_mul(bits_per_component)?)?,
                bits_per_component,
            )?;
            let decoded = decode[0] + raw as f64 / largest as f64 * (decode[1] - decode[0]);
            let alpha = if decoded < 0.5 {
                (opacity.clamp(0.0, 1.0) * 255.0).round() as u8
            } else {
                0
            };
            output.extend_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    Some(output)
}

fn read_bits_at(data: &[u8], bit_start: usize, bits: usize) -> Option<u64> {
    if bits == 0 || bits > 16 || bit_start.checked_add(bits)? > data.len().checked_mul(8)? {
        return None;
    }
    let mut value = 0u64;
    for position in bit_start..bit_start + bits {
        value = (value << 1) | u64::from((data[position / 8] >> (7 - position % 8)) & 1);
    }
    Some(value)
}

fn parse_pdf_hex_color(color: &str) -> Option<[u8; 3]> {
    let color = color.trim_start_matches('#');
    if color.len() != 6 {
        return None;
    }
    Some([
        u8::from_str_radix(&color[0..2], 16).ok()?,
        u8::from_str_radix(&color[2..4], 16).ok()?,
        u8::from_str_radix(&color[4..6], 16).ok()?,
    ])
}

fn add_alpha(samples: &[u8], components: usize, alpha: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(alpha.len().saturating_mul(components + 1));
    for (sample, opacity) in samples.chunks_exact(components).zip(alpha.iter()) {
        output.extend_from_slice(sample);
        output.push(*opacity);
    }
    output
}

fn clean_font_name(name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name);
    let name = name
        .split_once('+')
        .map_or(name.as_ref(), |(_, value)| value);
    match name {
        "ArialMT" => "Arial".into(),
        "Arial-BoldMT" => "Arial".into(),
        "Arial-ItalicMT" => "Arial".into(),
        "Arial-BoldItalicMT" => "Arial".into(),
        _ => name.replace([',', '-'], " "),
    }
}

fn stable_pdf_symbol_fallback<'a>(family: &str, text: &'a str) -> Option<&'a str> {
    let normalized = family
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if !normalized.starts_with("wingdings") {
        return None;
    }
    match text {
        "\u{26ab}" => Some("\u{25cf}"),
        "\u{25c6}" | "\u{2713}" | "\u{27a2}" => Some(text),
        _ => None,
    }
}

fn should_outline_pdf_text(
    requires_outline: bool,
    has_embedded_font: bool,
    explicit_fidelity_mode: bool,
) -> bool {
    requires_outline || (has_embedded_font && explicit_fidelity_mode)
}

fn pdf_font_stack(family: &str) -> String {
    if family.contains(',') {
        return family.to_owned();
    }
    let normalized = family
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if normalized.contains("meiryo")
        || normalized.contains("msgothic")
        || normalized.contains("yugothic")
        || normalized.contains("gothic")
    {
        let hiragino = if normalized.contains("bold") {
            "Hiragino Sans W6"
        } else {
            "Hiragino Sans W3"
        };
        return format!(
            "'{family}', '{hiragino}', 'Hiragino Sans', 'Yu Gothic', YuGothic, sans-serif"
        );
    }
    if normalized.contains("mincho") {
        return format!("'{family}', 'Hiragino Mincho ProN', 'Yu Mincho', serif");
    }
    if normalized.contains("arial")
        || normalized.contains("calibri")
        || normalized.contains("trebuchet")
    {
        return format!("'{family}', Arial, Verdana, sans-serif");
    }
    family.to_owned()
}

fn is_browser_font_family(family: &str) -> bool {
    let family = family.to_ascii_lowercase();
    [
        "arial",
        "helvetica",
        "times",
        "courier",
        "calibri",
        "cambria",
        "verdana",
        "georgia",
        "tahoma",
        "trebuchet",
        "symbol",
        "zapfdingbats",
        "meiryo",
        "yu gothic",
        "yugothic",
        "hiragino",
        "noto sans",
        "noto serif",
    ]
    .iter()
    .any(|candidate| family.contains(candidate))
}

fn fmt(value: f64) -> String {
    format!("{value:.5}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn pattern_variant_id(source_id: &str, transform: Matrix) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in source_id.bytes().chain(
        transform
            .iter()
            .flat_map(|value| value.to_bits().to_be_bytes()),
    ) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("pdf-pattern-variant-{hash:016x}")
}

fn deduplicate(values: Vec<String>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        if !result.contains(&value) {
            result.push(value);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_known_wingdings_bullet_without_emoji_metrics() {
        assert_eq!(
            stable_pdf_symbol_fallback("Wingdings Regular", "\u{26ab}"),
            Some("\u{25cf}")
        );
        assert_eq!(stable_pdf_symbol_fallback("Arial", "\u{26ab}"), None);
        assert_eq!(stable_pdf_symbol_fallback("Wingdings", "A"), None);
        assert_eq!(
            stable_pdf_symbol_fallback("Wingdings", "\u{27a2}"),
            Some("\u{27a2}")
        );
    }

    #[test]
    fn embedded_font_outlining_requires_explicit_fidelity_mode() {
        assert!(should_outline_pdf_text(true, false, false));
        assert!(!should_outline_pdf_text(false, true, false));
        assert!(should_outline_pdf_text(false, true, true));
        assert!(!should_outline_pdf_text(false, false, true));
    }

    #[test]
    fn supplies_script_appropriate_pdf_font_fallbacks() {
        assert_eq!(
            pdf_font_stack("Meiryo Bold"),
            "'Meiryo Bold', 'Hiragino Sans W6', 'Hiragino Sans', 'Yu Gothic', YuGothic, sans-serif"
        );
        assert_eq!(
            pdf_font_stack("MeiryoUI"),
            "'MeiryoUI', 'Hiragino Sans W3', 'Hiragino Sans', 'Yu Gothic', YuGothic, sans-serif"
        );
        assert_eq!(
            pdf_font_stack("MS Mincho"),
            "'MS Mincho', 'Hiragino Mincho ProN', 'Yu Mincho', serif"
        );
        assert_eq!(pdf_font_stack("Custom Font"), "Custom Font");
    }

    #[test]
    fn user_unit_is_page_local_and_rejects_invalid_values() {
        let mut document = Document::with_version("1.7");
        let parent_id = document.add_object(dictionary! { "UserUnit" => 2 });
        let page_id = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(parent_id),
        });
        assert_eq!(page_user_unit(&document, page_id).unwrap(), 1.0);
        document
            .get_object_mut(page_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("UserUnit", 0);
        assert!(page_user_unit(&document, page_id).is_err());
    }

    #[test]
    fn expands_shared_mesh_decode_component_range() {
        let decode = [-10.0, 20.0, -5.0, 15.0, 0.0, 1.0];
        assert_eq!(
            complete_mesh_decode_ranges(&decode, 3),
            Some(vec![-10.0, 20.0, -5.0, 15.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0])
        );
    }

    #[test]
    fn rejects_mesh_decode_without_complete_component_pair() {
        assert!(complete_mesh_decode_ranges(&[0.0, 1.0, 0.0, 1.0], 3).is_none());
        assert!(complete_mesh_decode_ranges(&[0.0, 1.0, 0.0, 1.0, 0.0], 3).is_none());
    }
    use lopdf::dictionary;

    #[test]
    fn parses_simple_to_unicode_map() {
        let map = parse_to_unicode_cmap(
            b"1 beginbfchar\n<01> <0041>\nendbfchar\n1 beginbfrange\n<02> <03> <0042>\nendbfrange",
        );
        assert_eq!(map.get(&vec![1]).map(String::as_str), Some("A"));
        assert_eq!(map.get(&vec![2]).map(String::as_str), Some("B"));
        assert_eq!(map.get(&vec![3]).map(String::as_str), Some("C"));
    }

    #[test]
    fn expands_pdf_short_curve_operators_to_svg_cubics() {
        let mut path = PathBuilder::default();
        path.command("M", &[1.0, 2.0]);
        let [x2, y2, x3, y3] = [3.0, 4.0, 5.0, 6.0];
        let (current_x, current_y) = path.current_point.unwrap();
        path.command("C", &[current_x, current_y, x2, y2, x3, y3]);
        path.command("C", &[7.0, 8.0, 9.0, 10.0, 9.0, 10.0]);
        assert_eq!(path.data, "M 1 2 C 1 2 3 4 5 6 C 7 8 9 10 9 10");
    }

    #[test]
    fn executes_multi_input_pdf_calculator_function() {
        let result =
            evaluate_calculator_function(b"{ pop dup 1 exch sub 0.25 }", &[0.25, 0.75]).unwrap();
        assert_eq!(result, vec![0.25, 0.75, 0.25]);
    }

    #[test]
    fn executes_pdf_calculator_conditionals_and_stack_ops() {
        let result = evaluate_calculator_function(
            b"{ dup 0.5 gt { dup 2 mul } { dup 4 mul } ifelse }",
            &[0.75],
        )
        .unwrap();
        assert_eq!(result, vec![0.75, 1.5]);
    }

    #[test]
    fn interpolates_multi_output_sampled_pdf_function() {
        let mut document = Document::with_version("1.7");
        let function_id = document.add_object(Stream::new(
            lopdf::dictionary! {
                "FunctionType" => 0,
                "Domain" => vec![0.into(), 1.into()],
                "Range" => vec![0.into(), 1.into(), 0.into(), 1.into()],
                "Size" => vec![2.into()],
                "BitsPerSample" => 8,
            },
            vec![0, 255, 255, 0],
        ));
        let result =
            evaluate_pdf_function(&document, &Object::Reference(function_id), 0.25, 0).unwrap();
        assert!((result[0] - 0.25).abs() < 1e-12);
        assert!((result[1] - 0.75).abs() < 1e-12);
    }

    #[test]
    fn interpolates_multi_input_sampled_pdf_function() {
        let mut document = Document::with_version("1.7");
        let function_id = document.add_object(Stream::new(
            lopdf::dictionary! {
                "FunctionType" => 0,
                "Domain" => vec![0.into(), 1.into(), 0.into(), 1.into()],
                "Range" => vec![0.into(), 1.into()],
                "Size" => vec![2.into(), 2.into()],
                "BitsPerSample" => 8,
            },
            vec![0, 64, 128, 255],
        ));
        let result = evaluate_pdf_function_inputs(
            &document,
            &Object::Reference(function_id),
            &[0.5, 0.5],
            0,
        )
        .unwrap();
        assert!((result[0] - 111.75 / 255.0).abs() < 1e-12);
    }

    #[test]
    fn outlines_raw_cff_type2_glyph() {
        let decoder = FontDecoder {
            family: "Test CFF".into(),
            bold: false,
            italic: false,
            requires_outline: true,
            font_data: None,
            glyph_names: HashMap::from([(65, "A".into())]),
            unicode_map: HashMap::from([(vec![65], "A".into())]),
            code_lengths: vec![1],
            fallback_kind: FontFallback::OneByte,
            widths: HashMap::from([(65, 500.0)]),
            default_width: 500.0,
            type3: None,
        };
        let path = decoder
            .outline_cff(&minimal_test_cff(), b"A", 0.0, 0.0)
            .unwrap();
        assert!(path.contains("M 0 0"));
        assert!(path.matches('L').count() >= 3);
    }

    #[test]
    fn repairs_allowed_damaged_group3_2d_row() {
        let spec = InlineImageSpec {
            width: 8,
            height: 4,
            bits: 1,
            color_space: "/G".into(),
            filter: Some("/CCF".into()),
            k: 2,
            columns: 8,
            rows: 4,
            end_of_block: true,
            end_of_line: true,
            byte_aligned: false,
            black_is_one: false,
            damaged_rows_before_error: 1,
            interpolate: false,
        };
        let encoded = [
            0x00, 0x1d, 0xb0, 0x01, 0x60, 0x02, 0x00, 0x00, 0x00, 0xe6, 0x00, 0x20, 0x02, 0x00,
            0x20, 0x02, 0x00, 0x20, 0x02,
        ];
        let decoded = spec.decode_ccitt(&encoded).unwrap();
        assert_eq!(&decoded[0..8], &[255, 255, 255, 255, 0, 0, 0, 0]);
        assert_eq!(&decoded[8..16], &[255, 255, 255, 255, 0, 0, 0, 0]);
        assert!(decoded[16..32].iter().all(|pixel| *pixel == 255));
    }

    #[test]
    fn decodes_external_ccitt_stream_parameters() {
        let encoded = vec![
            0x00, 0x1d, 0xb0, 0x01, 0x60, 0x02, 0x00, 0x00, 0x00, 0xe6, 0x00, 0x20, 0x02, 0x00,
            0x20, 0x02, 0x00, 0x20, 0x02,
        ];
        let stream = Stream::new(
            lopdf::dictionary! {
                "Width" => 8,
                "Height" => 4,
                "Filter" => "CCITTFaxDecode",
                "DecodeParms" => lopdf::dictionary! {
                    "K" => 2,
                    "Columns" => 8,
                    "Rows" => 4,
                    "EndOfBlock" => true,
                    "EndOfLine" => true,
                    "DamagedRowsBeforeError" => 1,
                },
            },
            encoded,
        );
        let spec = ccitt_spec_from_stream(&stream, 8, 4);
        let decoded = spec.decode_ccitt(&stream.content).unwrap();
        assert_eq!(&decoded[0..8], &[255, 255, 255, 255, 0, 0, 0, 0]);
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn unpacks_subbyte_image_rows_and_stencil_alpha() {
        let grayscale = Stream::new(
            lopdf::dictionary! { "Decode" => vec![0.into(), 1.into()] },
            Vec::new(),
        );
        let samples = normalize_image_samples(
            &grayscale,
            &[0x08, 0xF0, 0xF8, 0x00],
            3,
            2,
            4,
            1,
            Some(&Object::Name(b"DeviceGray".to_vec())),
        )
        .unwrap();
        assert_eq!(samples, vec![0, 136, 255, 255, 136, 0]);

        let stencil = Stream::new(
            lopdf::dictionary! { "Decode" => vec![0.into(), 1.into()] },
            Vec::new(),
        );
        let rgba = decode_stencil_image(
            &stencil,
            &[0b1010_1010],
            8,
            1,
            1,
            &Paint::Solid {
                color: "#00FF00".into(),
                opacity: 0.5,
            },
        )
        .unwrap();
        assert_eq!(&rgba[0..4], &[0, 255, 0, 0]);
        assert_eq!(&rgba[4..8], &[0, 255, 0, 128]);
    }

    fn minimal_test_cff() -> Vec<u8> {
        vec![
            1, 0, 4, 4, // Header
            0, 1, 1, 1, 5, b'T', b'e', b's', b't', // Name INDEX
            0, 1, 1, 1, 7, 188, 16, 185, 15, 167, 17, // Top DICT INDEX
            0, 0, // String INDEX
            0, 0, // Global Subr INDEX
            0, 2, 1, 1, 2, 13, // CharStrings INDEX
            14, // .notdef
            139, 139, 21, // 0 0 rmoveto
            239, 139, 89, 239, 89, 39, 5,  // triangle rlineto
            14, // endchar
            0, 0, 34, // custom charset: GID 1 = SID 34 (A)
            0, 1, 65, // custom encoding: code 65 = GID 1
        ]
    }
}
