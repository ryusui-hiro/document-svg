use quick_xml::XmlVersion;
use quick_xml::events::BytesStart;
use quick_xml::events::attributes::Attribute;

pub(crate) fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

/// The attribute's text, with XML entities resolved.
///
/// `Attribute::value` holds the raw, still-escaped bytes. Taking them verbatim
/// means an alt text of `R&amp;D` reaches the SVG writer as the literal
/// `R&amp;D`, which is escaped again and shows up as `R&amp;D` on screen.
fn attribute_text(item: &Attribute<'_>) -> String {
    // OOXML parts are XML 1.0; none of them carry a 1.1 declaration.
    item.normalized_value(XmlVersion::Implicit1_0).map_or_else(
        |_| String::from_utf8_lossy(item.value.as_ref()).into_owned(),
        |value| value.into_owned(),
    )
}

pub(crate) fn attribute(start: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .find_map(|item| (local_name(item.key.as_ref()) == name).then(|| attribute_text(&item)))
}

pub(crate) fn qualified_attribute(start: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .find_map(|item| (item.key.as_ref() == name).then(|| attribute_text(&item)))
}

pub(crate) fn parse_i64(value: Option<String>, default: i64) -> i64 {
    value
        .as_deref()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub(crate) fn parse_f64(value: Option<String>, default: f64) -> f64 {
    value
        .as_deref()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub(crate) fn color_from_hex(value: &str, fallback: &str) -> String {
    let trimmed = value.trim().trim_start_matches('#');
    if trimmed.len() == 6 && trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        format!("#{}", trimmed.to_ascii_uppercase())
    } else {
        fallback.to_owned()
    }
}
