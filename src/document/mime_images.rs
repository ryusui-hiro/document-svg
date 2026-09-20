//! Bounded shared image handling for local MIME Content-ID/Content-Location references.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use mail_parser::{HeaderName, Message, MimeHeaders, PartType};
use url::Url;

use crate::document::html::InlineHtmlImage;

const MAX_CID_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_CID_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_CID_URI_BYTES: usize = 48 * 1024 * 1024;
const MAX_CID_IMAGE_PIXELS: u64 = 40_000_000;
const MAX_TOTAL_CID_PIXELS: u64 = 100_000_000;
const MAX_MIME_URI_BYTES: usize = 8 * 1024;
const DEFAULT_MIME_BASE_URI: &str = "thismessage:/";

#[derive(Default)]
pub(crate) struct MimeImageResources {
    pub images: HashMap<String, InlineHtmlImage>,
    pub part_ids: HashMap<String, usize>,
    pub part_count: usize,
    pub warnings: Vec<String>,
    pub base_uri: Option<String>,
}

pub(crate) fn collect_mime_images(
    message: &Message<'_>,
    html_part_id: Option<u32>,
) -> MimeImageResources {
    collect_mime_images_impl(message, html_part_id, false)
}

pub(crate) fn collect_mime_images_with_uri_resolution(
    message: &Message<'_>,
    html_part_id: Option<u32>,
) -> MimeImageResources {
    collect_mime_images_impl(message, html_part_id, true)
}

fn collect_mime_images_impl(
    message: &Message<'_>,
    html_part_id: Option<u32>,
    resolve_locations: bool,
) -> MimeImageResources {
    let mut images = HashMap::new();
    let mut part_ids = HashMap::new();
    let mut part_count = 0usize;
    let mut warnings = Vec::new();
    let related_parts = html_part_id
        .map(|part_id| related_part_ids(message, part_id))
        .unwrap_or_default();
    let (parent_bases, effective_bases) = if resolve_locations {
        mime_part_bases(message, &mut warnings)
    } else {
        (Vec::new(), Vec::new())
    };
    let base_uri = resolve_locations.then(|| {
        html_part_id
            .and_then(|part_id| effective_bases.get(part_id as usize))
            .map(|base| base.as_str().to_owned())
            .unwrap_or_else(|| DEFAULT_MIME_BASE_URI.to_owned())
    });
    let mut total_bytes = 0usize;
    let mut total_uri_bytes = 0usize;
    let mut total_pixels = 0u64;

    for (part_id, part) in message.parts.iter().enumerate() {
        let Some(content_type) = part.content_type() else {
            continue;
        };
        if !content_type.ctype().eq_ignore_ascii_case("image") {
            continue;
        }
        let mut labels = HashSet::new();
        if let Some(content_id) = part.content_id() {
            labels.insert(normalize_content_id(content_id));
        }
        if let Some(location) = part.content_location() {
            let location = location.trim();
            // RFC 2557 requires CID URLs to match Content-ID only, never a
            // Content-Location value using the cid: scheme.
            if !location.is_empty()
                && !location
                    .get(..4)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("cid:"))
            {
                if related_parts.contains(&part_id) {
                    labels.insert(location.to_owned());
                    if resolve_locations && let Some(parent_base) = parent_bases.get(part_id) {
                        if let Some(resolved) = resolve_mime_uri(parent_base.as_str(), location) {
                            labels.insert(resolved);
                        } else {
                            push_warning_once(
                                &mut warnings,
                                "invalid or overlong MIME Content-Location URI was ignored",
                            );
                        }
                    }
                } else {
                    push_warning_once(
                        &mut warnings,
                        "MIME Content-Location image outside the HTML multipart/related scope was ignored",
                    );
                }
            }
        }
        if labels.is_empty() {
            continue;
        }
        let mime = match content_type
            .subtype()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png") => "image/png",
            Some("jpeg") | Some("jpg") => "image/jpeg",
            _ => {
                push_warning_once(&mut warnings, "unsupported MIME CID image type was omitted");
                continue;
            }
        };
        let bytes = part.contents();
        if bytes.len() > MAX_CID_IMAGE_BYTES {
            push_warning_once(
                &mut warnings,
                format!(
                    "MIME CID image exceeds the {MAX_CID_IMAGE_BYTES}-byte limit and was omitted"
                ),
            );
            continue;
        }
        if (mime == "image/png" && !bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
            || (mime == "image/jpeg" && !bytes.starts_with(&[0xff, 0xd8, 0xff]))
        {
            push_warning_once(
                &mut warnings,
                "MIME CID image bytes did not match their declared type",
            );
            continue;
        }
        let Some((width, height)) = image_dimensions(bytes, mime) else {
            push_warning_once(&mut warnings, "malformed MIME CID image was omitted");
            continue;
        };
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if width == 0
            || height == 0
            || pixels > MAX_CID_IMAGE_PIXELS
            || total_pixels.saturating_add(pixels) > MAX_TOTAL_CID_PIXELS
        {
            push_warning_once(
                &mut warnings,
                "MIME CID image exceeded the pixel limit and was omitted",
            );
            continue;
        }
        let encoded_size = bytes.len().div_ceil(3).saturating_mul(4);
        let uri_size = encoded_size.saturating_add(mime.len() + "data:;base64,".len());
        if total_bytes.saturating_add(bytes.len()) > MAX_TOTAL_CID_BYTES {
            push_warning_once(
                &mut warnings,
                "MIME Content-ID/Location images exceeded the total embedded image-byte budget",
            );
            continue;
        }
        let image = InlineHtmlImage {
            href: format!("data:{mime};base64,{}", BASE64_STANDARD.encode(bytes)),
            pixel_width: width,
            pixel_height: height,
        };
        let mut inserted_any_label = false;
        for key in labels {
            if images.contains_key(&key) {
                push_warning_once(
                    &mut warnings,
                    "duplicate MIME image reference label was omitted",
                );
                continue;
            }
            if total_uri_bytes.saturating_add(uri_size) > MAX_TOTAL_CID_URI_BYTES {
                push_warning_once(
                    &mut warnings,
                    "MIME Content-ID/Location images exceeded the total embedded data-URI budget",
                );
                continue;
            }
            images.insert(key.clone(), image.clone());
            part_ids.insert(key, part_id);
            total_uri_bytes += uri_size;
            inserted_any_label = true;
        }
        if inserted_any_label {
            total_bytes += bytes.len();
            total_pixels += pixels;
            part_count += 1;
        }
    }
    MimeImageResources {
        images,
        part_ids,
        part_count,
        warnings,
        base_uri,
    }
}

pub(crate) fn resolve_mime_uri(base: &str, reference: &str) -> Option<String> {
    let base = base.trim();
    let reference = reference.trim();
    if base.is_empty()
        || base.len() > MAX_MIME_URI_BYTES
        || reference.is_empty()
        || reference.len() > MAX_MIME_URI_BYTES
    {
        return None;
    }
    Url::parse(base)
        .ok()?
        .join(reference)
        .ok()
        .filter(|url| url.as_str().len() <= MAX_MIME_URI_BYTES)
        .map(|url| url.to_string())
}

fn mime_part_bases(
    message: &Message<'_>,
    warnings: &mut Vec<String>,
) -> (Vec<Arc<Url>>, Vec<Arc<Url>>) {
    let default =
        Arc::new(Url::parse(DEFAULT_MIME_BASE_URI).expect("static MIME base URI is valid"));
    let mut parent_bases = vec![Arc::clone(&default); message.parts.len()];
    let mut effective_bases = vec![Arc::clone(&default); message.parts.len()];
    if message.parts.is_empty() {
        return (parent_bases, effective_bases);
    }

    let mut pending = vec![(0usize, Arc::clone(&default))];
    while let Some((part_id, parent_base)) = pending.pop() {
        if part_id >= message.parts.len() {
            continue;
        }
        parent_bases[part_id] = parent_base.clone();
        let part = &message.parts[part_id];
        let effective = part
            .headers
            .iter()
            .find_map(|header| match &header.name {
                HeaderName::Other(name) if name.eq_ignore_ascii_case("Content-Base") => {
                    header.value.as_text()
                }
                _ => None,
            })
            .or_else(|| part.content_location());
        let effective_base = effective
            .and_then(|reference| resolve_mime_uri(parent_base.as_str(), reference))
            .and_then(|resolved| Url::parse(&resolved).ok())
            .map(Arc::new)
            .unwrap_or_else(|| {
                if effective.is_some() {
                    push_warning_once(
                        warnings,
                        "invalid or overlong MIME Content-Base/Location URI was ignored",
                    );
                }
                parent_base
            });
        effective_bases[part_id] = Arc::clone(&effective_base);
        if let PartType::Multipart(children) = &part.body {
            for child_id in children.iter().rev() {
                pending.push((*child_id as usize, Arc::clone(&effective_base)));
            }
        }
    }
    (parent_bases, effective_bases)
}

fn related_part_ids(message: &Message<'_>, html_part_id: u32) -> HashSet<usize> {
    let html_part_id = html_part_id as usize;
    if html_part_id >= message.parts.len() {
        return HashSet::new();
    }
    let mut parents = vec![None; message.parts.len()];
    for (parent_id, part) in message.parts.iter().enumerate() {
        if let PartType::Multipart(children) = &part.body {
            for child_id in children {
                if let Some(parent) = parents.get_mut(*child_id as usize) {
                    *parent = Some(parent_id);
                }
            }
        }
    }

    let mut current = html_part_id;
    let mut scope = HashSet::new();
    while let Some(parent_id) = parents[current] {
        let parent = &message.parts[parent_id];
        let is_related = parent.content_type().is_some_and(|content_type| {
            content_type.ctype().eq_ignore_ascii_case("multipart")
                && content_type
                    .subtype()
                    .is_some_and(|subtype| subtype.eq_ignore_ascii_case("related"))
        });
        if is_related {
            let mut pending = match &parent.body {
                PartType::Multipart(children) => children
                    .iter()
                    .map(|child_id| *child_id as usize)
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            while let Some(part_id) = pending.pop() {
                if !scope.insert(part_id) {
                    continue;
                }
                if let Some(part) = message.parts.get(part_id)
                    && let PartType::Multipart(children) = &part.body
                {
                    pending.extend(children.iter().map(|child_id| *child_id as usize));
                }
            }
        }
        current = parent_id;
    }
    scope
}

pub(crate) fn image_dimensions(bytes: &[u8], mime: &str) -> Option<(u32, u32)> {
    match mime {
        "image/png" => {
            let mut decoder = png::Decoder::new(Cursor::new(bytes));
            decoder.set_limits(png::Limits {
                bytes: MAX_CID_IMAGE_BYTES,
            });
            let reader = decoder.read_info().ok()?;
            Some((reader.info().width, reader.info().height))
        }
        "image/jpeg" => {
            let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
            decoder.set_max_decoding_buffer_size(MAX_CID_IMAGE_BYTES);
            decoder.read_info().ok()?;
            let info = decoder.info()?;
            Some((u32::from(info.width), u32::from(info.height)))
        }
        _ => None,
    }
}

fn normalize_content_id(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_owned()
}

fn push_warning_once(warnings: &mut Vec<String>, warning: impl Into<String>) {
    let warning = warning.into();
    if !warnings.contains(&warning) {
        warnings.push(warning);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::MessageParser;

    #[test]
    fn resolves_mime_uri_references_with_bounded_rfc_style_bases() {
        assert_eq!(
            resolve_mime_uri("https://example.test/a/b/page.html", "../images/logo.png").as_deref(),
            Some("https://example.test/a/images/logo.png")
        );
        assert_eq!(
            resolve_mime_uri(DEFAULT_MIME_BASE_URI, "images/logo.png").as_deref(),
            Some("thismessage:/images/logo.png")
        );
        assert!(
            resolve_mime_uri("https://example.test/", &"x".repeat(MAX_MIME_URI_BYTES + 1))
                .is_none()
        );
    }

    #[test]
    fn content_location_scope_includes_related_parts_and_excludes_other_attachments() {
        let raw = concat!(
            "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n",
            "--outer\r\nContent-Type: multipart/related; boundary=related; type=\"text/html\"\r\n\r\n",
            "--related\r\nContent-Type: text/html\r\n\r\n<html><img src=\"https://example.test/inside.png\"></html>\r\n",
            "--related\r\nContent-Type: image/png\r\nContent-Location: https://example.test/inside.png\r\n\r\ninside\r\n",
            "--related--\r\n",
            "--outer\r\nContent-Type: image/png\r\nContent-Location: https://example.test/outside.png\r\n\r\noutside\r\n",
            "--outer--\r\n"
        );
        let message = MessageParser::default().parse(raw.as_bytes()).unwrap();
        let html_id = message.html_body.first().copied().unwrap();
        let in_scope = related_part_ids(&message, html_id);
        let inside_id = message
            .parts
            .iter()
            .position(|part| part.content_location() == Some("https://example.test/inside.png"))
            .unwrap();
        let outside_id = message
            .parts
            .iter()
            .position(|part| part.content_location() == Some("https://example.test/outside.png"))
            .unwrap();
        assert!(in_scope.contains(&(html_id as usize)));
        assert!(in_scope.contains(&inside_id));
        assert!(!in_scope.contains(&outside_id));
    }

    #[test]
    fn content_location_images_are_collected_only_from_the_related_tree() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=";
        let raw = format!(
            "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n--outer\r\nContent-Type: multipart/related; boundary=related; type=\"text/html\"\r\n\r\n--related\r\nContent-Type: text/html\r\n\r\n<html><img src=\"https://example.test/inside.png\"></html>\r\n--related\r\nContent-Type: image/png\r\nContent-ID: <inside>\r\nContent-Location: https://example.test/inside.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{png}\r\n--related--\r\n--outer\r\nContent-Type: image/png\r\nContent-Location: https://example.test/outside.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{png}\r\n--outer--\r\n"
        );
        let message = MessageParser::default().parse(raw.as_bytes()).unwrap();
        let resources = collect_mime_images(&message, message.html_body.first().copied());
        assert!(resources.images.contains_key("inside"));
        assert!(
            resources
                .images
                .contains_key("https://example.test/inside.png")
        );
        assert!(
            !resources
                .images
                .contains_key("https://example.test/outside.png")
        );
        assert_eq!(resources.part_count, 1);
        assert_eq!(
            resources.part_ids["inside"],
            resources.part_ids["https://example.test/inside.png"]
        );
        assert!(
            resources
                .warnings
                .iter()
                .any(|warning| warning.contains("outside the HTML multipart/related scope"))
        );
    }

    #[test]
    fn content_location_scope_covers_same_and_surrounding_related_multiparts() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9Z0S8AAAAASUVORK5CYII=";
        let raw = format!(
            "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n--outer\r\nContent-Type: multipart/related; boundary=related; type=\"multipart/related\"\r\n\r\n--related\r\nContent-Type: multipart/related; boundary=inner; type=\"text/html\"\r\n\r\n--inner\r\nContent-Type: text/html\r\n\r\n<html><img src=\"https://example.test/inner.png\"><img src=\"https://example.test/outer.png\"></html>\r\n--inner\r\nContent-Type: image/png\r\nContent-Location: https://example.test/inner.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{png}\r\n--inner--\r\n--related\r\nContent-Type: image/png\r\nContent-Location: https://example.test/outer.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{png}\r\n--related--\r\n--outer\r\nContent-Type: image/png\r\nContent-Location: https://example.test/outside.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{png}\r\n--outer--\r\n"
        );
        let message = MessageParser::default().parse(raw.as_bytes()).unwrap();
        let resources = collect_mime_images(&message, message.html_body.first().copied());
        assert!(
            resources
                .images
                .contains_key("https://example.test/inner.png")
        );
        assert!(
            resources
                .images
                .contains_key("https://example.test/outer.png")
        );
        assert!(
            !resources
                .images
                .contains_key("https://example.test/outside.png")
        );
        assert_eq!(resources.part_count, 2);
    }
}
