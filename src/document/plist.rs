//! Bounded Apple Property List previews.
//!
//! XML property lists are walked as typed key paths. Binary property lists are
//! preflighted through their `bplist00` trailer and offset table, then shown as
//! a bounded object-marker summary without expanding arbitrary payloads.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};
use base64::Engine;

const MAX_PLIST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PLIST_XML_EVENTS: usize = 1_000_000;
const MAX_PLIST_XML_NODES: usize = 500_000;
const MAX_PLIST_XML_DEPTH: usize = 96;
const MAX_PLIST_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PLIST_OBJECTS: usize = 500_000;
const MAX_PLIST_ROWS: usize = 200_000;
const MAX_PLIST_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"bplist00")
        || crate::geospatial::xml_tree::looks_like_root(bytes, b"plist", None)
}

struct PlistPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for PlistPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "plist".into();
        if page.title.is_empty() {
            page.title = "Apple Property List".into();
        }
        page.description =
            "Apple Property List structure is rendered inertly; secret values, URLs, binary payloads and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    format: String,
    version: String,
    objects: usize,
    dictionaries: usize,
    arrays: usize,
    scalars: usize,
    data_bytes: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PLIST_BYTES),
        "Property List input",
    )?;
    let (summary, warnings) = if bytes.starts_with(b"bplist00") {
        parse_binary(&bytes)?
    } else {
        parse_xml(&bytes, options)?
    };
    let metadata = if summary.format == "Binary" {
        format!(
            "Format: Binary plist\nVersion: {}\nObjects: {}\nDictionaries: {}\nArrays: {}\nScalars: {}\nData bytes: {}",
            display_or_dash(&summary.version),
            summary.objects,
            summary.dictionaries,
            summary.arrays,
            summary.scalars,
            summary.data_bytes,
        )
    } else {
        format!(
            "Format: XML plist\nVersion: {}\nObjects: {}\nDictionaries: {}\nArrays: {}\nScalars: {}\nData bytes: {}",
            display_or_dash(&summary.version),
            summary.objects,
            summary.dictionaries,
            summary.arrays,
            summary.scalars,
            summary.data_bytes,
        )
    };
    let rows = if summary.rows.is_empty() {
        vec![vec!["—".into(), "—".into(), "—".into(), "0".into()]]
    } else {
        summary.rows
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Apple Property List".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Path".into(), "Type".into(), "Value".into(), "N".into()],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = PlistPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_xml(bytes: &[u8], options: &ConvertOptions) -> Result<(Summary, Vec<String>)> {
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_PLIST_XML_EVENTS),
            max_nodes: MAX_PLIST_XML_NODES,
            max_depth: MAX_PLIST_XML_DEPTH,
            max_text_bytes: MAX_PLIST_TEXT_BYTES,
        },
        "XML Property List",
    )?;
    if root.name != "plist" {
        return Err(Error::InvalidInput(
            "XML Property List root must be plist".into(),
        ));
    }
    let version = root.attribute("version").unwrap_or("1.0").to_owned();
    let value = root.children.iter().find(|child| child.name != "#text");
    let mut summary = Summary {
        format: "XML".into(),
        version,
        ..Summary::default()
    };
    if let Some(value) = value {
        walk_xml_value(value, "", None, &mut summary)?;
    }
    let warnings = vec![
        "Property List key paths and types are shown; URL values, secret-like scalar values, data payloads and arbitrary extension values are redacted or omitted".into(),
        "XML Property List traversal and rendered rows are bounded; DOCTYPE/external entities and resource resolution are never executed".into(),
    ];
    Ok((summary, warnings))
}

fn walk_xml_value(
    element: &XmlElement,
    path: &str,
    key_hint: Option<&str>,
    summary: &mut Summary,
) -> Result<()> {
    if summary.rows.len() >= MAX_PLIST_ROWS {
        return Err(Error::LimitExceeded(format!(
            "Property List rows exceed {MAX_PLIST_ROWS}"
        )));
    }
    summary.objects = summary.objects.saturating_add(1);
    match element.name.as_str() {
        "dict" => {
            summary.dictionaries = summary.dictionaries.saturating_add(1);
            let pairs = element
                .children
                .iter()
                .filter(|child| child.name != "key")
                .count();
            push_row(summary, path, "dict", format!("{pairs} entries"), pairs)?;
            let mut pending_key = None;
            for child in &element.children {
                if child.name == "key" {
                    pending_key = Some(truncate(child.text.trim()));
                } else if let Some(key) = pending_key.take() {
                    let child_path = join_key(path, &key);
                    walk_xml_value(child, &child_path, Some(&key), summary)?;
                }
            }
        }
        "array" => {
            summary.arrays = summary.arrays.saturating_add(1);
            let count = element.children.len();
            push_row(summary, path, "array", format!("{count} items"), count)?;
            for (index, child) in element.children.iter().enumerate() {
                let child_path = format!("{}[{}]", if path.is_empty() { "$" } else { path }, index);
                walk_xml_value(child, &child_path, key_hint, summary)?;
            }
        }
        "string" | "integer" | "real" | "date" | "uid" | "true" | "false" | "null" => {
            summary.scalars = summary.scalars.saturating_add(1);
            let value = if matches!(element.name.as_str(), "true" | "false" | "null") {
                element.name.clone()
            } else {
                safe_scalar(element.text.trim(), key_hint)
            };
            push_row(summary, path, &element.name, value, 1)?;
        }
        "data" => {
            let decoded_size = decode_data_size(element.text.trim())?;
            summary.data_bytes = summary.data_bytes.saturating_add(decoded_size);
            push_row(
                summary,
                path,
                "data",
                format!("{decoded_size} decoded bytes omitted"),
                1,
            )?;
        }
        _ => {
            push_row(
                summary,
                path,
                &element.name,
                "unsupported value omitted".into(),
                1,
            )?;
        }
    }
    Ok(())
}

fn decode_data_size(value: &str) -> Result<usize> {
    let compact: String = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map(|bytes| bytes.len())
        .map_err(|error| Error::InvalidInput(format!("invalid Property List data value: {error}")))
}

fn push_row(
    summary: &mut Summary,
    path: &str,
    kind: &str,
    value: String,
    count: usize,
) -> Result<()> {
    if summary.rows.len() >= MAX_PLIST_ROWS {
        return Err(Error::LimitExceeded(format!(
            "Property List rows exceed {MAX_PLIST_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(if path.is_empty() { "$" } else { path }),
        truncate(kind),
        truncate(&value),
        count.to_string(),
    ]);
    Ok(())
}

fn parse_binary(bytes: &[u8]) -> Result<(Summary, Vec<String>)> {
    if bytes.len() < 40 {
        return Err(Error::InvalidInput(
            "binary Property List is shorter than its header and trailer".into(),
        ));
    }
    let trailer = bytes.len() - 32;
    let offset_size = bytes[trailer + 6] as usize;
    let ref_size = bytes[trailer + 7] as usize;
    let objects = read_u64(&bytes[trailer + 8..trailer + 16])?;
    let top = read_u64(&bytes[trailer + 16..trailer + 24])?;
    let offsets = read_u64(&bytes[trailer + 24..trailer + 32])?;
    if !(1..=8).contains(&offset_size) || !(1..=16).contains(&ref_size) {
        return Err(Error::InvalidInput(
            "binary Property List has invalid offset/reference integer sizes".into(),
        ));
    }
    let objects_usize = usize::try_from(objects).map_err(|_| {
        Error::LimitExceeded("binary Property List object count overflows usize".into())
    })?;
    if objects_usize == 0 || objects_usize > MAX_PLIST_OBJECTS || top >= objects {
        return Err(Error::LimitExceeded(
            "binary Property List object count/top object is outside limits".into(),
        ));
    }
    let offset_table = usize::try_from(offsets).map_err(|_| {
        Error::InvalidInput("binary Property List offset table overflows usize".into())
    })?;
    let table_bytes = objects_usize.checked_mul(offset_size).ok_or_else(|| {
        Error::LimitExceeded("binary Property List offset table is too large".into())
    })?;
    let table_end = offset_table.checked_add(table_bytes).ok_or_else(|| {
        Error::LimitExceeded("binary Property List offset table overflows".into())
    })?;
    if offset_table < 8 || table_end > trailer {
        return Err(Error::InvalidInput(
            "binary Property List offset table is outside the file".into(),
        ));
    }
    let mut summary = Summary {
        format: "Binary".into(),
        version: String::from_utf8_lossy(&bytes[6..8]).into_owned(),
        objects: objects_usize,
        ..Summary::default()
    };
    for index in 0..objects_usize {
        if summary.rows.len() >= MAX_PLIST_ROWS {
            return Err(Error::LimitExceeded(format!(
                "Property List rows exceed {MAX_PLIST_ROWS}"
            )));
        }
        let start = offset_table + index * offset_size;
        let object_offset = read_uint(&bytes[start..start + offset_size])?;
        let object_offset = usize::try_from(object_offset).map_err(|_| {
            Error::InvalidInput("binary Property List object offset overflows".into())
        })?;
        if !(8..trailer).contains(&object_offset) {
            return Err(Error::InvalidInput(
                "binary Property List object offset is outside the file".into(),
            ));
        }
        let marker = bytes[object_offset];
        let kind = match marker >> 4 {
            0x0 => "simple",
            0x1 => "integer",
            0x2 => "real",
            0x3 => "date",
            0x4 => "data",
            0x5 => "ASCII string",
            0x6 => "UTF-16 string",
            0x7 => "UTF-8 string",
            0x8 => "UID",
            0xA => {
                summary.arrays = summary.arrays.saturating_add(1);
                "array"
            }
            0xC => {
                summary.arrays = summary.arrays.saturating_add(1);
                "set"
            }
            0xD => {
                summary.dictionaries = summary.dictionaries.saturating_add(1);
                "dictionary"
            }
            _ => "unknown",
        };
        if matches!(marker >> 4, 0x1..=0x3 | 0x5..=0x8) {
            summary.scalars = summary.scalars.saturating_add(1);
        }
        if marker >> 4 == 0x4 {
            summary.data_bytes = summary.data_bytes.saturating_add(marker as usize & 0x0f);
        }
        summary.rows.push(vec![
            format!("object[{index}]"),
            kind.into(),
            format!("marker 0x{marker:02x}"),
            "header only".into(),
        ]);
    }
    let warnings = vec![
        "Binary Property List object markers and offsets are shown; strings, keys, URL values and data payloads are not expanded".into(),
        "Binary Property List references are not dereferenced, and no external resource or application data is executed".into(),
    ];
    Ok((summary, warnings))
}

fn read_u64(bytes: &[u8]) -> Result<u64> {
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| Error::InvalidInput("Property List trailer is truncated".into()))?;
    Ok(u64::from_be_bytes(array))
}

fn read_uint(bytes: &[u8]) -> Result<u64> {
    if bytes.len() > 8 {
        return Err(Error::Unsupported(
            "binary Property List offset values wider than 8 bytes are unsupported".into(),
        ));
    }
    let mut value = 0u64;
    for byte in bytes {
        value = value
            .checked_shl(8)
            .and_then(|value| value.checked_add(*byte as u64))
            .ok_or_else(|| Error::LimitExceeded("binary Property List offset overflows".into()))?;
    }
    Ok(value)
}

fn join_key(path: &str, key: &str) -> String {
    if path.is_empty() {
        format!("$.{key}")
    } else {
        format!("{path}.{key}")
    }
}

fn safe_scalar(value: &str, key: Option<&str>) -> String {
    let lower = key.unwrap_or_default().to_ascii_lowercase();
    if [
        "password",
        "passwd",
        "secret",
        "token",
        "private",
        "credential",
        "cookie",
        "authorization",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return "[redacted]".into();
    }
    if value.contains("://") {
        return "[URL omitted]".into();
    }
    truncate(value)
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_PLIST_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_PLIST_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_xml_and_binary_property_lists() {
        assert!(looks_like_prefix(
            br#"<?xml version="1.0"?><plist version="1.0"><dict/></plist>"#
        ));
        assert!(looks_like_prefix(b"bplist00"));
        assert!(!looks_like_prefix(br#"<dict><key>x</key></dict>"#));
    }

    #[test]
    fn redacts_sensitive_and_url_scalars() {
        assert_eq!(
            safe_scalar("https://private.example.invalid", Some("url")),
            "[URL omitted]"
        );
        assert_eq!(safe_scalar("secret", Some("password")), "[redacted]");
        assert_eq!(safe_scalar("value", Some("name")), "value");
    }

    #[test]
    fn validates_binary_trailer_and_offsets() {
        let bytes = super::tests::minimal_binary_plist();
        let (summary, warnings) = parse_binary(&bytes).unwrap();
        assert_eq!(summary.format, "Binary");
        assert_eq!(summary.version, "00");
        assert_eq!(summary.objects, 3);
        assert!(!warnings.is_empty());
    }

    fn minimal_binary_plist() -> Vec<u8> {
        // bplist00 + ASCII "Name" + ASCII "Demo" + dict(key=0, value=1)
        let mut bytes = b"bplist00".to_vec();
        bytes.extend_from_slice(&[0x54, b'N', b'a', b'm', b'e']);
        bytes.extend_from_slice(&[0x54, b'D', b'e', b'm', b'o']);
        bytes.extend_from_slice(&[0xD1, 0x00, 0x01]);
        let offset_table = bytes.len();
        bytes.extend_from_slice(&[0x08, 0x0d, 0x12]);
        let trailer_offset = bytes.len();
        bytes.extend_from_slice(&[0; 6]);
        bytes.push(1); // offsetIntSize
        bytes.push(1); // objectRefSize
        bytes.extend_from_slice(&(3u64).to_be_bytes());
        bytes.extend_from_slice(&(2u64).to_be_bytes());
        bytes.extend_from_slice(&(offset_table as u64).to_be_bytes());
        assert_eq!(bytes.len(), trailer_offset + 32);
        bytes
    }
}
