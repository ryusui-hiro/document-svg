//! Bounded MRC/CCP4 scalar density-map preview.
//!
//! Validates the primary 1024-byte MRC header and renders mode 0/1/2/6
//! volumes as grayscale PNG-backed SVG pages. Labels, symmetry records,
//! origin/axis transforms, and extended metadata are omitted.

use std::io::{Cursor, Read};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use flate2::read::GzDecoder;
use png::Encoder;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};

const MAX_MRC_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MRC_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;
const MAX_MRC_DIMENSION: usize = 8192;
const MAX_MRC_VOXELS: u64 = 300_000_000;
const MAX_MRC_PLANES: usize = 1_000;
const MAX_MRC_PNG_BYTES: usize = 16 * 1024 * 1024;
const MAX_MRC_URI_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    if bytes.len() < 216 {
        return false;
    }
    let map = bytes.get(208..212) == Some(b"MAP ");
    let little = read_i32(bytes, 0, Endian::Little).is_some_and(|value| value > 0);
    let big = read_i32(bytes, 0, Endian::Big).is_some_and(|value| value > 0);
    map && (little || big)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let compressed = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_MRC_INPUT_BYTES),
        "MRC input",
    )?;
    let bytes = if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".mrc.gz"))
    {
        let decoder = GzDecoder::new(Cursor::new(compressed));
        let mut decompressed = Vec::new();
        decoder
            .take(MAX_MRC_DECOMPRESSED_BYTES as u64 + 1)
            .read_to_end(&mut decompressed)?;
        if decompressed.len() > MAX_MRC_DECOMPRESSED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "MRC gzip stream exceeds {MAX_MRC_DECOMPRESSED_BYTES} decompressed bytes"
            )));
        }
        decompressed
    } else {
        compressed
    };
    let (blocks, warnings) = parse_map(&bytes, options.max_pages)?;
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn parse_map(bytes: &[u8], max_pages: usize) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if bytes.len() < 1024 {
        return Err(Error::InvalidInput(
            "MRC input is shorter than the 1024-byte header".into(),
        ));
    }
    let endian = detect_endian(bytes)?;
    let nx = dimension(bytes, 0, endian)?;
    let ny = dimension(bytes, 4, endian)?;
    let nz = dimension(bytes, 8, endian)?;
    let mode = read_i32(bytes, 12, endian)
        .ok_or_else(|| Error::InvalidInput("MRC mode is missing".into()))?;
    if !matches!(mode, 0 | 1 | 2 | 6) {
        return Err(Error::Unsupported(format!(
            "MRC mode {mode} is unsupported"
        )));
    }
    if nz > MAX_MRC_PLANES || nz > max_pages {
        return Err(Error::LimitExceeded(format!(
            "MRC has {nz} planes but max_pages is {max_pages}"
        )));
    }
    let voxels = u64::try_from(nx)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(ny).unwrap_or(u64::MAX))
        .saturating_mul(u64::try_from(nz).unwrap_or(u64::MAX));
    if voxels > MAX_MRC_VOXELS {
        return Err(Error::LimitExceeded(format!(
            "MRC voxel count exceeds {MAX_MRC_VOXELS}"
        )));
    }
    let bytes_per_value = match mode {
        0 | 6 => 1,
        1 => 2,
        2 => 4,
        _ => unreachable!(),
    };
    let total_data = usize::try_from(voxels)
        .ok()
        .and_then(|count| count.checked_mul(bytes_per_value))
        .ok_or_else(|| Error::LimitExceeded("MRC data size overflowed".into()))?;
    let nsymbt = read_i32(bytes, 92, endian)
        .ok_or_else(|| Error::InvalidInput("MRC nsymbt is missing".into()))?;
    if nsymbt < 0 {
        return Err(Error::InvalidInput("MRC nsymbt is negative".into()));
    }
    let data_offset = 1024usize
        .checked_add(nsymbt as usize)
        .ok_or_else(|| Error::LimitExceeded("MRC data offset overflowed".into()))?;
    let data_end = data_offset
        .checked_add(total_data)
        .ok_or_else(|| Error::LimitExceeded("MRC data range overflowed".into()))?;
    if data_end > bytes.len() {
        return Err(Error::InvalidInput("MRC voxel data is truncated".into()));
    }
    let values = decode_values(&bytes[data_offset..data_end], mode, endian)?;
    let (min, max) = values
        .iter()
        .copied()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
            (min.min(value), max.max(value))
        });
    let mut warnings = vec![
        "MRC axis mapping, origin, labels, symmetry and extended metadata are omitted".into(),
        "MRC planes are rendered as grayscale PNG images".into(),
    ];
    let mut blocks = Vec::new();
    let mut uri_bytes = 0usize;
    let plane_size = nx.saturating_mul(ny);
    for plane in 0..nz {
        let start = plane.saturating_mul(plane_size);
        let pixels = values[start..start + plane_size]
            .iter()
            .map(|value| scale(*value, min, max))
            .collect::<Vec<_>>();
        let png = encode_png(nx as u32, ny as u32, &pixels)?;
        if png.len() > MAX_MRC_PNG_BYTES {
            return Err(Error::LimitExceeded(format!(
                "MRC plane PNG exceeds {MAX_MRC_PNG_BYTES} bytes"
            )));
        }
        let href = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(&png));
        uri_bytes = uri_bytes.saturating_add(href.len());
        if uri_bytes > MAX_MRC_URI_BYTES {
            return Err(Error::LimitExceeded(format!(
                "MRC data URI bytes exceed {MAX_MRC_URI_BYTES}"
            )));
        }
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: format!("MRC plane {}", plane + 1),
        });
        blocks.push(HtmlBlock::Image {
            href,
            pixel_width: nx as u32,
            pixel_height: ny as u32,
            alt: format!("MRC grayscale plane {}", plane + 1),
        });
        if plane + 1 < nz {
            blocks.push(HtmlBlock::PageBreak);
        }
    }
    Ok((blocks, std::mem::take(&mut warnings)))
}

fn detect_endian(bytes: &[u8]) -> Result<Endian> {
    let little = read_i32(bytes, 0, Endian::Little);
    let big = read_i32(bytes, 0, Endian::Big);
    if bytes.get(208..212) == Some(b"MAP ")
        && little.is_some_and(|value| value > 0 && value <= MAX_MRC_DIMENSION as i32)
    {
        Ok(Endian::Little)
    } else if bytes.get(208..212) == Some(b"MAP ")
        && big.is_some_and(|value| value > 0 && value <= MAX_MRC_DIMENSION as i32)
    {
        Ok(Endian::Big)
    } else {
        Err(Error::InvalidInput(
            "MRC header lacks a valid MAP signature/dimension".into(),
        ))
    }
}

fn dimension(bytes: &[u8], offset: usize, endian: Endian) -> Result<usize> {
    let value = read_i32(bytes, offset, endian)
        .ok_or_else(|| Error::InvalidInput("MRC dimension is missing".into()))?;
    if value <= 0 || value as usize > MAX_MRC_DIMENSION {
        return Err(Error::LimitExceeded(format!(
            "MRC dimension exceeds {MAX_MRC_DIMENSION}"
        )));
    }
    Ok(value as usize)
}

fn decode_values(bytes: &[u8], mode: i32, endian: Endian) -> Result<Vec<f64>> {
    let width = match mode {
        0 | 6 => 1,
        1 => 2,
        2 => 4,
        _ => unreachable!(),
    };
    if !bytes.len().is_multiple_of(width) {
        return Err(Error::InvalidInput("MRC data has an invalid stride".into()));
    }
    let mut values = Vec::with_capacity(bytes.len() / width);
    for chunk in bytes.chunks_exact(width) {
        let value = match mode {
            0 => (chunk[0] as i8) as f64,
            6 => f64::from(chunk[0]),
            1 => read_i16(chunk, 0, endian) as f64,
            2 => read_f32(chunk, 0, endian) as f64,
            _ => unreachable!(),
        };
        values.push(value);
    }
    Ok(values)
}

fn scale(value: f64, min: f64, max: f64) -> u8 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() || max <= min {
        return 0;
    }
    (((value - min) / (max - min)).clamp(0.0, 1.0) * 255.0).round() as u8
}

fn encode_png(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>> {
    let mut png = Vec::new();
    let mut encoder = Encoder::new(&mut png, width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| Error::InvalidInput(format!("cannot encode MRC PNG: {error}")))?;
    writer
        .write_image_data(pixels)
        .map_err(|error| Error::InvalidInput(format!("cannot encode MRC PNG: {error}")))?;
    drop(writer);
    Ok(png)
}

fn read_i32(bytes: &[u8], offset: usize, endian: Endian) -> Option<i32> {
    let data = bytes.get(offset..offset + 4)?;
    let value = match endian {
        Endian::Little => i32::from_le_bytes(data.try_into().ok()?),
        Endian::Big => i32::from_be_bytes(data.try_into().ok()?),
    };
    Some(value)
}

fn read_i16(bytes: &[u8], offset: usize, endian: Endian) -> i16 {
    let data = [bytes[offset], bytes[offset + 1]];
    match endian {
        Endian::Little => i16::from_le_bytes(data),
        Endian::Big => i16::from_be_bytes(data),
    }
}

fn read_f32(bytes: &[u8], offset: usize, endian: Endian) -> f32 {
    let data = [
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ];
    f32::from_bits(match endian {
        Endian::Little => u32::from_le_bytes(data),
        Endian::Big => u32::from_be_bytes(data),
    })
}
