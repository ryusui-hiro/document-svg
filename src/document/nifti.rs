//! Bounded NIfTI-1 scalar volume preview.
//!
//! Single-file NIfTI volumes are rendered as one grayscale PNG-backed SVG
//! page per z/time slice. Affine orientation, labels, extensions, and color
//! lookup tables are deliberately omitted.

use std::io::{Cursor, Read};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use flate2::read::GzDecoder;
use png::Encoder;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};

const MAX_NIFTI_INPUT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_NIFTI_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;
const MAX_NIFTI_DIMENSION: usize = 4096;
const MAX_NIFTI_VOXELS: u64 = 200_000_000;
const MAX_NIFTI_FRAMES: usize = 1_000;
const MAX_NIFTI_SLICE_PNG_BYTES: usize = 16 * 1024 * 1024;
const MAX_NIFTI_TOTAL_URI_BYTES: usize = 512 * 1024 * 1024;

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    if bytes.len() < 348 {
        return false;
    }
    let header =
        read_i32(bytes, 0, Endian::Little) == 348 || read_i32(bytes, 0, Endian::Big) == 348;
    header && matches!(bytes.get(344..348), Some(b"n+1\0") | Some(b"ni1\0"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let compressed = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_NIFTI_INPUT_BYTES),
        "NIfTI input",
    )?;
    let bytes = if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".nii.gz"))
    {
        let decoder = GzDecoder::new(Cursor::new(compressed));
        let mut decompressed = Vec::new();
        decoder
            .take(MAX_NIFTI_DECOMPRESSED_BYTES as u64 + 1)
            .read_to_end(&mut decompressed)?;
        if decompressed.len() > MAX_NIFTI_DECOMPRESSED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "NIfTI gzip stream exceeds {MAX_NIFTI_DECOMPRESSED_BYTES} decompressed bytes"
            )));
        }
        decompressed
    } else {
        compressed
    };
    let (blocks, warnings) = parse_volume(&bytes, options.max_pages)?;
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(warnings)
}

fn parse_volume(bytes: &[u8], max_pages: usize) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if bytes.len() < 352 {
        return Err(Error::InvalidInput(
            "NIfTI header is shorter than 352 bytes".into(),
        ));
    }
    let endian = if read_i32(bytes, 0, Endian::Little) == 348 {
        Endian::Little
    } else if read_i32(bytes, 0, Endian::Big) == 348 {
        Endian::Big
    } else {
        return Err(Error::InvalidInput("NIfTI sizeof_hdr is not 348".into()));
    };
    if !matches!(bytes.get(344..348), Some(b"n+1\0") | Some(b"ni1\0")) {
        return Err(Error::InvalidInput("NIfTI magic is not n+1 or ni1".into()));
    }
    let mut dimensions = [1usize; 8];
    for (index, dimension) in dimensions.iter_mut().enumerate().skip(1).take(5) {
        let raw = read_i16(bytes, 40 + index * 2, endian);
        if raw < 0 {
            return Err(Error::InvalidInput(
                "NIfTI dimensions cannot be negative".into(),
            ));
        }
        *dimension = usize::try_from(raw).unwrap_or(0).max(1);
        if *dimension > MAX_NIFTI_DIMENSION {
            return Err(Error::LimitExceeded(format!(
                "NIfTI dimension exceeds {MAX_NIFTI_DIMENSION}"
            )));
        }
    }
    let width = dimensions[1];
    let height = dimensions[2];
    let slices = dimensions[3];
    let frames = dimensions[4];
    if frames > MAX_NIFTI_FRAMES {
        return Err(Error::LimitExceeded(format!(
            "NIfTI frame count exceeds {MAX_NIFTI_FRAMES}"
        )));
    }
    let pages = slices
        .checked_mul(frames)
        .ok_or_else(|| Error::LimitExceeded("NIfTI page count overflowed".into()))?;
    if pages == 0 || pages > max_pages {
        return Err(Error::LimitExceeded(format!(
            "NIfTI has {pages} slices/frames but max_pages is {max_pages}"
        )));
    }
    let voxels = u64::try_from(width)
        .unwrap_or(u64::MAX)
        .saturating_mul(u64::try_from(height).unwrap_or(u64::MAX))
        .saturating_mul(u64::try_from(slices).unwrap_or(u64::MAX))
        .saturating_mul(u64::try_from(frames).unwrap_or(u64::MAX));
    if voxels > MAX_NIFTI_VOXELS {
        return Err(Error::LimitExceeded(format!(
            "NIfTI voxel count exceeds {MAX_NIFTI_VOXELS}"
        )));
    }
    let datatype = read_u16(bytes, 70, endian);
    let bitpix = read_u16(bytes, 72, endian);
    let bytes_per_voxel = usize::from(bitpix / 8);
    if bytes_per_voxel == 0 || !matches!(datatype, 2 | 4 | 8 | 16 | 64 | 256 | 512 | 768) {
        return Err(Error::Unsupported(format!(
            "NIfTI datatype {datatype} or bitpix {bitpix} is unsupported"
        )));
    }
    let data_bytes = usize::try_from(voxels)
        .ok()
        .and_then(|count| count.checked_mul(bytes_per_voxel))
        .ok_or_else(|| Error::LimitExceeded("NIfTI voxel byte count overflowed".into()))?;
    let vox_offset = read_f32(bytes, 108, endian);
    let data_offset = if vox_offset.is_finite() && vox_offset >= 352.0 {
        usize::try_from(vox_offset as u64)
            .map_err(|_| Error::InvalidInput("NIfTI vox_offset is invalid".into()))?
    } else {
        352
    };
    let data_end = data_offset
        .checked_add(data_bytes)
        .ok_or_else(|| Error::LimitExceeded("NIfTI data range overflowed".into()))?;
    if data_end > bytes.len() {
        return Err(Error::InvalidInput("NIfTI voxel data is truncated".into()));
    }
    let slope_raw = read_f32(bytes, 112, endian);
    let intercept_raw = read_f32(bytes, 116, endian);
    let slope = if slope_raw.is_finite() && slope_raw != 0.0 {
        slope_raw
    } else {
        1.0
    };
    let intercept = if intercept_raw.is_finite() {
        intercept_raw
    } else {
        0.0
    };
    let values = decode_values(
        &bytes[data_offset..data_end],
        datatype,
        endian,
        slope,
        intercept,
    )?;
    let (min, max) = values
        .iter()
        .copied()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(min, max), value| {
            (min.min(value), max.max(value))
        });
    let mut warnings = vec![
        "NIfTI affine orientation, labels, extensions, and color lookup tables are omitted".into(),
        "NIfTI slices are rendered as grayscale PNG images".into(),
    ];
    let mut blocks = Vec::new();
    let mut total_uri_bytes = 0usize;
    let slice_voxels = width.saturating_mul(height);
    for page in 0..pages {
        let base = page.saturating_mul(slice_voxels);
        let pixels = values[base..base + slice_voxels]
            .iter()
            .map(|value| scale_sample(*value, min, max))
            .collect::<Vec<_>>();
        let png = encode_gray_png(width as u32, height as u32, &pixels)?;
        if png.len() > MAX_NIFTI_SLICE_PNG_BYTES {
            return Err(Error::LimitExceeded(format!(
                "NIfTI slice PNG exceeds {MAX_NIFTI_SLICE_PNG_BYTES} bytes"
            )));
        }
        let href = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(&png));
        total_uri_bytes = total_uri_bytes.saturating_add(href.len());
        if total_uri_bytes > MAX_NIFTI_TOTAL_URI_BYTES {
            return Err(Error::LimitExceeded(format!(
                "NIfTI image data URI bytes exceed {MAX_NIFTI_TOTAL_URI_BYTES}"
            )));
        }
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: format!("NIfTI slice {}", page + 1),
        });
        blocks.push(HtmlBlock::Image {
            href,
            pixel_width: width as u32,
            pixel_height: height as u32,
            alt: format!("NIfTI grayscale slice {}", page + 1),
        });
        if page + 1 < pages {
            blocks.push(HtmlBlock::PageBreak);
        }
    }
    Ok((blocks, std::mem::take(&mut warnings)))
}

fn decode_values(
    bytes: &[u8],
    datatype: u16,
    endian: Endian,
    slope: f32,
    intercept: f32,
) -> Result<Vec<f32>> {
    let width = match datatype {
        2 | 256 => 1,
        4 | 512 => 2,
        8 | 16 | 768 => 4,
        64 => 8,
        _ => unreachable!(),
    };
    if !bytes.len().is_multiple_of(width) {
        return Err(Error::InvalidInput(
            "NIfTI voxel data has an invalid stride".into(),
        ));
    }
    let mut values = Vec::with_capacity(bytes.len() / width);
    for chunk in bytes.chunks_exact(width) {
        let raw = match datatype {
            2 => f32::from(chunk[0]),
            256 => (chunk[0] as i8) as f32,
            4 => read_i16(chunk, 0, endian) as f32,
            512 => read_u16(chunk, 0, endian) as f32,
            8 => read_i32(chunk, 0, endian) as f32,
            768 => read_u32(chunk, 0, endian) as f32,
            16 => read_f32(chunk, 0, endian),
            64 => read_f64(chunk, 0, endian) as f32,
            _ => unreachable!(),
        };
        values.push(raw.mul_add(slope, intercept));
    }
    Ok(values)
}

fn scale_sample(value: f32, min: f32, max: f32) -> u8 {
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
        .map_err(|error| Error::InvalidInput(format!("cannot encode NIfTI PNG: {error}")))?;
    writer
        .write_image_data(pixels)
        .map_err(|error| Error::InvalidInput(format!("cannot encode NIfTI PNG: {error}")))?;
    drop(writer);
    Ok(png)
}

fn read_u16(bytes: &[u8], offset: usize, endian: Endian) -> u16 {
    let data = [bytes[offset], bytes[offset + 1]];
    match endian {
        Endian::Little => u16::from_le_bytes(data),
        Endian::Big => u16::from_be_bytes(data),
    }
}

fn read_i16(bytes: &[u8], offset: usize, endian: Endian) -> i16 {
    read_u16(bytes, offset, endian) as i16
}

fn read_u32(bytes: &[u8], offset: usize, endian: Endian) -> u32 {
    let data = [
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ];
    match endian {
        Endian::Little => u32::from_le_bytes(data),
        Endian::Big => u32::from_be_bytes(data),
    }
}

fn read_i32(bytes: &[u8], offset: usize, endian: Endian) -> i32 {
    read_u32(bytes, offset, endian) as i32
}

fn read_f32(bytes: &[u8], offset: usize, endian: Endian) -> f32 {
    f32::from_bits(read_u32(bytes, offset, endian))
}

fn read_f64(bytes: &[u8], offset: usize, endian: Endian) -> f64 {
    let data = [
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ];
    match endian {
        Endian::Little => f64::from_le_bytes(data),
        Endian::Big => f64::from_be_bytes(data),
    }
}
