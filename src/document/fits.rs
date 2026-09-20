//! Bounded FITS astronomical image preview.
//!
//! Reads the primary image HDU of a FITS file, validates the fixed 2880-byte
//! header and common BITPIX/NAXIS cards, and renders each image plane as a
//! grayscale PNG-backed SVG page. Tables, extensions, WCS, provenance and
//! instrument metadata remain inert and are reported as omitted.

use std::io::{Cursor, Read};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use flate2::read::GzDecoder;
use png::Encoder;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};

const MAX_FITS_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FITS_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;
const MAX_FITS_DIMENSION: usize = 8192;
const MAX_FITS_PIXELS: u64 = 300_000_000;
const MAX_FITS_PLANES: usize = 1_000;
const MAX_FITS_SLICE_PNG_BYTES: usize = 16 * 1024 * 1024;
const MAX_FITS_TOTAL_URI_BYTES: usize = 512 * 1024 * 1024;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 80 && bytes[0..8] == *b"SIMPLE  "
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let compressed = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_FITS_INPUT_BYTES),
        "FITS input",
    )?;
    let bytes = if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".fits.gz"))
    {
        let decoder = GzDecoder::new(Cursor::new(compressed));
        let mut decompressed = Vec::new();
        decoder
            .take(MAX_FITS_DECOMPRESSED_BYTES as u64 + 1)
            .read_to_end(&mut decompressed)?;
        if decompressed.len() > MAX_FITS_DECOMPRESSED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "FITS gzip stream exceeds {MAX_FITS_DECOMPRESSED_BYTES} decompressed bytes"
            )));
        }
        decompressed
    } else {
        compressed
    };
    let (blocks, warnings) = parse_primary_image(&bytes, options.max_pages)?;
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn parse_primary_image(bytes: &[u8], max_pages: usize) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if bytes.len() < 2880 || bytes[0..8] != *b"SIMPLE  " {
        return Err(Error::InvalidInput(
            "FITS input is missing a SIMPLE primary header".into(),
        ));
    }
    let header_end = find_header_end(bytes)?;
    let header = &bytes[..header_end];
    let bitpix = parse_card_i64(header, "BITPIX")?;
    let naxis = parse_card_i64(header, "NAXIS")?;
    if !(1..=4).contains(&naxis) {
        return Err(Error::Unsupported(format!(
            "FITS NAXIS {naxis} is unsupported; expected 1 through 4"
        )));
    }
    let mut dimensions = [1usize; 4];
    for (index, dimension) in dimensions.iter_mut().enumerate().take(naxis as usize) {
        let value = parse_card_i64(header, &format!("NAXIS{}", index + 1))?;
        if value <= 0 || value as usize > MAX_FITS_DIMENSION {
            return Err(Error::LimitExceeded(format!(
                "FITS NAXIS{} exceeds {MAX_FITS_DIMENSION}",
                index + 1
            )));
        }
        *dimension = value as usize;
    }
    let planes = dimensions[2].saturating_mul(dimensions[3]).max(1);
    if planes > MAX_FITS_PLANES || planes > max_pages {
        return Err(Error::LimitExceeded(format!(
            "FITS contains {planes} image planes but max_pages is {max_pages}"
        )));
    }
    let pixels = u64::try_from(dimensions[0])
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(dimensions[1]).unwrap_or(u64::MAX))
        .saturating_mul(u64::try_from(planes).unwrap_or(u64::MAX));
    if pixels > MAX_FITS_PIXELS {
        return Err(Error::LimitExceeded(format!(
            "FITS image pixels exceed {MAX_FITS_PIXELS}"
        )));
    }
    let bytes_per_value = match bitpix {
        8 => 1,
        16 => 2,
        32 | -32 => 4,
        64 | -64 => 8,
        _ => {
            return Err(Error::Unsupported(format!(
                "FITS BITPIX {bitpix} is unsupported"
            )));
        }
    };
    let total_data = usize::try_from(pixels)
        .ok()
        .and_then(|count| count.checked_mul(bytes_per_value))
        .ok_or_else(|| Error::LimitExceeded("FITS data size overflowed".into()))?;
    let data_offset = align_2880(header_end);
    let data_end = data_offset
        .checked_add(total_data)
        .ok_or_else(|| Error::LimitExceeded("FITS data range overflowed".into()))?;
    if data_end > bytes.len() {
        return Err(Error::InvalidInput(
            "FITS primary image data is truncated".into(),
        ));
    }
    let bscale = parse_card_f64(header, "BSCALE").unwrap_or(1.0);
    let bzero = parse_card_f64(header, "BZERO").unwrap_or(0.0);
    let values = decode_values(&bytes[data_offset..data_end], bitpix, bscale, bzero)?;
    let (min, max) = values
        .iter()
        .copied()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
            (min.min(value), max.max(value))
        });
    let mut warnings = vec![
        "FITS extension HDUs, tables, WCS, provenance, and instrument metadata are omitted".into(),
        "FITS image planes are rendered as grayscale PNG images".into(),
    ];
    let mut blocks = Vec::new();
    let mut total_uri_bytes = 0usize;
    let plane_pixels = dimensions[0].saturating_mul(dimensions[1]);
    for plane in 0..planes {
        let start = plane.saturating_mul(plane_pixels);
        let pixels = values[start..start + plane_pixels]
            .iter()
            .map(|value| scale_sample(*value, min, max))
            .collect::<Vec<_>>();
        let png = encode_gray_png(dimensions[0] as u32, dimensions[1] as u32, &pixels)?;
        if png.len() > MAX_FITS_SLICE_PNG_BYTES {
            return Err(Error::LimitExceeded(format!(
                "FITS plane PNG exceeds {MAX_FITS_SLICE_PNG_BYTES} bytes"
            )));
        }
        let href = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(&png));
        total_uri_bytes = total_uri_bytes.saturating_add(href.len());
        if total_uri_bytes > MAX_FITS_TOTAL_URI_BYTES {
            return Err(Error::LimitExceeded(format!(
                "FITS image data URI bytes exceed {MAX_FITS_TOTAL_URI_BYTES}"
            )));
        }
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: format!("FITS image plane {}", plane + 1),
        });
        blocks.push(HtmlBlock::Image {
            href,
            pixel_width: dimensions[0] as u32,
            pixel_height: dimensions[1] as u32,
            alt: format!("FITS grayscale plane {}", plane + 1),
        });
        if plane + 1 < planes {
            blocks.push(HtmlBlock::PageBreak);
        }
    }
    Ok((blocks, std::mem::take(&mut warnings)))
}

fn find_header_end(bytes: &[u8]) -> Result<usize> {
    for block_start in (0..bytes.len()).step_by(2880) {
        let block_end = block_start.saturating_add(2880).min(bytes.len());
        if block_end - block_start < 2880 {
            break;
        }
        for card_start in (block_start..block_end).step_by(80) {
            let card = &bytes[card_start..card_start + 80];
            if card.starts_with(b"END") {
                return Ok(block_end);
            }
        }
    }
    Err(Error::InvalidInput("FITS header has no END card".into()))
}

fn align_2880(value: usize) -> usize {
    value.div_ceil(2880) * 2880
}

fn card_value<'a>(header: &'a [u8], key: &str) -> Option<&'a str> {
    for card in header.chunks_exact(80) {
        let name = std::str::from_utf8(&card[..8]).ok()?.trim();
        if name == key {
            let text = std::str::from_utf8(&card[10..]).ok()?;
            return text.split('/').next().map(str::trim);
        }
    }
    None
}

fn parse_card_i64(header: &[u8], key: &str) -> Result<i64> {
    card_value(header, key)
        .ok_or_else(|| Error::InvalidInput(format!("FITS header is missing {key}")))?
        .parse::<i64>()
        .map_err(|_| Error::InvalidInput(format!("FITS {key} is invalid")))
}

fn parse_card_f64(header: &[u8], key: &str) -> Option<f64> {
    card_value(header, key)?
        .replace('D', "E")
        .parse::<f64>()
        .ok()
}

fn decode_values(bytes: &[u8], bitpix: i64, scale: f64, zero: f64) -> Result<Vec<f64>> {
    let width = (bitpix.unsigned_abs() / 8) as usize;
    if width == 0 || !bytes.len().is_multiple_of(width) {
        return Err(Error::InvalidInput(
            "FITS pixel data has an invalid stride".into(),
        ));
    }
    let mut values = Vec::with_capacity(bytes.len() / width);
    for chunk in bytes.chunks_exact(width) {
        let raw = match bitpix {
            8 => f64::from(chunk[0]),
            16 => i16::from_be_bytes([chunk[0], chunk[1]]) as f64,
            32 => i32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f64,
            64 => i64::from_be_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ]) as f64,
            -32 => {
                f32::from_bits(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])) as f64
            }
            -64 => f64::from_bits(u64::from_be_bytes([
                chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
            ])),
            _ => unreachable!(),
        };
        values.push(raw.mul_add(scale, zero));
    }
    Ok(values)
}

fn scale_sample(value: f64, min: f64, max: f64) -> u8 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() || max <= min {
        return 0;
    }
    (((value - min) / (max - min)).clamp(0.0, 1.0) * 255.0).round() as u8
}

fn encode_gray_png(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    let mut png = Vec::new();
    let mut encoder = Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| Error::InvalidInput(format!("cannot encode FITS PNG: {error}")))?;
    writer
        .write_image_data(pixels)
        .map_err(|error| Error::InvalidInput(format!("cannot encode FITS PNG: {error}")))?;
    drop(writer);
    Ok(png)
}
