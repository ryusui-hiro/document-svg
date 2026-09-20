//! Bounded TIFF/BigTIFF reader for single- and multi-page raster documents.
//!
//! The pages are embedded as PNGs so the SVG output does not depend on browser
//! support for TIFF. Supported pages include common unsigned-integer grayscale, RGB and
//! alpha variants, plus CMYK. Wider integer channels are reduced to 8-bit.
//! Other color models and floating-point samples are rejected explicitly rather
//! than emitted with guessed color semantics.

use std::fs::File;
use std::io::BufReader;
use std::num::NonZeroUsize;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, SourceMeta};

const MAX_TIFF_PIXELS: u64 = 20_000_000;
const MAX_TIFF_TOTAL_PIXELS: u64 = 100_000_000;
const MAX_TIFF_DECODED_BYTES: usize = 96 * 1024 * 1024;
const MAX_TIFF_DATA_URI_BYTES: usize = 128 * 1024 * 1024;
const MAX_TIFF_TOTAL_DATA_URI_BYTES: usize = 512 * 1024 * 1024;
const MAX_TIFF_PAGE_DIMENSION: f64 = 1_000_000.0;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let file = File::open(path)?;
    let mut limits = Limits::default();
    limits.decoding_buffer_size = MAX_TIFF_DECODED_BYTES;
    limits.intermediate_buffer_size = MAX_TIFF_DECODED_BYTES;
    let mut decoder = Decoder::new(BufReader::new(file))
        .map_err(|error| map_tiff_error(error, "TIFF header"))?
        .with_limits(limits);
    let mut decoded = DecodingResult::U8(Vec::new());
    let mut page_number = 0usize;
    let mut total_pixels = 0u64;
    let mut total_data_uri_bytes = 0usize;

    loop {
        page_number = page_number.saturating_add(1);
        if page_number > options.max_pages {
            return Err(Error::LimitExceeded(format!(
                "TIFF contains more than {} image directories",
                options.max_pages
            )));
        }
        let (width, height) = decoder
            .dimensions()
            .map_err(|error| map_tiff_error(error, "TIFF page dimensions"))?;
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| Error::LimitExceeded("TIFF pixel count overflowed".into()))?;
        if width == 0 || height == 0 || pixels > MAX_TIFF_PIXELS {
            return Err(Error::LimitExceeded(format!(
                "TIFF page {page_number} is {width}x{height}; maximum is {MAX_TIFF_PIXELS} pixels"
            )));
        }
        total_pixels = total_pixels
            .checked_add(pixels)
            .ok_or_else(|| Error::LimitExceeded("TIFF total pixel count overflowed".into()))?;
        if total_pixels > MAX_TIFF_TOTAL_PIXELS {
            return Err(Error::LimitExceeded(format!(
                "TIFF pages exceed the {MAX_TIFF_TOTAL_PIXELS}-pixel total limit"
            )));
        }

        let color_type = decoder
            .colortype()
            .map_err(|error| map_tiff_error(error, "TIFF color model"))?;
        let has_icc_profile = {
            let image_ifd = decoder.image_ifd();
            image_ifd.directory().contains(Tag::IccProfile)
        };
        let orientation = decoder
            .find_tag_unsigned::<u16>(Tag::Orientation)
            .ok()
            .flatten()
            .filter(|value| (1..=8).contains(value))
            .unwrap_or(1);
        let (dpi_x, dpi_y) = page_resolution(&mut decoder);
        let layout = decoder
            .read_image_to_buffer(&mut decoded)
            .map_err(|error| map_tiff_error(error, "TIFF image data"))?;
        if decoded_buffer_len(&decoded) < layout.complete_len {
            return Err(Error::LimitExceeded(format!(
                "TIFF page {page_number} needs {} decoded bytes for all sample planes, exceeding the configured decoder limit",
                layout.complete_len
            )));
        }

        let png =
            encode_page_png(width, height, color_type, &layout, &decoded).map_err(|error| {
                match error {
                    Error::LimitExceeded(_) | Error::Unsupported(_) => error,
                    other => Error::InvalidInput(format!("TIFF page {page_number}: {other}")),
                }
            })?;
        let data_uri = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png));
        if data_uri.len() > MAX_TIFF_DATA_URI_BYTES {
            return Err(Error::LimitExceeded(format!(
                "TIFF page {page_number} PNG data URI exceeds {MAX_TIFF_DATA_URI_BYTES} bytes"
            )));
        }
        total_data_uri_bytes = total_data_uri_bytes
            .checked_add(data_uri.len())
            .ok_or_else(|| Error::LimitExceeded("TIFF total data URI size overflowed".into()))?;
        if total_data_uri_bytes > MAX_TIFF_TOTAL_DATA_URI_BYTES {
            return Err(Error::LimitExceeded(format!(
                "TIFF pages exceed the {MAX_TIFF_TOTAL_DATA_URI_BYTES}-byte total image limit"
            )));
        }

        let transposed = orientation >= 5;
        let (oriented_width, oriented_height) = if transposed {
            (height, width)
        } else {
            (width, height)
        };
        let (scale_x, scale_y) = if transposed {
            (72.0 / dpi_y, 72.0 / dpi_x)
        } else {
            (72.0 / dpi_x, 72.0 / dpi_y)
        };
        let page_width = f64::from(oriented_width) * scale_x;
        let page_height = f64::from(oriented_height) * scale_y;
        if !page_width.is_finite()
            || !page_height.is_finite()
            || page_width <= 0.0
            || page_height <= 0.0
            || page_width > MAX_TIFF_PAGE_DIMENSION
            || page_height > MAX_TIFF_PAGE_DIMENSION
        {
            return Err(Error::LimitExceeded(format!(
                "TIFF page {page_number} physical dimensions exceed the supported range"
            )));
        }

        let mut page = Page::new(page_number, page_width, page_height, "tiff");
        page.description = format!("TIFF image directory {page_number} ({width}x{height})");
        if color_type.bit_depth() > 8 {
            page.warn(format!(
                "TIFF page {page_number} sample channels were reduced from {}-bit to 8-bit PNG",
                color_type.bit_depth()
            ));
        }
        if matches!(color_type, ColorType::CMYK(_) | ColorType::CMYKA(_)) {
            page.warn(format!(
                "TIFF page {page_number} CMYK samples were converted with a generic RGB formula; color management is not applied"
            ));
        }
        if has_icc_profile {
            page.warn(format!(
                "TIFF page {page_number} embeds an ICC profile that was not applied"
            ));
        }
        page.nodes.push(Node::Image {
            id: format!("tiff-image-{page_number}"),
            href: data_uri,
            x: 0.0,
            y: 0.0,
            width: f64::from(width),
            height: f64::from(height),
            transform: orientation_transform(orientation, width, height, scale_x, scale_y),
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "tiff:image".into(),
                ..Default::default()
            },
        });
        sink.consume(page)?;

        if !decoder.more_images() {
            break;
        }
        decoder
            .next_image()
            .map_err(|error| map_tiff_error(error, "next TIFF image directory"))?;
    }
    Ok(Vec::new())
}

fn encode_page_png(
    width: u32,
    height: u32,
    color_type: ColorType,
    layout: &tiff::decoder::BufferLayoutPreference,
    decoded: &DecodingResult,
) -> Result<Vec<u8>> {
    let (png_color, channels, bits) = match color_type {
        ColorType::Gray(bits @ (1 | 2 | 4 | 8 | 16 | 32 | 64)) => {
            (png::ColorType::Grayscale, 1, bits)
        }
        ColorType::GrayA(bits @ (8 | 16)) => (png::ColorType::GrayscaleAlpha, 2, bits),
        ColorType::RGB(bits @ (8 | 16 | 32 | 64)) => (png::ColorType::Rgb, 3, bits),
        ColorType::RGBA(bits @ (8 | 16 | 32 | 64)) => (png::ColorType::Rgba, 4, bits),
        ColorType::CMYK(bits @ (8 | 16 | 32 | 64)) => (png::ColorType::Rgb, 4, bits),
        ColorType::CMYKA(8) => (png::ColorType::Rgba, 5, 8),
        unsupported => {
            return Err(Error::Unsupported(format!(
                "TIFF color type {unsupported:?} is not supported; expected integer grayscale, RGB, CMYK, or alpha samples"
            )));
        }
    };
    if layout.planes != 1 && layout.planes != usize::from(channels) {
        return Err(Error::Unsupported(format!(
            "TIFF sample layout has {} planes for {channels} output channels",
            layout.planes
        )));
    }
    let sample_bytes = usize::from(bits).div_ceil(8);
    let row_stride = layout.row_stride.map(NonZeroUsize::get).unwrap_or_else(|| {
        usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::from(channels) * sample_bytes)
    });
    let plane_stride = layout
        .plane_stride
        .map(NonZeroUsize::get)
        .unwrap_or_else(|| row_stride.saturating_mul(height as usize));
    let minimum_row = if bits < 8 {
        usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::from(bits))
            .div_ceil(8)
    } else if layout.planes > 1 {
        usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(sample_bytes)
    } else {
        usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::from(channels) * sample_bytes)
    };
    if row_stride < minimum_row {
        return Err(Error::InvalidInput(
            "TIFF decoder returned a row stride shorter than its pixel data".into(),
        ));
    }
    if layout.planes > 1
        && plane_stride
            < row_stride
                .checked_mul(height as usize)
                .ok_or_else(|| Error::LimitExceeded("TIFF plane stride overflowed".into()))?
    {
        return Err(Error::InvalidInput(
            "TIFF decoder returned an incomplete planar image".into(),
        ));
    }
    let bytes_per_pixel = png_color
        .samples()
        .checked_mul(usize::try_from(width).unwrap_or(0))
        .ok_or_else(|| Error::LimitExceeded("TIFF PNG row size overflowed".into()))?;
    let output_len = bytes_per_pixel
        .checked_mul(height as usize)
        .ok_or_else(|| Error::LimitExceeded("TIFF PNG size overflowed".into()))?;
    if output_len > MAX_TIFF_DECODED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "TIFF PNG output requires {output_len} bytes; maximum is {MAX_TIFF_DECODED_BYTES}"
        )));
    }
    let mut output = Vec::with_capacity(output_len);
    let packed = bits < 8;
    let is_cmyk = matches!(color_type, ColorType::CMYK(_) | ColorType::CMYKA(_));
    if bits == 8 && layout.planes == 1 && !is_cmyk {
        let DecodingResult::U8(data) = decoded else {
            return Err(Error::Unsupported(
                "8-bit TIFF samples were not decoded into a byte buffer".into(),
            ));
        };
        let row_bytes = usize::try_from(width)
            .unwrap_or(0)
            .checked_mul(usize::from(channels))
            .ok_or_else(|| Error::LimitExceeded("TIFF row size overflowed".into()))?;
        for y in 0..height as usize {
            let start = y
                .checked_mul(row_stride)
                .ok_or_else(|| Error::LimitExceeded("TIFF row offset overflowed".into()))?;
            let end = start
                .checked_add(row_bytes)
                .ok_or_else(|| Error::LimitExceeded("TIFF row offset overflowed".into()))?;
            let row = data.get(start..end).ok_or_else(|| {
                Error::InvalidInput("TIFF sample buffer ended before a complete row".into())
            })?;
            output.extend_from_slice(row);
        }
    } else {
        for y in 0..height as usize {
            for x in 0..width as usize {
                if is_cmyk {
                    let mut cmyk = [0u8; 5];
                    for (channel, slot) in cmyk.iter_mut().take(usize::from(channels)).enumerate() {
                        *slot = read_tiff_sample(
                            decoded,
                            layout,
                            x,
                            y,
                            channel,
                            channels,
                            bits,
                            sample_bytes,
                            row_stride,
                            plane_stride,
                            packed,
                        )?;
                    }
                    output.extend_from_slice(&cmyk_to_rgb(cmyk[0], cmyk[1], cmyk[2], cmyk[3]));
                    if channels == 5 {
                        output.push(cmyk[4]);
                    }
                    continue;
                }
                for channel in 0..usize::from(channels) {
                    let value = read_tiff_sample(
                        decoded,
                        layout,
                        x,
                        y,
                        channel,
                        channels,
                        bits,
                        sample_bytes,
                        row_stride,
                        plane_stride,
                        packed,
                    )?;
                    output.push(value);
                }
            }
        }
    }

    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
        encoder.set_color(png_color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| {
            Error::InvalidInput(format!("could not encode TIFF PNG header: {error}"))
        })?;
        writer.write_image_data(&output).map_err(|error| {
            Error::InvalidInput(format!("could not encode TIFF PNG data: {error}"))
        })?;
        writer.finish().map_err(|error| {
            Error::InvalidInput(format!("could not finish TIFF PNG image: {error}"))
        })?;
    }
    if png_bytes.len().saturating_mul(4).div_ceil(3) > MAX_TIFF_DATA_URI_BYTES {
        return Err(Error::LimitExceeded(format!(
            "TIFF PNG data URI would exceed {MAX_TIFF_DATA_URI_BYTES} bytes"
        )));
    }
    Ok(png_bytes)
}

#[allow(clippy::too_many_arguments)]
fn read_tiff_sample(
    decoded: &DecodingResult,
    layout: &tiff::decoder::BufferLayoutPreference,
    x: usize,
    y: usize,
    channel: usize,
    channels: u8,
    bits: u8,
    sample_bytes: usize,
    row_stride: usize,
    plane_stride: usize,
    packed: bool,
) -> Result<u8> {
    let plane = if layout.planes > 1 { channel } else { 0 };
    let plane_start = plane
        .checked_mul(plane_stride)
        .ok_or_else(|| Error::LimitExceeded("TIFF plane offset overflowed".into()))?;
    let row_start = plane_start
        .checked_add(
            y.checked_mul(row_stride)
                .ok_or_else(|| Error::LimitExceeded("TIFF row offset overflowed".into()))?,
        )
        .ok_or_else(|| Error::LimitExceeded("TIFF row offset overflowed".into()))?;
    if packed {
        let bit_offset = x
            .checked_mul(usize::from(bits))
            .ok_or_else(|| Error::LimitExceeded("TIFF packed sample offset overflowed".into()))?;
        let byte_offset = row_start
            .checked_add(bit_offset / 8)
            .ok_or_else(|| Error::LimitExceeded("TIFF packed sample offset overflowed".into()))?;
        let shift = 8 - bits - (bit_offset % 8) as u8;
        let maximum = (1u16 << bits) - 1;
        let value = match decoded {
            DecodingResult::U8(data) => *data
                .get(byte_offset)
                .ok_or_else(|| Error::InvalidInput("TIFF sample buffer ended early".into()))?,
            _ => {
                return Err(Error::Unsupported(
                    "packed TIFF samples were not decoded as 8-bit values".into(),
                ));
            }
        };
        let sample = (u16::from(value) >> shift) & maximum;
        return Ok(((u32::from(sample) * 255 + u32::from(maximum) / 2) / u32::from(maximum)) as u8);
    }

    let pixel_samples = if layout.planes > 1 {
        x
    } else {
        x * usize::from(channels) + channel
    };
    let offset = row_start
        .checked_add(
            pixel_samples
                .checked_mul(sample_bytes)
                .ok_or_else(|| Error::LimitExceeded("TIFF sample offset overflowed".into()))?,
        )
        .ok_or_else(|| Error::LimitExceeded("TIFF sample offset overflowed".into()))?;
    match (decoded, sample_bytes) {
        (DecodingResult::U8(data), 1) => data
            .get(offset)
            .copied()
            .ok_or_else(|| Error::InvalidInput("TIFF sample buffer ended early".into())),
        (DecodingResult::U16(data), 2) => {
            if offset % 2 != 0 {
                return Err(Error::InvalidInput(
                    "TIFF 16-bit sample is misaligned".into(),
                ));
            }
            let sample = *data
                .get(offset / 2)
                .ok_or_else(|| Error::InvalidInput("TIFF sample buffer ended early".into()))?;
            Ok(((u32::from(sample) * 255 + 32767) / 65535) as u8)
        }
        (DecodingResult::U32(data), 4) => {
            if offset % 4 != 0 {
                return Err(Error::InvalidInput(
                    "TIFF 32-bit sample is misaligned".into(),
                ));
            }
            let sample = *data
                .get(offset / 4)
                .ok_or_else(|| Error::InvalidInput("TIFF sample buffer ended early".into()))?;
            Ok(((u64::from(sample) * 255 + u64::from(u32::MAX) / 2) / u64::from(u32::MAX)) as u8)
        }
        (DecodingResult::U64(data), 8) => {
            if offset % 8 != 0 {
                return Err(Error::InvalidInput(
                    "TIFF 64-bit sample is misaligned".into(),
                ));
            }
            let sample = *data
                .get(offset / 8)
                .ok_or_else(|| Error::InvalidInput("TIFF sample buffer ended early".into()))?;
            Ok(
                ((u128::from(sample) * 255 + u128::from(u64::MAX) / 2) / u128::from(u64::MAX))
                    as u8,
            )
        }
        _ => Err(Error::Unsupported(format!(
            "TIFF {bits}-bit integer samples use an unsupported decoder buffer type"
        ))),
    }
}

fn decoded_buffer_len(decoded: &DecodingResult) -> usize {
    match decoded {
        DecodingResult::U8(values) => values.len(),
        DecodingResult::U16(values) => values.len().saturating_mul(2),
        DecodingResult::U32(values) => values.len().saturating_mul(4),
        DecodingResult::U64(values) => values.len().saturating_mul(8),
        DecodingResult::I8(values) => values.len(),
        DecodingResult::I16(values) => values.len().saturating_mul(2),
        DecodingResult::I32(values) => values.len().saturating_mul(4),
        DecodingResult::I64(values) => values.len().saturating_mul(8),
        DecodingResult::F16(values) => values.len().saturating_mul(2),
        DecodingResult::F32(values) => values.len().saturating_mul(4),
        DecodingResult::F64(values) => values.len().saturating_mul(8),
    }
}

fn cmyk_to_rgb(cyan: u8, magenta: u8, yellow: u8, black: u8) -> [u8; 3] {
    let convert =
        |ink: u8| ((u16::from(u8::MAX - ink) * u16::from(u8::MAX - black) + 127) / 255) as u8;
    [convert(cyan), convert(magenta), convert(yellow)]
}

fn page_resolution<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> (f64, f64) {
    let unit = decoder
        .find_tag_unsigned::<u16>(Tag::ResolutionUnit)
        .ok()
        .flatten()
        .unwrap_or(2);
    let multiplier = match unit {
        2 => 1.0,
        3 => 2.54,
        _ => return (72.0, 72.0),
    };
    let read_dpi = |decoder: &mut Decoder<R>, tag| {
        decoder
            .get_tag_f64(tag)
            .ok()
            .filter(|resolution| resolution.is_finite() && *resolution > 0.0)
            .map(|resolution| resolution * multiplier)
            .filter(|dpi| (1.0..=100_000.0).contains(dpi))
            .unwrap_or(72.0)
    };
    (
        read_dpi(decoder, Tag::XResolution),
        read_dpi(decoder, Tag::YResolution),
    )
}

fn orientation_transform(
    orientation: u16,
    width: u32,
    height: u32,
    scale_x: f64,
    scale_y: f64,
) -> [f64; 6] {
    let w = f64::from(width);
    let h = f64::from(height);
    let [a, b, c, d, e, f] = match orientation {
        2 => [-1.0, 0.0, 0.0, 1.0, w, 0.0],
        3 => [-1.0, 0.0, 0.0, -1.0, w, h],
        4 => [1.0, 0.0, 0.0, -1.0, 0.0, h],
        5 => [0.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        6 => [0.0, 1.0, -1.0, 0.0, h, 0.0],
        7 => [0.0, -1.0, -1.0, 0.0, h, w],
        8 => [0.0, -1.0, 1.0, 0.0, 0.0, w],
        _ => IDENTITY,
    };
    [
        scale_x * a,
        scale_y * b,
        scale_x * c,
        scale_y * d,
        scale_x * e,
        scale_y * f,
    ]
}

fn map_tiff_error(error: tiff::TiffError, context: &str) -> Error {
    match error {
        tiff::TiffError::LimitsExceeded | tiff::TiffError::IntSizeError => {
            Error::LimitExceeded(format!("{context}: decoder limits exceeded"))
        }
        error @ tiff::TiffError::UnsupportedError(_) => {
            Error::Unsupported(format!("{context}: {error}"))
        }
        error => Error::InvalidInput(format!("{context}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::orientation_transform;

    #[test]
    fn all_tiff_orientations_map_to_the_oriented_page_bounds() {
        let width = 13.0;
        let height = 7.0;
        let corners = [(0.0, 0.0), (width, 0.0), (0.0, height), (width, height)];
        for orientation in 1..=8 {
            let [a, b, c, d, e, f] = orientation_transform(orientation, 13, 7, 1.0, 1.0);
            let mapped = corners.map(|(x, y)| (a * x + c * y + e, b * x + d * y + f));
            let min_x = mapped
                .iter()
                .map(|point| point.0)
                .fold(f64::INFINITY, f64::min);
            let max_x = mapped
                .iter()
                .map(|point| point.0)
                .fold(f64::NEG_INFINITY, f64::max);
            let min_y = mapped
                .iter()
                .map(|point| point.1)
                .fold(f64::INFINITY, f64::min);
            let max_y = mapped
                .iter()
                .map(|point| point.1)
                .fold(f64::NEG_INFINITY, f64::max);
            let (expected_width, expected_height) = if orientation >= 5 {
                (height, width)
            } else {
                (width, height)
            };
            assert_eq!(
                (min_x, max_x, min_y, max_y),
                (0.0, expected_width, 0.0, expected_height)
            );
        }
    }
}
