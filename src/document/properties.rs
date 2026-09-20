//! Bounded, inert previews for Java-style `.properties` files.

use std::collections::HashMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_PROPERTIES_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PROPERTIES_LINES: usize = 100_000;
const MAX_PROPERTIES_LINE_BYTES: usize = 1024 * 1024;
const MAX_PROPERTIES_LOGICAL_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROPERTIES_ROWS: usize = 200_000;
const MAX_PROPERTIES_SCALAR_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROPERTIES_PATH_BYTES: usize = 4 * 1024;
const MAX_PROPERTIES_RENDERED_BYTES: usize = 32 * 1024 * 1024;

struct PropertyRow {
    key: String,
    value: String,
}

struct PropertiesPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for PropertiesPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "properties".into();
        if page.title.is_empty() {
            page.title = "Java properties".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PROPERTIES_BYTES),
        "Java properties input",
    )?;
    let (rows, warnings) = parse_properties(&bytes)?;
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "Java properties conversion requires at least one page; max_pages is zero".into(),
        ));
    }
    let blocks = rows_to_blocks(&rows)?;
    let mut page_sink = PropertiesPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_properties(bytes: &[u8]) -> Result<(Vec<PropertyRow>, Vec<String>)> {
    if bytes.len() as u64 > MAX_PROPERTIES_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Java properties input exceeds {MAX_PROPERTIES_BYTES} bytes"
        )));
    }

    let mut rows = Vec::<PropertyRow>::new();
    let mut key_positions = HashMap::<String, usize>::new();
    let mut duplicate_count = 0usize;
    let mut logical_line = String::new();
    let mut continuing = false;
    let mut offset = 0usize;
    let mut physical_lines = 0usize;

    while offset < bytes.len() {
        let start = offset;
        while offset < bytes.len() && bytes[offset] != b'\r' && bytes[offset] != b'\n' {
            offset += 1;
        }
        let line = &bytes[start..offset];
        if offset < bytes.len() {
            if bytes[offset] == b'\r' && bytes.get(offset + 1) == Some(&b'\n') {
                offset += 2;
            } else {
                offset += 1;
            }
        }
        physical_lines = physical_lines.saturating_add(1);
        if physical_lines > MAX_PROPERTIES_LINES {
            return Err(Error::LimitExceeded(format!(
                "Java properties input exceeds {MAX_PROPERTIES_LINES} physical lines"
            )));
        }
        if line.len() > MAX_PROPERTIES_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Java properties physical line exceeds {MAX_PROPERTIES_LINE_BYTES} bytes"
            )));
        }

        let mut decoded = line
            .iter()
            .map(|byte| char::from(*byte))
            .collect::<String>();
        if continuing {
            decoded = decoded
                .trim_start_matches(is_properties_whitespace)
                .to_owned();
            logical_line.push_str(&decoded);
        } else {
            let significant = decoded.trim_start_matches(is_properties_whitespace);
            if significant.is_empty()
                || significant.starts_with('#')
                || significant.starts_with('!')
            {
                continue;
            }
            logical_line.push_str(significant);
        }
        if logical_line.len() > MAX_PROPERTIES_LOGICAL_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Java properties logical line exceeds {MAX_PROPERTIES_LOGICAL_LINE_BYTES} bytes"
            )));
        }

        let trailing_slashes = logical_line
            .chars()
            .rev()
            .take_while(|character| *character == '\\')
            .count();
        if trailing_slashes % 2 == 1 {
            logical_line.pop();
            continuing = true;
            continue;
        }

        continuing = false;
        insert_entry(
            &logical_line,
            &mut rows,
            &mut key_positions,
            &mut duplicate_count,
        )?;
        logical_line.clear();
    }

    if continuing {
        // OpenJDK's Properties line reader drops the final continuation slash at EOF.
        insert_entry(
            &logical_line,
            &mut rows,
            &mut key_positions,
            &mut duplicate_count,
        )?;
    }

    let warnings = if duplicate_count == 0 {
        Vec::new()
    } else {
        vec![format!(
            "{duplicate_count} duplicate Java properties key(s) use their last value, matching Properties.load"
        )]
    };
    Ok((rows, warnings))
}

fn insert_entry(
    logical_line: &str,
    rows: &mut Vec<PropertyRow>,
    key_positions: &mut HashMap<String, usize>,
    duplicate_count: &mut usize,
) -> Result<()> {
    let (key, value) = parse_logical_line(logical_line)?;
    if key.len() > MAX_PROPERTIES_SCALAR_BYTES || value.len() > MAX_PROPERTIES_SCALAR_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Java properties key/value exceeds {MAX_PROPERTIES_SCALAR_BYTES} decoded bytes"
        )));
    }
    if let Some(index) = key_positions.get(&key).copied() {
        rows[index].value = value;
        *duplicate_count = duplicate_count.saturating_add(1);
    } else {
        if rows.len() >= MAX_PROPERTIES_ROWS {
            return Err(Error::LimitExceeded(format!(
                "Java properties input exceeds {MAX_PROPERTIES_ROWS} distinct keys"
            )));
        }
        key_positions.insert(key.clone(), rows.len());
        rows.push(PropertyRow { key, value });
    }
    Ok(())
}

fn parse_logical_line(line: &str) -> Result<(String, String)> {
    let characters = line.chars().collect::<Vec<_>>();
    let mut key_end = characters.len();
    let mut value_start = characters.len();
    let mut has_separator = false;
    let mut preceding_backslash = false;

    for (index, character) in characters.iter().copied().enumerate() {
        if !preceding_backslash && (character == '=' || character == ':') {
            key_end = index;
            value_start = index + 1;
            has_separator = true;
            break;
        }
        if !preceding_backslash && is_properties_whitespace(character) {
            key_end = index;
            value_start = index + 1;
            break;
        }
        if character == '\\' {
            preceding_backslash = !preceding_backslash;
        } else {
            preceding_backslash = false;
        }
    }

    while value_start < characters.len() {
        let character = characters[value_start];
        if is_properties_whitespace(character) {
            value_start += 1;
        } else if !has_separator && (character == '=' || character == ':') {
            has_separator = true;
            value_start += 1;
        } else {
            break;
        }
    }

    let key = decode_escapes(&characters[..key_end])?;
    let value = decode_escapes(&characters[value_start..])?;
    Ok((key, value))
}

fn decode_escapes(source: &[char]) -> Result<String> {
    let mut utf16 = Vec::<u16>::with_capacity(source.len());
    let mut index = 0usize;
    while index < source.len() {
        let character = source[index];
        index += 1;
        if character != '\\' {
            let mut encoded = [0u16; 2];
            utf16.extend_from_slice(character.encode_utf16(&mut encoded));
            continue;
        }

        let Some(escaped) = source.get(index).copied() else {
            return Err(Error::InvalidInput(
                "Java properties escape ends at end of logical line".into(),
            ));
        };
        index += 1;
        match escaped {
            't' => utf16.push('\t' as u16),
            'n' => utf16.push('\n' as u16),
            'r' => utf16.push('\r' as u16),
            'f' => utf16.push('\u{000c}' as u16),
            'u' => {
                if index + 4 > source.len() {
                    return Err(Error::InvalidInput(
                        "Java properties Unicode escape must contain exactly four hex digits"
                            .into(),
                    ));
                }
                let mut code_unit = 0u16;
                for digit in &source[index..index + 4] {
                    let Some(value) = digit.to_digit(16) else {
                        return Err(Error::InvalidInput(
                            "Java properties Unicode escape contains a non-hex digit".into(),
                        ));
                    };
                    code_unit = code_unit * 16 + value as u16;
                }
                utf16.push(code_unit);
                index += 4;
            }
            other => {
                let mut encoded = [0u16; 2];
                utf16.extend_from_slice(other.encode_utf16(&mut encoded));
            }
        }
    }
    String::from_utf16(&utf16).map_err(|error| {
        Error::InvalidInput(format!(
            "Java properties Unicode escapes do not form valid UTF-16: {error}"
        ))
    })
}

fn is_properties_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\u{000c}')
}

fn rows_to_blocks(rows: &[PropertyRow]) -> Result<Vec<HtmlBlock>> {
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: "Java properties".into(),
    }];
    let mut rendered_bytes = 0usize;
    for row in rows {
        let path = format!("$[{}]", serde_json::to_string(&row.key)?);
        if path.len() > MAX_PROPERTIES_PATH_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Java properties path exceeds {MAX_PROPERTIES_PATH_BYTES} bytes"
            )));
        }
        let value = serde_json::to_string(&row.value)?;
        let line = format!("{path} = {value}");
        rendered_bytes = rendered_bytes.saturating_add(line.len());
        if rendered_bytes > MAX_PROPERTIES_RENDERED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Java properties preview text exceeds {MAX_PROPERTIES_RENDERED_BYTES} bytes"
            )));
        }
        blocks.push(HtmlBlock::Paragraph { text: line });
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_java_properties_comments_escapes_and_continuations() {
        let source = b"# comment\r\n! another comment\nname = Catalog\naccent=\xC3\xA9\nkey\\:with\\=separators: value\ncontinued=first\\\n  second\nemoji=\\uD83D\\uDE80\n";
        let (rows, warnings) = parse_properties(source).unwrap();
        let values = rows
            .into_iter()
            .map(|row| (row.key, row.value))
            .collect::<HashMap<_, _>>();
        assert_eq!(values.get("name").map(String::as_str), Some("Catalog"));
        assert_eq!(values.get("accent").map(String::as_str), Some("Ã©"));
        assert_eq!(
            values.get("key:with=separators").map(String::as_str),
            Some("value")
        );
        assert_eq!(
            values.get("continued").map(String::as_str),
            Some("firstsecond")
        );
        assert_eq!(values.get("emoji").map(String::as_str), Some("🚀"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn duplicate_keys_use_last_value_and_malformed_unicode_fails() {
        let (rows, warnings) = parse_properties(b"name=first\nname=last\n").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].value, "last");
        assert!(warnings.iter().any(|warning| warning.contains("duplicate")));
        assert!(parse_properties(b"value=\\u12xz\n").is_err());
        assert!(parse_properties(b"value=\\uD800\n").is_err());
        let (rows, _) = parse_properties(b"unfinished=kept\\").unwrap();
        assert_eq!(rows[0].value, "kept");
    }

    #[test]
    fn bounds_input_lines_and_rows() {
        assert!(matches!(
            parse_properties(&vec![b'x'; MAX_PROPERTIES_BYTES as usize + 1]),
            Err(Error::LimitExceeded(_))
        ));
        let too_long = vec![b'x'; MAX_PROPERTIES_LINE_BYTES + 1];
        assert!(matches!(
            parse_properties(&too_long),
            Err(Error::LimitExceeded(_))
        ));
    }
}
