//! Bounded PNG/JPEG/BMP/TIFF/WebP/Netpbm grayscale raster vectorization.

use std::fs::File;
use std::io::Read;
use std::num::NonZeroU64;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke};

const MAX_RASTER_PIXELS: usize = 20_000_000;
const MAX_RASTER_DIMENSION: usize = 100_000;
const MAX_VECTOR_SPANS: usize = 500_000;

pub(crate) fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
}

pub(crate) fn is_gif(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

pub(crate) fn is_pnm(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(0..2),
        Some(b"P1" | b"P2" | b"P3" | b"P4" | b"P5" | b"P6" | b"P7")
    )
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut bytes = Vec::new();
    Read::take(File::open(path)?, options.max_input_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > options.max_input_bytes {
        return Err(Error::LimitExceeded(format!(
            "raster input exceeds maximum bytes ({})",
            options.max_input_bytes
        )));
    }

    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    let mut warnings = Vec::<String>::new();
    let (width, height, grayscale_pixels) = if ext == "png"
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
    {
        decode_png(&bytes)?
    } else if matches!(ext.as_str(), "jpg" | "jpeg" | "jpe" | "jfif")
        || bytes.starts_with(b"\xff\xd8\xff")
    {
        let (width, height, pixels, jpeg_warnings) = decode_jpeg(&bytes)?;
        warnings.extend(jpeg_warnings);
        (width, height, pixels)
    } else if matches!(ext.as_str(), "bmp" | "dib") || bytes.starts_with(b"BM") {
        decode_bmp(&bytes)?
    } else if ext == "gif" || is_gif(&bytes) {
        let (width, height, pixels, animated, partial_first_frame) = decode_gif(&bytes)?;
        if animated {
            warnings.push("animated GIF was reduced to its first frame for vectorization".into());
        }
        if partial_first_frame {
            warnings
                .push("partial first GIF frame was composited over a transparent canvas".into());
        }
        (width, height, pixels)
    } else if matches!(ext.as_str(), "tif" | "tiff")
        || bytes.starts_with(b"II*\x00")
        || bytes.starts_with(b"MM\x00*")
    {
        decode_tiff(&bytes)?
    } else if ext == "webp" || is_webp(&bytes) {
        let (width, height, pixels, animated) = decode_webp(&bytes)?;
        if animated {
            warnings.push("animated WebP was reduced to its first frame for vectorization".into());
        }
        (width, height, pixels)
    } else if matches!(ext.as_str(), "pbm" | "pgm" | "ppm" | "pnm" | "pam") || is_pnm(&bytes) {
        let (width, height, pixels, pnm_warnings) = decode_pnm(&bytes)?;
        warnings.extend(pnm_warnings);
        (width, height, pixels)
    } else {
        return Err(Error::Unsupported(format!(
            "unsupported raster format: {ext}"
        )));
    };

    let mut page = vectorize_grayscale(width, height, &grayscale_pixels, 128)?;
    for warning in &warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(warnings)
}

fn decode_pnm(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>, Vec<String>)> {
    let mut cursor = PnmCursor { bytes, position: 0 };
    let magic = cursor.token("magic")?;
    if magic == b"P7" {
        return decode_pam(bytes);
    }
    if magic.len() != 2 || !matches!(magic[0], b'P') || !matches!(magic[1], b'1'..=b'6') {
        return Err(Error::InvalidInput(
            "unsupported Netpbm magic number".into(),
        ));
    }
    let format = magic[1];
    let width = cursor.parse_usize("width")?;
    let height = cursor.parse_usize("height")?;
    let pixel_count = validate_dimensions(width, height)?;
    let maxval = if matches!(format, b'1' | b'4') {
        1u32
    } else {
        let value = cursor.parse_u32("maxval")?;
        if value == 0 || value > 65_535 {
            return Err(Error::InvalidInput(
                "Netpbm maxval must be between 1 and 65535".into(),
            ));
        }
        value
    };
    let mut pixels = Vec::with_capacity(pixel_count);
    match format {
        b'1' => {
            for _ in 0..pixel_count {
                let value = cursor.parse_u32("PBM sample")?;
                if value > 1 {
                    return Err(Error::InvalidInput("PBM sample must be 0 or 1".into()));
                }
                pixels.push(if value == 1 { 0 } else { 255 });
            }
        }
        b'2' => {
            for _ in 0..pixel_count {
                pixels.push(scale_pnm(cursor.parse_u32("PGM sample")?, maxval)?);
            }
        }
        b'3' => {
            for _ in 0..pixel_count {
                let red = scale_pnm(cursor.parse_u32("PPM red sample")?, maxval)? as u32;
                let green = scale_pnm(cursor.parse_u32("PPM green sample")?, maxval)? as u32;
                let blue = scale_pnm(cursor.parse_u32("PPM blue sample")?, maxval)? as u32;
                pixels.push(((red * 299 + green * 587 + blue * 114) / 1000) as u8);
            }
        }
        b'4' => {
            cursor.consume_binary_separator()?;
            let row_bytes = width.div_ceil(8);
            let payload_len = row_bytes
                .checked_mul(height)
                .ok_or_else(|| Error::LimitExceeded("PBM payload size overflowed".into()))?;
            let payload = cursor.take_bytes(payload_len, "PBM payload")?;
            for y in 0..height {
                for x in 0..width {
                    let bit = (payload[y * row_bytes + x / 8] >> (7 - (x % 8))) & 1;
                    pixels.push(if bit == 1 { 0 } else { 255 });
                }
            }
        }
        b'5' => {
            cursor.consume_binary_separator()?;
            let bytes_per_sample = if maxval < 256 { 1 } else { 2 };
            let payload_len = pixel_count
                .checked_mul(bytes_per_sample)
                .ok_or_else(|| Error::LimitExceeded("PGM payload size overflowed".into()))?;
            let payload = cursor.take_bytes(payload_len, "PGM payload")?;
            for chunk in payload.chunks_exact(bytes_per_sample) {
                let sample = if bytes_per_sample == 1 {
                    u32::from(chunk[0])
                } else {
                    u32::from(u16::from_be_bytes([chunk[0], chunk[1]]))
                };
                pixels.push(scale_pnm(sample, maxval)?);
            }
        }
        b'6' => {
            cursor.consume_binary_separator()?;
            let bytes_per_sample = if maxval < 256 { 1 } else { 2 };
            let payload_len = pixel_count
                .checked_mul(3)
                .and_then(|value| value.checked_mul(bytes_per_sample))
                .ok_or_else(|| Error::LimitExceeded("PPM payload size overflowed".into()))?;
            let payload = cursor.take_bytes(payload_len, "PPM payload")?;
            let mut offset = 0usize;
            for _ in 0..pixel_count {
                let read = |payload: &[u8], offset: &mut usize| -> u32 {
                    if bytes_per_sample == 1 {
                        let value = u32::from(payload[*offset]);
                        *offset += 1;
                        value
                    } else {
                        let value =
                            u32::from(u16::from_be_bytes([payload[*offset], payload[*offset + 1]]));
                        *offset += 2;
                        value
                    }
                };
                let red = u32::from(scale_pnm(read(payload, &mut offset), maxval)?);
                let green = u32::from(scale_pnm(read(payload, &mut offset), maxval)?);
                let blue = u32::from(scale_pnm(read(payload, &mut offset), maxval)?);
                pixels.push(((red * 299 + green * 587 + blue * 114) / 1000) as u8);
            }
        }
        _ => unreachable!(),
    }
    Ok((width, height, pixels, Vec::new()))
}

fn decode_pam(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>, Vec<String>)> {
    let mut cursor = PnmCursor { bytes, position: 2 };
    cursor.consume_binary_separator()?;
    let mut width = None;
    let mut height = None;
    let mut depth = None;
    let mut maxval = None;
    loop {
        let line = cursor.line("PAM header")?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.eq_ignore_ascii_case("ENDHDR") {
            break;
        }
        let mut fields = line.split_whitespace();
        let key = fields.next().unwrap_or_default();
        let value = fields
            .next()
            .ok_or_else(|| Error::InvalidInput(format!("PAM {key} header value is missing")))?;
        if fields.next().is_some() {
            return Err(Error::InvalidInput(format!(
                "PAM {key} header contains extra fields"
            )));
        }
        match key.to_ascii_uppercase().as_str() {
            "WIDTH" => {
                width = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| Error::InvalidInput("PAM WIDTH is invalid".into()))?,
                )
            }
            "HEIGHT" => {
                height = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| Error::InvalidInput("PAM HEIGHT is invalid".into()))?,
                )
            }
            "DEPTH" => {
                depth = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| Error::InvalidInput("PAM DEPTH is invalid".into()))?,
                )
            }
            "MAXVAL" => {
                maxval = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| Error::InvalidInput("PAM MAXVAL is invalid".into()))?,
                )
            }
            "TUPLTYPE" => {}
            _ => {
                return Err(Error::Unsupported(format!(
                    "PAM header keyword {key:?} is unsupported"
                )));
            }
        }
    }
    let width = width.ok_or_else(|| Error::InvalidInput("PAM WIDTH is missing".into()))?;
    let height = height.ok_or_else(|| Error::InvalidInput("PAM HEIGHT is missing".into()))?;
    let depth = depth.ok_or_else(|| Error::InvalidInput("PAM DEPTH is missing".into()))?;
    let maxval = maxval.ok_or_else(|| Error::InvalidInput("PAM MAXVAL is missing".into()))?;
    if maxval == 0 || maxval > 65_535 || !(1..=4).contains(&depth) {
        return Err(Error::Unsupported("PAM depth/maxval is unsupported".into()));
    }
    let pixel_count = validate_dimensions(width, height)?;
    let bytes_per_sample = if maxval < 256 { 1 } else { 2 };
    let payload_len = pixel_count
        .checked_mul(depth)
        .and_then(|value| value.checked_mul(bytes_per_sample))
        .ok_or_else(|| Error::LimitExceeded("PAM payload size overflowed".into()))?;
    let payload = cursor.take_bytes(payload_len, "PAM payload")?;
    let mut pixels = Vec::with_capacity(pixel_count);
    let mut offset = 0usize;
    let read = |payload: &[u8], offset: &mut usize| -> u32 {
        if bytes_per_sample == 1 {
            let value = u32::from(payload[*offset]);
            *offset += 1;
            value
        } else {
            let value = u32::from(u16::from_be_bytes([payload[*offset], payload[*offset + 1]]));
            *offset += 2;
            value
        }
    };
    for _ in 0..pixel_count {
        let first = scale_pnm(read(payload, &mut offset), maxval)?;
        match depth {
            1 => pixels.push(first),
            2 => {
                let alpha = scale_pnm(read(payload, &mut offset), maxval)?;
                pixels.push(if alpha < 128 { 255 } else { first });
            }
            3 => {
                let green = scale_pnm(read(payload, &mut offset), maxval)?;
                let blue = scale_pnm(read(payload, &mut offset), maxval)?;
                pixels.push(
                    ((u32::from(first) * 299 + u32::from(green) * 587 + u32::from(blue) * 114)
                        / 1000) as u8,
                );
            }
            4 => {
                let green = scale_pnm(read(payload, &mut offset), maxval)?;
                let blue = scale_pnm(read(payload, &mut offset), maxval)?;
                let alpha = scale_pnm(read(payload, &mut offset), maxval)?;
                let luminance =
                    ((u32::from(first) * 299 + u32::from(green) * 587 + u32::from(blue) * 114)
                        / 1000) as u8;
                pixels.push(if alpha < 128 { 255 } else { luminance });
            }
            _ => unreachable!(),
        }
    }
    Ok((width, height, pixels, Vec::new()))
}

fn scale_pnm(value: u32, maxval: u32) -> Result<u8> {
    if value > maxval {
        return Err(Error::InvalidInput("Netpbm sample exceeds maxval".into()));
    }
    Ok(((value * 255 + maxval / 2) / maxval) as u8)
}

struct PnmCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> PnmCursor<'a> {
    fn token(&mut self, context: &str) -> Result<Vec<u8>> {
        self.skip_header_space_and_comments();
        let start = self.position;
        while self.position < self.bytes.len()
            && !self.bytes[self.position].is_ascii_whitespace()
            && self.bytes[self.position] != b'#'
        {
            self.position += 1;
        }
        if start == self.position {
            return Err(Error::InvalidInput(format!("Netpbm {context} is missing")));
        }
        Ok(self.bytes[start..self.position].to_vec())
    }

    fn parse_usize(&mut self, context: &str) -> Result<usize> {
        let token = self.token(context)?;
        token
            .iter()
            .copied()
            .map(char::from)
            .collect::<String>()
            .parse::<usize>()
            .map_err(|_| Error::InvalidInput(format!("Netpbm {context} is invalid")))
    }

    fn parse_u32(&mut self, context: &str) -> Result<u32> {
        let token = self.token(context)?;
        token
            .iter()
            .copied()
            .map(char::from)
            .collect::<String>()
            .parse::<u32>()
            .map_err(|_| Error::InvalidInput(format!("Netpbm {context} is invalid")))
    }

    fn skip_header_space_and_comments(&mut self) {
        loop {
            while self.position < self.bytes.len()
                && self.bytes[self.position].is_ascii_whitespace()
            {
                self.position += 1;
            }
            if self.bytes.get(self.position) != Some(&b'#') {
                break;
            }
            while self.position < self.bytes.len() && self.bytes[self.position] != b'\n' {
                self.position += 1;
            }
        }
    }

    fn consume_binary_separator(&mut self) -> Result<()> {
        match self.bytes.get(self.position) {
            Some(b'\r') if self.bytes.get(self.position + 1) == Some(&b'\n') => self.position += 2,
            Some(byte) if byte.is_ascii_whitespace() => self.position += 1,
            _ => {
                return Err(Error::InvalidInput(
                    "Netpbm binary payload separator is missing".into(),
                ));
            }
        }
        Ok(())
    }

    fn take_bytes(&mut self, count: usize, context: &str) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| Error::LimitExceeded(format!("Netpbm {context} size overflowed")))?;
        let payload = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| Error::InvalidInput(format!("Netpbm {context} is truncated")))?;
        self.position = end;
        Ok(payload)
    }

    fn line(&mut self, context: &str) -> Result<&'a str> {
        let start = self.position;
        let end = self.bytes[start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| start + offset)
            .unwrap_or(self.bytes.len());
        self.position = if end < self.bytes.len() { end + 1 } else { end };
        std::str::from_utf8(self.bytes.get(start..end).unwrap_or_default())
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .map_err(|error| Error::InvalidInput(format!("Netpbm {context} is not UTF-8: {error}")))
    }
}

fn decode_png(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| Error::InvalidInput(format!("PNG error: {e}")))?;
    let (width, height) = reader.info().size();
    let pixel_count = validate_dimensions(width as usize, height as usize)?;
    let max_output_bytes = pixel_count
        .checked_mul(4)
        .ok_or_else(|| Error::LimitExceeded("PNG output size overflowed".into()))?;
    let buf_size = reader
        .output_buffer_size()
        .ok_or_else(|| Error::LimitExceeded("PNG output buffer size overflowed".into()))?;
    if buf_size > max_output_bytes {
        return Err(Error::LimitExceeded(
            "PNG output buffer exceeds the supported pixel limit".into(),
        ));
    }
    let mut buf = vec![0; buf_size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| Error::InvalidInput(format!("PNG frame error: {e}")))?;

    let width = info.width as usize;
    let height = info.height as usize;
    let mut gray = Vec::with_capacity(pixel_count);

    match info.color_type {
        png::ColorType::Rgb => {
            let limit = info.buffer_size().min(buf.len());
            for chunk in buf[..limit].chunks_exact(3) {
                let lum = ((chunk[0] as u32 * 299 + chunk[1] as u32 * 587 + chunk[2] as u32 * 114)
                    / 1000) as u8;
                gray.push(lum);
            }
            gray.resize(width * height, 255);
        }
        png::ColorType::Rgba => {
            let limit = info.buffer_size().min(buf.len());
            for chunk in buf[..limit].chunks_exact(4) {
                let alpha = chunk[3];
                if alpha < 128 {
                    gray.push(255); // Treat transparent as white
                } else {
                    let lum =
                        ((chunk[0] as u32 * 299 + chunk[1] as u32 * 587 + chunk[2] as u32 * 114)
                            / 1000) as u8;
                    gray.push(lum);
                }
            }
            gray.resize(width * height, 255);
        }
        png::ColorType::Grayscale => {
            let needed = (width * height).min(buf.len());
            gray.extend_from_slice(&buf[..needed]);
            gray.resize(width * height, 255);
        }
        png::ColorType::GrayscaleAlpha => {
            let limit = info.buffer_size().min(buf.len());
            for chunk in buf[..limit].chunks_exact(2) {
                let alpha = chunk[1];
                if alpha < 128 {
                    gray.push(255);
                } else {
                    gray.push(chunk[0]);
                }
            }
            gray.resize(width * height, 255);
        }
        _ => {
            // Fallback: fill with 255
            gray.resize(width * height, 255);
        }
    }

    Ok((width, height, gray))
}

fn decode_jpeg(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>, Vec<String>)> {
    let mut decoder = jpeg_decoder::Decoder::new(bytes);
    decoder
        .read_info()
        .map_err(|e| Error::InvalidInput(format!("JPEG metadata error: {e}")))?;
    let metadata = decoder
        .info()
        .ok_or_else(|| Error::InvalidInput("JPEG metadata missing".into()))?;
    let width = metadata.width as usize;
    let height = metadata.height as usize;
    let pixel_count = validate_dimensions(width, height)?;
    let pixel_format = metadata.pixel_format;
    let pixels = decoder
        .decode()
        .map_err(|e| Error::InvalidInput(format!("JPEG error: {e}")))?;
    let mut gray = Vec::with_capacity(pixel_count);
    let mut warnings = Vec::new();

    match pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            for chunk in pixels.chunks_exact(3) {
                let lum = ((chunk[0] as u32 * 299 + chunk[1] as u32 * 587 + chunk[2] as u32 * 114)
                    / 1000) as u8;
                gray.push(lum);
            }
        }
        jpeg_decoder::PixelFormat::L8 => {
            gray = pixels;
        }
        jpeg_decoder::PixelFormat::L16 => {
            if pixels.len() != pixel_count.saturating_mul(2) {
                return Err(Error::InvalidInput(
                    "16-bit JPEG output buffer size does not match its dimensions".into(),
                ));
            }
            for sample in pixels.chunks_exact(2) {
                let value = u16::from_ne_bytes([sample[0], sample[1]]);
                gray.push((value >> 8) as u8);
            }
            warnings.push("16-bit lossless JPEG samples were reduced to 8-bit grayscale".into());
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            if pixels.len() != pixel_count.saturating_mul(4) {
                return Err(Error::InvalidInput(
                    "CMYK JPEG output buffer size does not match its dimensions".into(),
                ));
            }
            for sample in pixels.chunks_exact(4) {
                let cyan = sample[0] as u32;
                let magenta = sample[1] as u32;
                let yellow = sample[2] as u32;
                let black = sample[3] as u32;
                let red = (255 - cyan) * (255 - black) / 255;
                let green = (255 - magenta) * (255 - black) / 255;
                let blue = (255 - yellow) * (255 - black) / 255;
                gray.push(((red * 299 + green * 587 + blue * 114) / 1000) as u8);
            }
            warnings.push(
                "CMYK JPEG pixels were approximated in RGB; embedded color profiles are not applied".into(),
            );
        }
    }

    Ok((width, height, gray, warnings))
}

fn decode_webp(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>, bool)> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("WebP decoder error: {error}")))?;
    let (width, height) = decoder.dimensions();
    let width = width as usize;
    let height = height as usize;
    let pixel_count = validate_dimensions(width, height)?;
    decoder.set_memory_limit(MAX_RASTER_PIXELS.saturating_mul(4));
    let animated = decoder.is_animated();
    let bytes_per_pixel = if decoder.has_alpha() { 4 } else { 3 };
    let expected_size = pixel_count
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| Error::LimitExceeded("WebP output size overflowed".into()))?;
    let output_size = decoder
        .output_buffer_size()
        .ok_or_else(|| Error::LimitExceeded("WebP output buffer size overflowed".into()))?;
    if output_size != expected_size {
        return Err(Error::InvalidInput(
            "WebP output buffer size does not match its declared dimensions".into(),
        ));
    }
    let mut decoded = vec![0u8; output_size];
    decoder
        .read_image(&mut decoded)
        .map_err(|error| Error::InvalidInput(format!("WebP decode error: {error}")))?;
    let mut gray = Vec::with_capacity(pixel_count);
    if bytes_per_pixel == 4 {
        for chunk in decoded.chunks_exact(4) {
            if chunk[3] < 128 {
                gray.push(255);
            } else {
                gray.push(luminance(chunk[0], chunk[1], chunk[2]));
            }
        }
    } else {
        for chunk in decoded.chunks_exact(3) {
            gray.push(luminance(chunk[0], chunk[1], chunk[2]));
        }
    }
    Ok((width, height, gray, animated))
}

fn decode_gif(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>, bool, bool)> {
    let memory_limit = (MAX_RASTER_PIXELS * 4) as u64;
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    options.set_memory_limit(gif::MemoryLimit::Bytes(
        NonZeroU64::new(memory_limit).expect("raster memory limit is nonzero"),
    ));
    options.check_frame_consistency(true);
    let mut decoder = options
        .read_info(std::io::Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("GIF decoder error: {error}")))?;
    let width = decoder.width() as usize;
    let height = decoder.height() as usize;
    let pixel_count = validate_dimensions(width, height)?;
    let mut rgba = vec![0u8; pixel_count * 4];
    let (frame_left, frame_top, frame_width, frame_height) = {
        let frame = decoder
            .read_next_frame()
            .map_err(|error| Error::InvalidInput(format!("GIF frame error: {error}")))?
            .ok_or_else(|| Error::InvalidInput("GIF contains no image frames".into()))?;
        let frame_left = frame.left as usize;
        let frame_top = frame.top as usize;
        let frame_width = frame.width as usize;
        let frame_height = frame.height as usize;
        let frame_pixels = validate_dimensions(frame_width, frame_height)?;
        let expected_len = frame_pixels
            .checked_mul(4)
            .ok_or_else(|| Error::LimitExceeded("GIF frame size overflowed".into()))?;
        if frame.buffer.len() != expected_len {
            return Err(Error::InvalidInput(
                "GIF RGBA frame buffer does not match its dimensions".into(),
            ));
        }
        let right = frame_left
            .checked_add(frame_width)
            .ok_or_else(|| Error::InvalidInput("GIF frame horizontal offset overflowed".into()))?;
        let bottom = frame_top
            .checked_add(frame_height)
            .ok_or_else(|| Error::InvalidInput("GIF frame vertical offset overflowed".into()))?;
        if right > width || bottom > height {
            return Err(Error::InvalidInput(
                "GIF frame extends beyond its logical screen".into(),
            ));
        }
        for row in 0..frame_height {
            let source_start = row * frame_width * 4;
            let destination_start = ((frame_top + row) * width + frame_left) * 4;
            let bytes = frame_width * 4;
            rgba[destination_start..destination_start + bytes]
                .copy_from_slice(&frame.buffer[source_start..source_start + bytes]);
        }
        (frame_left, frame_top, frame_width, frame_height)
    };
    let animated = decoder
        .next_frame_info()
        .map_err(|error| Error::InvalidInput(format!("GIF next-frame metadata error: {error}")))?
        .is_some();
    let partial_first_frame =
        frame_left != 0 || frame_top != 0 || frame_width != width || frame_height != height;
    let mut gray = Vec::with_capacity(pixel_count);
    for pixel in rgba.chunks_exact(4) {
        if pixel[3] < 128 {
            gray.push(255);
        } else {
            gray.push(luminance(pixel[0], pixel[1], pixel[2]));
        }
    }
    Ok((width, height, gray, animated, partial_first_frame))
}

fn luminance(red: u8, green: u8, blue: u8) -> u8 {
    ((red as u32 * 299 + green as u32 * 587 + blue as u32 * 114) / 1000) as u8
}

fn validate_dimensions(width: usize, height: usize) -> Result<usize> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidInput(
            "raster image dimensions must be greater than zero".into(),
        ));
    }
    if width > MAX_RASTER_DIMENSION || height > MAX_RASTER_DIMENSION {
        return Err(Error::LimitExceeded(format!(
            "raster image dimensions exceed {MAX_RASTER_DIMENSION} pixels per side"
        )));
    }
    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| Error::LimitExceeded("raster pixel count overflowed".into()))?;
    if pixels > MAX_RASTER_PIXELS {
        return Err(Error::LimitExceeded(format!(
            "raster image has {pixels} pixels; maximum is {MAX_RASTER_PIXELS}"
        )));
    }
    Ok(pixels)
}

#[derive(Clone, Copy, Debug)]
struct ActiveSpan {
    x: usize,
    width: usize,
    y: usize,
    height: usize,
}

/// Converts a 2D grayscale bitmap into clean 2D merged vector rectangles.
pub fn vectorize_grayscale(
    width: usize,
    height: usize,
    pixels: &[u8],
    threshold: u8,
) -> Result<Page> {
    let pixel_count = validate_dimensions(width, height)?;
    if pixels.len() < pixel_count {
        return Err(Error::InvalidInput(format!(
            "raster pixel buffer has {} bytes; expected at least {pixel_count}",
            pixels.len()
        )));
    }
    let mut path_d = String::new();
    let mut active_spans: Vec<ActiveSpan> = Vec::new();
    let mut row_spans: Vec<(usize, usize)> = Vec::new();
    let mut next_active: Vec<ActiveSpan> = Vec::new();
    let mut matched_row_indices: Vec<bool> = Vec::new();
    let mut path_count = 0usize;

    // 2D greedy vertical span merging
    for y in 0..height {
        row_spans.clear();
        let mut in_run = false;
        let mut run_start = 0;

        for x in 0..width {
            let is_dark = pixels.get(y * width + x).copied().unwrap_or(255) < threshold;
            if is_dark && !in_run {
                in_run = true;
                run_start = x;
            } else if !is_dark && in_run {
                in_run = false;
                if row_spans.len() >= MAX_VECTOR_SPANS {
                    return Err(Error::LimitExceeded(format!(
                        "raster row exceeds {MAX_VECTOR_SPANS} vector spans"
                    )));
                }
                row_spans.push((run_start, x - run_start));
            }
        }
        if in_run {
            if row_spans.len() >= MAX_VECTOR_SPANS {
                return Err(Error::LimitExceeded(format!(
                    "raster row exceeds {MAX_VECTOR_SPANS} vector spans"
                )));
            }
            row_spans.push((run_start, width - run_start));
        }

        next_active.clear();
        matched_row_indices.clear();
        matched_row_indices.resize(row_spans.len(), false);

        for mut span in active_spans.drain(..) {
            if let Ok(idx) = row_spans.binary_search(&(span.x, span.width))
                && !matched_row_indices[idx]
            {
                span.height += 1;
                next_active.push(span);
                matched_row_indices[idx] = true;
            } else {
                path_count = path_count.saturating_add(1);
                if path_count > MAX_VECTOR_SPANS {
                    return Err(Error::LimitExceeded(format!(
                        "raster vectorization exceeds {MAX_VECTOR_SPANS} path spans"
                    )));
                }
                path_d.push_str(&format!(
                    "M {},{} h {} v {} h -{} Z ",
                    span.x, span.y, span.width, span.height, span.width
                ));
            }
        }

        for (idx, &(rx, rw)) in row_spans.iter().enumerate() {
            if !matched_row_indices[idx] {
                next_active.push(ActiveSpan {
                    x: rx,
                    width: rw,
                    y,
                    height: 1,
                });
            }
        }

        std::mem::swap(&mut active_spans, &mut next_active);
    }

    for span in active_spans {
        path_count = path_count.saturating_add(1);
        if path_count > MAX_VECTOR_SPANS {
            return Err(Error::LimitExceeded(format!(
                "raster vectorization exceeds {MAX_VECTOR_SPANS} path spans"
            )));
        }
        path_d.push_str(&format!(
            "M {},{} h {} v {} h -{} Z ",
            span.x, span.y, span.width, span.height, span.width
        ));
    }

    let mut page = Page::new(1, width as f64, height as f64, "vectorized");
    page.nodes.push(Node::Path {
        id: "vectorized_path".to_string(),
        d: path_d,
        fill_rule: "evenodd".to_string(),
        fill: Paint::solid("#000000"),
        stroke: Stroke::default(),
        transform: IDENTITY,
        clip_id: None,
        meta: SourceMeta::default(),
    });

    Ok(page)
}

fn decode_bmp(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    if bytes.len() < 54 || &bytes[0..2] != b"BM" {
        return Err(Error::InvalidInput(
            "invalid BMP signature or header".into(),
        ));
    }
    let pixel_offset = u32::from_le_bytes(
        bytes[10..14]
            .try_into()
            .map_err(|_| Error::InvalidInput("BMP header too short".into()))?,
    ) as usize;
    let width = i32::from_le_bytes(
        bytes[18..22]
            .try_into()
            .map_err(|_| Error::InvalidInput("BMP header too short".into()))?,
    );
    let raw_height = i32::from_le_bytes(
        bytes[22..26]
            .try_into()
            .map_err(|_| Error::InvalidInput("BMP header too short".into()))?,
    );
    let bpp = u16::from_le_bytes(
        bytes[28..30]
            .try_into()
            .map_err(|_| Error::InvalidInput("BMP header too short".into()))?,
    );
    let compression = u32::from_le_bytes(
        bytes[30..34]
            .try_into()
            .map_err(|_| Error::InvalidInput("BMP header too short".into()))?,
    );

    if width <= 0 || raw_height == 0 {
        return Err(Error::InvalidInput("invalid BMP dimensions".into()));
    }
    if compression != 0 {
        return Err(Error::Unsupported("compressed BMP not supported".into()));
    }

    let w = width as usize;
    let h = raw_height.unsigned_abs() as usize;
    let pixel_count = validate_dimensions(w, h)?;
    let top_down = raw_height < 0;

    let mut palette = Vec::new();
    if bpp <= 8 {
        let bi_size = u32::from_le_bytes(
            bytes[14..18]
                .try_into()
                .map_err(|_| Error::InvalidInput("BMP header too short".into()))?,
        ) as usize;
        let clr_used = if bytes.len() >= 50 {
            u32::from_le_bytes(bytes[46..50].try_into().unwrap_or([0, 0, 0, 0])) as usize
        } else {
            0
        };
        let max_entries = 1usize << (bpp as usize);
        let num_colors = if clr_used > 0 && clr_used <= max_entries {
            clr_used
        } else {
            max_entries
        };
        let palette_offset = 14 + bi_size;
        for i in 0..num_colors {
            let entry_offset = palette_offset + i * 4;
            if entry_offset + 3 < bytes.len() && entry_offset + 3 < pixel_offset {
                let b = bytes[entry_offset] as u32;
                let g = bytes[entry_offset + 1] as u32;
                let r = bytes[entry_offset + 2] as u32;
                let lum = ((r * 299 + g * 587 + b * 114) / 1000) as u8;
                palette.push(lum);
            } else if bpp == 1 {
                palette.push(if i == 0 { 0 } else { 255 });
            } else {
                palette.push((i * 255 / (max_entries - 1).max(1)) as u8);
            }
        }
    }

    let row_stride = match bpp {
        32 => w * 4,
        24 => (w * 3 + 3) & !3,
        8 => (w + 3) & !3,
        4 => (w.div_ceil(2) + 3) & !3,
        1 => (w.div_ceil(8) + 3) & !3,
        _ => return Err(Error::Unsupported(format!("unsupported BMP bpp: {bpp}"))),
    };

    if bytes.len() < pixel_offset.saturating_add(row_stride.saturating_mul(h)) {
        return Err(Error::InvalidInput("BMP file truncated".into()));
    }

    let mut gray = vec![0u8; pixel_count];
    for row in 0..h {
        let src_row = if top_down { row } else { h - 1 - row };
        let row_start = pixel_offset + src_row * row_stride;
        let dst_start = row * w;
        match bpp {
            32 => {
                for col in 0..w {
                    let b = bytes[row_start + col * 4];
                    let g = bytes[row_start + col * 4 + 1];
                    let r = bytes[row_start + col * 4 + 2];
                    let a = bytes[row_start + col * 4 + 3];
                    if a < 128 {
                        gray[dst_start + col] = 255;
                    } else {
                        let lum = ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8;
                        gray[dst_start + col] = lum;
                    }
                }
            }
            24 => {
                for col in 0..w {
                    let b = bytes[row_start + col * 3];
                    let g = bytes[row_start + col * 3 + 1];
                    let r = bytes[row_start + col * 3 + 2];
                    let lum = ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8;
                    gray[dst_start + col] = lum;
                }
            }
            8 => {
                for col in 0..w {
                    let idx = bytes[row_start + col] as usize;
                    gray[dst_start + col] = palette.get(idx).copied().unwrap_or(idx as u8);
                }
            }
            4 => {
                for col in 0..w {
                    let byte_idx = row_start + col / 2;
                    let nibble = if col % 2 == 0 {
                        (bytes[byte_idx] >> 4) & 0x0F
                    } else {
                        bytes[byte_idx] & 0x0F
                    } as usize;
                    gray[dst_start + col] =
                        palette.get(nibble).copied().unwrap_or((nibble * 17) as u8);
                }
            }
            1 => {
                for col in 0..w {
                    let byte_idx = row_start + col / 8;
                    let bit = ((bytes[byte_idx] >> (7 - (col % 8))) & 1) as usize;
                    gray[dst_start + col] =
                        palette
                            .get(bit)
                            .copied()
                            .unwrap_or(if bit == 0 { 0 } else { 255 });
                }
            }
            _ => unreachable!(),
        }
    }

    Ok((w, h, gray))
}

pub fn decode_tiff(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let mut limits = tiff::decoder::Limits::default();
    limits.decoding_buffer_size = 96 * 1024 * 1024;
    limits.intermediate_buffer_size = 96 * 1024 * 1024;
    let mut decoder = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| Error::InvalidInput(format!("TIFF decoder error: {e}")))?
        .with_limits(limits);
    let (width, height) = decoder
        .dimensions()
        .map_err(|e| Error::InvalidInput(format!("TIFF dimensions error: {e}")))?;
    let w = width as usize;
    let h = height as usize;
    let pixel_count = validate_dimensions(w, h)?;
    let result = decoder
        .read_image()
        .map_err(|e| Error::InvalidInput(format!("TIFF read_image error: {e}")))?;
    let mut gray = Vec::with_capacity(pixel_count);

    match result {
        tiff::decoder::DecodingResult::U8(buf) => {
            let colortype = decoder
                .colortype()
                .map_err(|e| Error::InvalidInput(format!("TIFF colortype error: {e}")))?;
            match colortype {
                tiff::ColorType::Gray(8) => {
                    let needed = (w * h).min(buf.len());
                    gray.extend_from_slice(&buf[..needed]);
                    gray.resize(w * h, 255);
                }
                tiff::ColorType::RGB(8) => {
                    for chunk in buf.chunks_exact(3) {
                        let lum = ((chunk[0] as u32 * 299
                            + chunk[1] as u32 * 587
                            + chunk[2] as u32 * 114)
                            / 1000) as u8;
                        gray.push(lum);
                    }
                    gray.resize(w * h, 255);
                }
                tiff::ColorType::RGBA(8) => {
                    for chunk in buf.chunks_exact(4) {
                        if chunk[3] < 128 {
                            gray.push(255);
                        } else {
                            let lum = ((chunk[0] as u32 * 299
                                + chunk[1] as u32 * 587
                                + chunk[2] as u32 * 114)
                                / 1000) as u8;
                            gray.push(lum);
                        }
                    }
                    gray.resize(w * h, 255);
                }
                _ => {
                    let needed = (w * h).min(buf.len());
                    gray.extend_from_slice(&buf[..needed]);
                    gray.resize(w * h, 255);
                }
            }
        }
        _ => {
            gray.resize(w * h, 255);
        }
    }
    Ok((w, h, gray))
}

#[cfg(test)]
mod tests {
    use super::vectorize_grayscale;
    use crate::error::Error;

    #[test]
    fn rejects_oversized_raster_dimensions_before_reading_pixels() {
        let error = vectorize_grayscale(5_000, 5_000, &[], 128).unwrap_err();
        assert!(matches!(error, Error::LimitExceeded(_)));
    }

    #[test]
    fn rejects_an_incomplete_pixel_buffer() {
        let error = vectorize_grayscale(2, 2, &[0, 0, 0], 128).unwrap_err();
        assert!(matches!(error, Error::InvalidInput(_)));
    }

    #[test]
    fn rejects_raster_images_with_pathological_vector_complexity() {
        let width = 1_001usize;
        let height = 1_000usize;
        let pixels = (0..width * height)
            .map(|index| {
                let x = index % width;
                let y = index / width;
                if (x + y).is_multiple_of(2) { 0 } else { 255 }
            })
            .collect::<Vec<_>>();

        let error = vectorize_grayscale(width, height, &pixels, 128).unwrap_err();

        assert!(matches!(error, Error::LimitExceeded(_)));
    }
}
