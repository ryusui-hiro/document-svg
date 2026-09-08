use quick_xml::events::BytesStart;

pub(crate) fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| *byte == b':').next().unwrap_or(name)
}

pub(crate) fn attribute(start: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .find_map(|item| {
            (local_name(item.key.as_ref()) == name)
                .then(|| String::from_utf8_lossy(item.value.as_ref()).into_owned())
        })
}

pub(crate) fn qualified_attribute(start: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    start
        .attributes()
        .with_checks(false)
        .flatten()
        .find_map(|item| {
            (item.key.as_ref() == name)
                .then(|| String::from_utf8_lossy(item.value.as_ref()).into_owned())
        })
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
