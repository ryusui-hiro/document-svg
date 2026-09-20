//! EPUB electronic book parser and paginated vector SVG renderer.
//!
//! Inspects the EPUB package (`META-INF/container.xml`), follows the OPF manifest & spine,
//! typesets each chapter's XHTML content and bounded package-linked PNG/JPEG images,
//! and paginates into sequential vector SVG pages.

#![allow(clippy::collapsible_if)]

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer};
use crate::document::html::{
    HtmlBlock, InlineHtmlImage, collect_html_image_sources_with_limit,
    parse_html_blocks_with_inline_images, render_blocks_to_pages,
};
use crate::error::{Error, Result};
use crate::ooxml::{ZipPackage, attribute, local_name};

const MAX_EPUB_CONTAINER_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EPUB_OPF_BYTES: u64 = 32 * 1024 * 1024;
const MAX_EPUB_SPINE_ITEMS: usize = 100_000;
const MAX_EPUB_IMAGE_REFERENCES: usize = 10_000;
const MAX_EPUB_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EPUB_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_EPUB_TOTAL_DATA_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_EPUB_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_EPUB_TOTAL_IMAGE_PIXELS: u64 = 100_000_000;

#[derive(Default)]
struct EpubImageBudget {
    references: usize,
    loaded_image_bytes: usize,
    data_uri_bytes: usize,
    instance_pixels: u64,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut archive = ZipPackage::open(path, options.max_zip_entry_bytes)
        .map_err(|error| Error::InvalidInput(format!("failed to open EPUB archive: {error}")))?;

    // 1. Read container.xml to locate OPF package
    let opf_path = read_container_rootfile(&mut archive, options.max_xml_events)?;

    // 2. Read OPF file to extract spine chapter order
    let (manifest, spine) = read_opf_spine(&mut archive, &opf_path, options)?;

    let opf_dir = opf_path.rsplit_once('/').map_or("", |(parent, _)| parent);

    // 3. Read chapters in spine order and render
    let mut all_blocks = Vec::new();
    let total_chapter_limit = options.max_zip_entry_bytes.min(options.max_input_bytes);
    let mut total_chapter_bytes = 0u64;
    let mut warnings = Vec::new();
    let mut image_budget = EpubImageBudget::default();
    let normalized_chapter_limit = usize::try_from(total_chapter_limit)
        .unwrap_or(usize::MAX)
        .min(512 * 1024 * 1024);
    for idref in spine {
        let Some(href) = manifest.get(&idref) else {
            warnings.push(format!(
                "EPUB spine item '{idref}' has no manifest entry and was skipped"
            ));
            continue;
        };
        let chapter_path = match resolve_package_href(opf_dir, href) {
            Ok(path) => path,
            Err(message) => {
                warnings.push(format!(
                    "EPUB spine resource '{href}' was skipped: {message}"
                ));
                continue;
            }
        };
        let chapter_bytes = match archive.read(&chapter_path) {
            Ok(bytes) => bytes,
            Err(Error::Zip(zip::result::ZipError::FileNotFound)) => {
                warnings.push(format!(
                    "EPUB spine resource '{chapter_path}' is missing from the package"
                ));
                continue;
            }
            Err(error @ Error::LimitExceeded(_)) => return Err(error),
            Err(error) => {
                warnings.push(format!(
                    "EPUB spine resource '{chapter_path}' could not be read: {error}"
                ));
                continue;
            }
        };
        total_chapter_bytes = total_chapter_bytes
            .checked_add(chapter_bytes.len() as u64)
            .ok_or_else(|| Error::LimitExceeded("EPUB chapter byte count overflowed".into()))?;
        if total_chapter_bytes > total_chapter_limit {
            return Err(Error::LimitExceeded(format!(
                "EPUB spine content exceeds the {total_chapter_limit}-byte total read limit"
            )));
        }
        let chapter_xml = match String::from_utf8(chapter_bytes) {
            Ok(xml) => xml,
            Err(error) => {
                warnings.push(format!(
                    "EPUB spine resource '{chapter_path}' is not valid UTF-8: {error}"
                ));
                continue;
            }
        };
        let remaining_image_references =
            MAX_EPUB_IMAGE_REFERENCES.saturating_sub(image_budget.references);
        let (image_sources, omitted_image_sources) = match collect_html_image_sources_with_limit(
            &chapter_xml,
            options.max_xml_events,
            remaining_image_references,
        ) {
            Ok(result) => result,
            Err(Error::LimitExceeded(message)) => {
                return Err(Error::LimitExceeded(format!(
                    "EPUB spine resource '{chapter_path}': {message}"
                )));
            }
            Err(error) => {
                warnings.push(format!(
                    "EPUB spine resource '{chapter_path}' image references could not be scanned: {error}"
                ));
                (Vec::new(), false)
            }
        };
        image_budget.references = image_budget.references.saturating_add(image_sources.len());
        let mut inline_images = HashMap::new();
        let mut seen_image_sources = HashSet::new();
        for source in image_sources {
            if !seen_image_sources.insert(source.clone()) {
                if let Some(image) = inline_images.get(&source) {
                    reserve_epub_image_instance(image, &mut image_budget, &mut warnings);
                }
                continue;
            }
            if let Some(image) = load_epub_image(
                &mut archive,
                &chapter_path,
                &source,
                &mut image_budget,
                &mut warnings,
            )? {
                inline_images.insert(source, image);
            }
        }
        if omitted_image_sources {
            push_epub_warning_once(
                &mut warnings,
                "EPUB image references exceeded the supported limit; remaining images were omitted",
            );
            image_budget.references = MAX_EPUB_IMAGE_REFERENCES;
        }

        match parse_html_blocks_with_inline_images(
            &chapter_xml,
            options.max_xml_events,
            normalized_chapter_limit,
            &inline_images,
        ) {
            Ok((blocks, html_warnings, _used_images)) => {
                for warning in html_warnings {
                    push_epub_warning_once(&mut warnings, &warning);
                }
                if !all_blocks.is_empty() && !blocks.is_empty() {
                    all_blocks.push(HtmlBlock::PageBreak);
                }
                all_blocks.extend(blocks);
            }
            Err(Error::LimitExceeded(message)) => {
                return Err(Error::LimitExceeded(format!(
                    "EPUB spine resource '{chapter_path}': {message}"
                )));
            }
            Err(error) => warnings.push(format!(
                "EPUB spine resource '{chapter_path}' could not be parsed: {error}"
            )),
        }
    }

    if all_blocks.is_empty() {
        return Err(Error::InvalidInput(
            "no chapter text found in EPUB spine".into(),
        ));
    }

    render_blocks_to_pages(&all_blocks, sink, options)?;
    Ok(warnings)
}

fn load_epub_image(
    archive: &mut ZipPackage<File>,
    chapter_path: &str,
    source: &str,
    budget: &mut EpubImageBudget,
    warnings: &mut Vec<String>,
) -> Result<Option<InlineHtmlImage>> {
    let chapter_dir = chapter_path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let target = match resolve_package_href(chapter_dir, source) {
        Ok(target) => target,
        Err(message) => {
            let warning = if message.contains("remote resources") {
                "remote EPUB image resources are not fetched"
            } else {
                "EPUB image paths escaping the package or using unsupported URI schemes were omitted"
            };
            push_epub_warning_once(warnings, warning);
            return Ok(None);
        }
    };
    let bytes = match archive.read_optional_limited(&target, MAX_EPUB_IMAGE_BYTES) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            push_epub_warning_once(warnings, "missing EPUB image parts were omitted");
            return Ok(None);
        }
        Err(Error::LimitExceeded(_)) => {
            push_epub_warning_once(
                warnings,
                "EPUB image parts exceeding the per-image byte limit were omitted",
            );
            return Ok(None);
        }
        Err(error) => {
            push_epub_warning_once(
                warnings,
                &format!("EPUB image parts could not be read: {error}"),
            );
            return Ok(None);
        }
    };
    let Some(mime) = crate::ooxml::sniff_image_mime(&bytes)
        .filter(|mime| matches!(*mime, "image/png" | "image/jpeg"))
    else {
        push_epub_warning_once(
            warnings,
            "unsupported EPUB image types were omitted; only PNG and JPEG are embedded",
        );
        return Ok(None);
    };
    let Some((width, height)) = crate::document::mhtml::image_dimensions(&bytes, mime) else {
        push_epub_warning_once(warnings, "invalid EPUB PNG/JPEG images were omitted");
        return Ok(None);
    };
    let pixels = u64::from(width) * u64::from(height);
    if width == 0 || height == 0 || pixels > MAX_EPUB_IMAGE_PIXELS {
        push_epub_warning_once(
            warnings,
            "EPUB images exceeding the per-image pixel limit were omitted",
        );
        return Ok(None);
    }
    let next_loaded_bytes = budget.loaded_image_bytes.saturating_add(bytes.len());
    if next_loaded_bytes > MAX_EPUB_TOTAL_IMAGE_BYTES {
        push_epub_warning_once(
            warnings,
            "EPUB images exceeding the total image byte limit were omitted",
        );
        return Ok(None);
    }
    let prefix = format!("data:{mime};base64,");
    let uri_bytes = prefix
        .len()
        .saturating_add(bytes.len().div_ceil(3).saturating_mul(4));
    if !reserve_epub_image_dimensions(width, height, uri_bytes, budget, warnings) {
        return Ok(None);
    }
    budget.loaded_image_bytes = next_loaded_bytes;
    Ok(Some(InlineHtmlImage {
        href: format!("{prefix}{}", BASE64_STANDARD.encode(&bytes)),
        pixel_width: width,
        pixel_height: height,
    }))
}

fn reserve_epub_image_instance(
    image: &InlineHtmlImage,
    budget: &mut EpubImageBudget,
    warnings: &mut Vec<String>,
) -> bool {
    reserve_epub_image_dimensions(
        image.pixel_width,
        image.pixel_height,
        image.href.len(),
        budget,
        warnings,
    )
}

fn reserve_epub_image_dimensions(
    width: u32,
    height: u32,
    uri_bytes: usize,
    budget: &mut EpubImageBudget,
    warnings: &mut Vec<String>,
) -> bool {
    let pixels = u64::from(width) * u64::from(height);
    let next_pixels = budget.instance_pixels.saturating_add(pixels);
    if next_pixels > MAX_EPUB_TOTAL_IMAGE_PIXELS {
        push_epub_warning_once(
            warnings,
            "EPUB images exceeding the total decoded pixel limit were omitted",
        );
        return false;
    }
    let next_uri_bytes = budget.data_uri_bytes.saturating_add(uri_bytes);
    if next_uri_bytes > MAX_EPUB_TOTAL_DATA_URI_BYTES {
        push_epub_warning_once(
            warnings,
            "EPUB images exceeding the total data URI byte limit were omitted",
        );
        return false;
    }
    budget.instance_pixels = next_pixels;
    budget.data_uri_bytes = next_uri_bytes;
    true
}

fn push_epub_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

fn read_container_rootfile(archive: &mut ZipPackage<File>, max_events: usize) -> Result<String> {
    let bytes = match archive.read_limited("META-INF/container.xml", MAX_EPUB_CONTAINER_BYTES) {
        Ok(bytes) => bytes,
        Err(Error::Zip(zip::result::ZipError::FileNotFound)) => {
            return Err(Error::InvalidInput(
                "missing META-INF/container.xml in EPUB".into(),
            ));
        }
        Err(error) => return Err(error),
    };
    let xml = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("EPUB container.xml is not UTF-8: {error}"))
    })?;
    let mut reader = Reader::from_reader(xml.as_bytes());
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut event_count = 0usize;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > max_events {
            return Err(Error::LimitExceeded(format!(
                "EPUB container.xml exceeds {max_events} parser events"
            )));
        }
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if name == b"rootfile"
                    && let Some(path) = attribute(e, b"full-path")
                {
                    return normalize_package_path(&path);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Err(Error::InvalidInput(
        "no rootfile declaration in EPUB container.xml".into(),
    ))
}

fn read_opf_spine(
    archive: &mut ZipPackage<File>,
    opf_path: &str,
    options: &ConvertOptions,
) -> Result<(HashMap<String, String>, Vec<String>)> {
    let bytes = match archive.read_limited(opf_path, MAX_EPUB_OPF_BYTES) {
        Ok(bytes) => bytes,
        Err(Error::Zip(zip::result::ZipError::FileNotFound)) => {
            return Err(Error::InvalidInput(format!(
                "EPUB package document '{opf_path}' is missing"
            )));
        }
        Err(error) => return Err(error),
    };
    let xml = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("EPUB package document is not UTF-8: {error}"))
    })?;
    let mut reader = Reader::from_reader(xml.as_bytes());
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut event_count = 0usize;

    loop {
        event_count = event_count.saturating_add(1);
        if event_count > options.max_xml_events {
            return Err(Error::LimitExceeded(format!(
                "EPUB package document exceeds {} parser events",
                options.max_xml_events
            )));
        }
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let qname = e.name();
                let name = local_name(qname.as_ref());
                if name == b"item" {
                    if let (Some(id), Some(href)) = (attribute(e, b"id"), attribute(e, b"href")) {
                        if manifest.len() >= MAX_EPUB_SPINE_ITEMS {
                            return Err(Error::LimitExceeded(format!(
                                "EPUB manifest exceeds {MAX_EPUB_SPINE_ITEMS} items"
                            )));
                        }
                        manifest.insert(id, href);
                    }
                } else if name == b"itemref"
                    && let Some(idref) = attribute(e, b"idref")
                {
                    if spine.len() >= MAX_EPUB_SPINE_ITEMS {
                        return Err(Error::LimitExceeded(format!(
                            "EPUB spine exceeds {MAX_EPUB_SPINE_ITEMS} items"
                        )));
                    }
                    spine.push(idref);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok((manifest, spine))
}

fn resolve_package_href(base_dir: &str, href: &str) -> std::result::Result<String, String> {
    if href.starts_with("//") || href.contains("://") {
        return Err("remote resources are not fetched".into());
    }
    let path = href
        .split(['?', '#'])
        .next()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "resource path is empty".to_owned())?;
    let joined = if base_dir.is_empty() || path.starts_with('/') {
        path.trim_start_matches('/').to_owned()
    } else {
        format!("{base_dir}/{path}")
    };
    normalize_package_path(&joined).map_err(|error| error.to_string())
}

fn normalize_package_path(path: &str) -> Result<String> {
    let mut components = Vec::new();
    let normalized_separators = path.replace('\\', "/");
    for component in normalized_separators.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.pop().is_none() {
                    return Err(Error::InvalidInput(
                        "EPUB resource path escapes the package root".into(),
                    ));
                }
            }
            value if value.contains(':') => {
                return Err(Error::InvalidInput(
                    "EPUB resource path must be package-relative".into(),
                ));
            }
            value => components.push(value),
        }
    }
    if components.is_empty() {
        return Err(Error::InvalidInput("EPUB resource path is empty".into()));
    }
    Ok(components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_spine_paths_without_leaving_the_package() {
        assert_eq!(
            resolve_package_href("OPS", "../Text/chapter.xhtml#start").unwrap(),
            "Text/chapter.xhtml"
        );
        assert!(resolve_package_href("OPS", "../../outside.xhtml").is_err());
        assert!(resolve_package_href("OPS", "https://example.com/ch.xhtml").is_err());
    }

    #[test]
    fn repeated_image_uses_share_a_bounded_uri_budget() {
        let image = InlineHtmlImage {
            href: "data:image/png;base64,AA==".into(),
            pixel_width: 10,
            pixel_height: 10,
        };
        let mut budget = EpubImageBudget {
            instance_pixels: MAX_EPUB_TOTAL_IMAGE_PIXELS - 50,
            ..EpubImageBudget::default()
        };
        let mut warnings = Vec::new();

        assert!(!reserve_epub_image_instance(
            &image,
            &mut budget,
            &mut warnings
        ));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("total decoded pixel limit"))
        );

        let mut uri_budget = EpubImageBudget::default();
        assert!(reserve_epub_image_dimensions(
            1,
            1,
            MAX_EPUB_TOTAL_DATA_URI_BYTES,
            &mut uri_budget,
            &mut warnings
        ));
        assert!(!reserve_epub_image_instance(
            &image,
            &mut uri_budget,
            &mut warnings
        ));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("total data URI byte limit"))
        );
    }
}
