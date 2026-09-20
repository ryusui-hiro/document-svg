//! Bounded standalone JPEG 2000 / JP2 image conversion.
//!
//! PDF and DICOM already use the shared JPEG 2000 header validator. This
//! adapter exposes the same safe subset as a normal raster document: one
//! unsigned grayscale, gray+alpha, RGB, or RGB+alpha image per SVG page.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use jpeg2k::{ColorSpace, Image};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, SourceMeta};

const MAX_JPEG2000_BYTES: u64 = 128 * 1024 * 1024;
const MAX_JPEG2000_PIXELS: u64 = 20_000_000;
const MAX_JPEG2000_DIMENSION: u32 = 100_000;
const MAX_JPEG2000_DECODED_BYTES: usize = 128 * 1024 * 1024;
const MAX_JPEG2000_DATA_URI_BYTES: usize = 192 * 1024 * 1024;

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_JPEG2000_BYTES),
        "JPEG 2000 input",
    )?;
    let (png, width, height, mut warnings) = decode_png(&bytes)?;
    let data_uri = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png));
    if data_uri.len() > MAX_JPEG2000_DATA_URI_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JPEG 2000 PNG data URI exceeds {MAX_JPEG2000_DATA_URI_BYTES} bytes"
        )));
    }
    let mut page = Page::new(1, f64::from(width), f64::from(height), "jpeg2000");
    page.title = "JPEG 2000 image".into();
    page.description = format!("JPEG 2000 image {width}×{height}");
    for warning in &warnings {
        page.warn(warning.clone());
    }
    page.nodes.push(Node::Image {
        id: "jpeg2000-image-1".into(),
        href: data_uri,
        x: 0.0,
        y: 0.0,
        width: f64::from(width),
        height: f64::from(height),
        transform: IDENTITY,
        opacity: 1.0,
        clip_id: None,
        meta: SourceMeta {
            semantic_role: "jpeg2000:image".into(),
            ..Default::default()
        },
    });
    sink.consume(page)?;
    warnings.sort();
    warnings.dedup();
    Ok(warnings)
}

fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32, Vec<String>)> {
    let header = crate::jpeg2000::parse_jpx_header(bytes)?;
    if header.width == 0
        || header.height == 0
        || header.width > MAX_JPEG2000_DIMENSION
        || header.height > MAX_JPEG2000_DIMENSION
    {
        return Err(Error::LimitExceeded(format!(
            "JPEG 2000 dimensions {}×{} exceed the supported range",
            header.width, header.height
        )));
    }
    let pixels = u64::from(header.width)
        .checked_mul(u64::from(header.height))
        .ok_or_else(|| Error::LimitExceeded("JPEG 2000 pixel count overflowed".into()))?;
    if pixels > MAX_JPEG2000_PIXELS {
        return Err(Error::LimitExceeded(format!(
            "JPEG 2000 image contains {pixels} pixels; maximum is {MAX_JPEG2000_PIXELS}"
        )));
    }
    let decoded_sample_bytes = usize::try_from(pixels)
        .ok()
        .and_then(|count| count.checked_mul(header.components.len()))
        .and_then(|count| count.checked_mul(std::mem::size_of::<i32>()))
        .ok_or_else(|| Error::LimitExceeded("JPEG 2000 decoded sample size overflowed".into()))?;
    if decoded_sample_bytes > MAX_JPEG2000_DECODED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JPEG 2000 decoded samples need {decoded_sample_bytes} bytes; maximum is {MAX_JPEG2000_DECODED_BYTES}"
        )));
    }
    let image = Image::from_bytes(bytes)
        .map_err(|error| Error::InvalidInput(format!("JPEG 2000 decode failed: {error}")))?;
    if image.width() != header.width || image.height() != header.height {
        return Err(Error::InvalidInput(
            "JPEG 2000 decoder dimensions do not match the validated header".into(),
        ));
    }
    let components = image.components();
    if components.len() != header.components.len() {
        return Err(Error::InvalidInput(
            "JPEG 2000 decoder component count does not match the validated header".into(),
        ));
    }
    let precision = header.components[0].precision;
    if !(1..=16).contains(&precision)
        || header.components.iter().any(|component| {
            component.precision != precision
                || component.signed
                || component.width != header.width
                || component.height != header.height
        })
    {
        return Err(Error::Unsupported(
            "JPEG 2000 requires unsigned components with a shared 1–16-bit precision".into(),
        ));
    }
    let mut warnings = Vec::new();
    let color_space = image.color_space();
    let alpha_index = components
        .iter()
        .enumerate()
        .filter_map(|(index, component)| component.is_alpha().then_some(index))
        .collect::<Vec<_>>();
    let alpha = match alpha_index.as_slice() {
        [] => match components.len() {
            2 if matches!(
                color_space,
                ColorSpace::Gray | ColorSpace::Unknown | ColorSpace::Unspecified
            ) =>
            {
                warnings.push(
                    "JPEG 2000 two-component image was interpreted as grayscale plus alpha".into(),
                );
                Some(1)
            }
            4 if matches!(
                color_space,
                ColorSpace::SRGB | ColorSpace::Unknown | ColorSpace::Unspecified
            ) =>
            {
                warnings.push(
                    "JPEG 2000 four-component image was interpreted as RGB plus alpha".into(),
                );
                Some(3)
            }
            _ => None,
        },
        [index] => Some(*index),
        _ => {
            return Err(Error::Unsupported(
                "JPEG 2000 images with multiple alpha components are unsupported".into(),
            ));
        }
    };
    let color_indices = (0..components.len())
        .filter(|index| Some(*index) != alpha)
        .collect::<Vec<_>>();
    let png_color = match (color_indices.len(), alpha.is_some()) {
        (1, false) => png::ColorType::Grayscale,
        (1, true) => png::ColorType::GrayscaleAlpha,
        (3, false) => png::ColorType::Rgb,
        (3, true) => png::ColorType::Rgba,
        _ => {
            return Err(Error::Unsupported(format!(
                "JPEG 2000 component count {} is unsupported; expected grayscale or RGB with optional alpha",
                components.len()
            )));
        }
    };
    let output_channels = png_color.samples();
    let output_len = usize::try_from(pixels)
        .ok()
        .and_then(|count| count.checked_mul(output_channels))
        .ok_or_else(|| Error::LimitExceeded("JPEG 2000 PNG size overflowed".into()))?;
    if output_len > MAX_JPEG2000_DECODED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JPEG 2000 PNG samples need {output_len} bytes; maximum is {MAX_JPEG2000_DECODED_BYTES}"
        )));
    }
    let samples = components
        .iter()
        .map(|component| component.data())
        .collect::<Vec<_>>();
    let maximum = (1u32 << precision) - 1;
    let mut output = Vec::with_capacity(output_len);
    for pixel in 0..usize::try_from(pixels).unwrap_or(0) {
        for index in &color_indices {
            output.push(scale_sample(samples[*index].get(pixel).copied(), maximum)?);
        }
        if let Some(alpha_index) = alpha {
            output.push(scale_sample(
                samples[alpha_index].get(pixel).copied(),
                maximum,
            )?);
        }
    }
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, header.width, header.height);
        encoder.set_color(png_color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(|error| {
            Error::InvalidInput(format!("could not encode JPEG 2000 PNG header: {error}"))
        })?;
        writer.write_image_data(&output).map_err(|error| {
            Error::InvalidInput(format!("could not encode JPEG 2000 PNG data: {error}"))
        })?;
        writer.finish().map_err(|error| {
            Error::InvalidInput(format!("could not finish JPEG 2000 PNG image: {error}"))
        })?;
    }
    if precision != 8 {
        warnings.push(format!(
            "JPEG 2000 {precision}-bit samples were reduced to 8-bit PNG"
        ));
    }
    if !matches!(
        color_space,
        ColorSpace::Gray | ColorSpace::SRGB | ColorSpace::Unknown | ColorSpace::Unspecified
    ) {
        warnings.push("JPEG 2000 color space metadata was not applied".into());
    }
    Ok((png_bytes, header.width, header.height, warnings))
}

fn scale_sample(sample: Option<i32>, maximum: u32) -> Result<u8> {
    let sample = sample.ok_or_else(|| {
        Error::InvalidInput("JPEG 2000 decoder returned an incomplete component".into())
    })?;
    if sample < 0 || u32::try_from(sample).unwrap_or(u32::MAX) > maximum {
        return Err(Error::InvalidInput(
            "JPEG 2000 sample is outside its declared precision".into(),
        ));
    }
    Ok(
        ((u64::try_from(sample).unwrap_or(0) * 255 + u64::from(maximum) / 2) / u64::from(maximum))
            as u8,
    )
}
