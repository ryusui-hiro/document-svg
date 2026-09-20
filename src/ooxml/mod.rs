pub(crate) mod chart;
pub(crate) mod docx;
mod package;
pub(crate) mod pptx;
pub(crate) mod xls;
pub(crate) mod xlsb;
pub(crate) mod xlsx;
mod xml;

pub(crate) use package::{MAX_ZIP_PACKAGE_ENTRIES, Relationships, ZipPackage, resolve_part_target};
pub(crate) use xml::{
    attribute, color_from_hex, local_name, parse_f64, parse_i64, qualified_attribute,
};

/// Resolve an XML entity reference such as `&amp;` to the text it stands for.
///
/// quick-xml reports these as `Event::GeneralRef` rather than as text, so a
/// parser that only handles `Event::Text` silently drops every `&` in the
/// document. Every text-collecting loop in this module needs both events.
pub(crate) fn decode_xml_reference(
    reference: &quick_xml::events::BytesRef<'_>,
    context: &str,
) -> crate::error::Result<String> {
    let name = reference.decode().map_err(|error| {
        crate::error::Error::InvalidInput(format!("invalid XML reference in {context}: {error}"))
    })?;
    quick_xml::escape::unescape(&format!("&{name};"))
        .map(|value| value.into_owned())
        .map_err(|error| {
            crate::error::Error::InvalidInput(format!(
                "invalid XML reference in {context}: {error}"
            ))
        })
}

/// The image type `bytes` really are, when their signature says so.
///
/// Word and PowerPoint keep whatever file name an image arrived with, so a
/// JPEG stored as `media/image1.png` is common. A `data:` URI that promises
/// PNG and delivers JPEG renders as nothing in strict SVG renderers, and the
/// document simply loses the picture, so trust the bytes over the extension.
/// Returns `None` for anything not recognised, leaving the extension to decide.
pub(crate) fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(&[0x01, 0x00, 0x00, 0x00]) {
        return Some("image/x-emf");
    }
    if bytes.starts_with(&[0xd7, 0xcd, 0xc6, 0x9a]) {
        return Some("image/x-wmf");
    }
    None
}

pub(crate) fn text_advance_factor(character: char) -> f64 {
    let code = u32::from(character);
    if character == '\t' {
        return 1.5;
    }
    if character.is_control() || character == '\u{00ad}' {
        return 0.0;
    }
    if matches!(
        code,
        0x0300..=0x036f
            | 0x1ab0..=0x1aff
            | 0x1dc0..=0x1dff
            | 0x20d0..=0x20ff
            | 0xfe20..=0xfe2f
    ) {
        return 0.0;
    }
    if character.is_whitespace() {
        return 0.28;
    }
    if character.is_ascii() {
        return match character {
            'i' | 'l' | 'I' | '!' | '|' | '.' | ',' | ':' | ';' | '\'' | '`' => 0.28,
            'f' | 'j' | 'r' | 't' | '(' | ')' | '[' | ']' | '{' | '}' => 0.38,
            'm' | 'w' | 'M' | 'W' | '@' | '%' | '&' => 0.82,
            'A'..='Z' => 0.64,
            '0'..='9' => 0.56,
            'a'..='z' => 0.52,
            _ => 0.5,
        };
    }
    if matches!(code, 0xff61..=0xff9f) {
        return 0.5;
    }
    if matches!(
        code,
        0x1100..=0x11ff
            | 0x2e80..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe6f
            | 0xff01..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f000..=0x1faff
    ) {
        return 1.0;
    }
    0.58
}

#[cfg(test)]
mod tests {
    use super::{sniff_image_mime, text_advance_factor};

    #[test]
    fn estimates_deterministic_script_aware_character_widths() {
        assert!(text_advance_factor('i') < text_advance_factor('W'));
        assert_eq!(text_advance_factor('漢'), 1.0);
        assert_eq!(text_advance_factor('ｶ'), 0.5);
        assert_eq!(text_advance_factor('\u{0301}'), 0.0);
    }

    #[test]
    fn reads_the_image_type_from_the_signature_not_the_name() {
        assert_eq!(
            sniff_image_mime(b"\xff\xd8\xff\xe0\x00\x10JFIF"),
            Some("image/jpeg")
        );
        assert_eq!(sniff_image_mime(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
        assert_eq!(sniff_image_mime(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff_image_mime(b"RIFF____WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image_mime(b"<svg xmlns"), None);
        assert_eq!(sniff_image_mime(b""), None);
    }
}
