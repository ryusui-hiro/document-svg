//! Bounded Comic Book Archive (CBZ) reader.
//!
//! CBZ is a ZIP archive convention rather than a standalone page format. This
//! importer orders image members by a case-insensitive natural filename sort
//! and emits one SVG page for each embedded PNG or JPEG. It reads entries in
//! memory and never extracts archive paths to the filesystem.

use std::cmp::Ordering;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, SourceMeta};
use crate::ooxml::sniff_image_mime;

const MAX_CBZ_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_CBZ_IMAGE_NAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_CBZ_PAGE_PIXELS: u64 = 40_000_000;
const MAX_CBZ_TOTAL_PIXELS: u64 = 100_000_000;
const MAX_CBZ_PAGE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_CBZ_TOTAL_IMAGE_BYTES: u64 = 384 * 1024 * 1024;
const MAX_CBZ_TOTAL_DATA_URI_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CBZ_PAGE_DIMENSION: f64 = 1_000_000.0;

struct ImageEntry {
    index: usize,
    name: String,
    size: u64,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut archive = ZipArchive::new(BufReader::new(File::open(path)?))?;
    if archive.len() > MAX_CBZ_ARCHIVE_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "CBZ archive contains {} entries; maximum is {MAX_CBZ_ARCHIVE_ENTRIES}",
            archive.len()
        )));
    }
    let mut image_entries = Vec::new();
    let mut total_image_name_bytes = 0usize;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.is_dir() || entry.is_symlink() || !is_comic_image_filename(entry.name()) {
            continue;
        }
        total_image_name_bytes = total_image_name_bytes
            .checked_add(entry.name().len())
            .ok_or_else(|| Error::LimitExceeded("CBZ image name size overflowed".into()))?;
        if total_image_name_bytes > MAX_CBZ_IMAGE_NAME_BYTES {
            return Err(Error::LimitExceeded(format!(
                "CBZ image member names exceed {MAX_CBZ_IMAGE_NAME_BYTES} bytes"
            )));
        }
        image_entries.push(ImageEntry {
            index,
            name: entry.name().to_owned(),
            size: entry.size(),
        });
        if image_entries.len() > options.max_pages {
            return Err(Error::LimitExceeded(format!(
                "CBZ contains more than {} image members",
                options.max_pages
            )));
        }
    }
    image_entries.sort_by(|left, right| {
        natural_cmp(&left.name, &right.name).then_with(|| left.name.cmp(&right.name))
    });
    if image_entries.is_empty() {
        return Err(Error::InvalidInput(
            "CBZ archive contains no PNG or JPEG page images".into(),
        ));
    }

    let page_limit = options.max_zip_entry_bytes.min(MAX_CBZ_PAGE_BYTES);
    let total_input_limit = options.max_input_bytes.min(MAX_CBZ_TOTAL_IMAGE_BYTES);
    let mut total_image_bytes = 0u64;
    let mut total_data_uri_bytes = 0u64;
    let mut total_pixels = 0u64;
    let mut unsupported_image_count = 0usize;
    let mut invalid_image_count = 0usize;
    let mut page_number = 0usize;
    for image_entry in image_entries {
        if image_entry.size > page_limit {
            return Err(Error::LimitExceeded(format!(
                "CBZ image '{}' is {} bytes; maximum is {page_limit}",
                image_entry.name, image_entry.size
            )));
        }
        let mut entry = archive.by_index(image_entry.index)?;
        let capacity = usize::try_from(image_entry.size).map_err(|_| {
            Error::LimitExceeded(format!(
                "CBZ image '{}' does not fit in memory",
                image_entry.name
            ))
        })?;
        let mut bytes = Vec::with_capacity(capacity.min(8 * 1024 * 1024));
        entry
            .by_ref()
            .take(page_limit.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > page_limit {
            return Err(Error::LimitExceeded(format!(
                "CBZ image '{}' expanded beyond {page_limit} bytes",
                image_entry.name
            )));
        }
        total_image_bytes = total_image_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::LimitExceeded("CBZ image byte count overflowed".into()))?;
        if total_image_bytes > total_input_limit {
            return Err(Error::LimitExceeded(format!(
                "CBZ image data exceeds {total_input_limit} bytes"
            )));
        }

        let mime = match sniff_image_mime(&bytes) {
            Some("image/png") => "image/png",
            Some("image/jpeg") => "image/jpeg",
            Some(_) => {
                unsupported_image_count += 1;
                continue;
            }
            None => {
                invalid_image_count += 1;
                continue;
            }
        };
        let (width, height) = match image_dimensions(&bytes, mime) {
            Ok(dimensions) => dimensions,
            Err(_) => {
                invalid_image_count += 1;
                continue;
            }
        };
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| Error::LimitExceeded("CBZ image pixel count overflowed".into()))?;
        if width == 0 || height == 0 || pixels > MAX_CBZ_PAGE_PIXELS {
            return Err(Error::LimitExceeded(format!(
                "CBZ image '{}' is {width}x{height}; maximum is {MAX_CBZ_PAGE_PIXELS} pixels",
                image_entry.name
            )));
        }
        total_pixels = total_pixels
            .checked_add(pixels)
            .ok_or_else(|| Error::LimitExceeded("CBZ total pixel count overflowed".into()))?;
        if total_pixels > MAX_CBZ_TOTAL_PIXELS {
            return Err(Error::LimitExceeded(format!(
                "CBZ images exceed the {MAX_CBZ_TOTAL_PIXELS}-pixel total limit"
            )));
        }
        let data_uri = format!("data:{mime};base64,{}", BASE64_STANDARD.encode(bytes));
        total_data_uri_bytes = total_data_uri_bytes
            .checked_add(data_uri.len() as u64)
            .ok_or_else(|| Error::LimitExceeded("CBZ data URI byte count overflowed".into()))?;
        if total_data_uri_bytes > MAX_CBZ_TOTAL_DATA_URI_BYTES {
            return Err(Error::LimitExceeded(format!(
                "CBZ embedded image data exceeds {MAX_CBZ_TOTAL_DATA_URI_BYTES} bytes"
            )));
        }
        let width_points = f64::from(width);
        let height_points = f64::from(height);
        if width_points > MAX_CBZ_PAGE_DIMENSION || height_points > MAX_CBZ_PAGE_DIMENSION {
            return Err(Error::LimitExceeded(format!(
                "CBZ image '{}' page dimensions exceed the supported range",
                image_entry.name
            )));
        }

        page_number += 1;
        let mut page = Page::new(page_number, width_points, height_points, "cbz");
        page.title = format!("Comic page {page_number}");
        page.description = format!("CBZ image page {page_number}");
        page.nodes.push(Node::Image {
            id: format!("cbz-image-{page_number}"),
            href: data_uri,
            x: 0.0,
            y: 0.0,
            width: width_points,
            height: height_points,
            transform: IDENTITY,
            opacity: 1.0,
            clip_id: None,
            meta: SourceMeta {
                semantic_role: "cbz:image".into(),
                alt_text: format!("Comic page {page_number}"),
                ..Default::default()
            },
        });
        sink.consume(page)?;
    }

    if page_number == 0 {
        return Err(Error::InvalidInput(
            "CBZ archive has no valid PNG or JPEG pages; unsupported or malformed image entries may have been present".into(),
        ));
    }
    let mut warnings = Vec::new();
    if unsupported_image_count > 0 {
        warnings.push(format!(
            "CBZ omitted {unsupported_image_count} image member(s) with formats other than PNG/JPEG"
        ));
    }
    if invalid_image_count > 0 {
        warnings.push(format!(
            "CBZ omitted {invalid_image_count} malformed or uninspectable image member(s)"
        ));
    }
    Ok(warnings)
}

fn is_comic_image_filename(name: &str) -> bool {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    extension.is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff"
        )
    })
}

fn image_dimensions(bytes: &[u8], mime: &str) -> Result<(u32, u32)> {
    match mime {
        "image/png" => {
            let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
            let reader = decoder
                .read_info()
                .map_err(|error| Error::InvalidInput(format!("invalid PNG header: {error}")))?;
            Ok((reader.info().width, reader.info().height))
        }
        "image/jpeg" => {
            let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
            decoder
                .read_info()
                .map_err(|error| Error::InvalidInput(format!("invalid JPEG header: {error}")))?;
            let info = decoder
                .info()
                .ok_or_else(|| Error::InvalidInput("JPEG image metadata is missing".into()))?;
            Ok((u32::from(info.width), u32::from(info.height)))
        }
        other => Err(Error::Unsupported(format!(
            "unsupported CBZ image MIME type '{other}'"
        ))),
    }
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left_bytes = left.as_bytes();
    let right_bytes = right.as_bytes();
    let (mut left_index, mut right_index) = (0usize, 0usize);
    while left_index < left_bytes.len() && right_index < right_bytes.len() {
        if left_bytes[left_index].is_ascii_digit() && right_bytes[right_index].is_ascii_digit() {
            let left_start = left_index;
            let right_start = right_index;
            while left_index < left_bytes.len() && left_bytes[left_index].is_ascii_digit() {
                left_index += 1;
            }
            while right_index < right_bytes.len() && right_bytes[right_index].is_ascii_digit() {
                right_index += 1;
            }
            let left_significant = significant_digits(&left_bytes[left_start..left_index]);
            let right_significant = significant_digits(&right_bytes[right_start..right_index]);
            let numeric_order = left_significant
                .len()
                .cmp(&right_significant.len())
                .then_with(|| left_significant.cmp(right_significant));
            if numeric_order != Ordering::Equal {
                return numeric_order;
            }
            let leading_zero_order = (left_index - left_start).cmp(&(right_index - right_start));
            if leading_zero_order != Ordering::Equal {
                return leading_zero_order;
            }
            continue;
        }
        let character_order = left_bytes[left_index]
            .to_ascii_lowercase()
            .cmp(&right_bytes[right_index].to_ascii_lowercase());
        if character_order != Ordering::Equal {
            return character_order;
        }
        left_index += 1;
        right_index += 1;
    }
    left_bytes
        .len()
        .cmp(&right_bytes.len())
        .then_with(|| left.cmp(right))
}

fn significant_digits(digits: &[u8]) -> &[u8] {
    let first_nonzero = digits.iter().position(|digit| *digit != b'0');
    first_nonzero.map_or_else(
        || &digits[digits.len().saturating_sub(1)..],
        |index| &digits[index..],
    )
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering;

    #[test]
    fn sorts_numeric_page_names_in_natural_order() {
        let mut names = ["page10.png", "page2.png", "Page 1.png", "page01.png"];
        names.sort_by(|left, right| natural_cmp(left, right));
        assert_eq!(
            names,
            ["Page 1.png", "page01.png", "page2.png", "page10.png"]
        );
        assert_eq!(natural_cmp("2.jpg", "10.jpg"), Ordering::Less);
    }
}
