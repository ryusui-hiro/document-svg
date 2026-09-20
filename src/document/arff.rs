//! Bounded Attribute-Relation File Format (ARFF) dataset preview.
//!
//! ARFF is the text dataset format used by Weka and related tools.  The
//! converter keeps the relation header and renders dense or sparse instances
//! as a paginated inert table.  It never evaluates dates, nominal values, or
//! relation-valued attributes.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_ARFF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARFF_LINES: usize = 1_000_000;
const MAX_ARFF_LINE_BYTES: usize = 1024 * 1024;
const MAX_ARFF_ATTRIBUTES: usize = 256;
const MAX_ARFF_RECORDS: usize = 50_000;
const MAX_ARFF_CELLS: usize = 1_000_000;
const MAX_ARFF_VALUE_BYTES: usize = 64 * 1024;
const MAX_ARFF_NAME_BYTES: usize = 4 * 1024;

/// Returns true for an ARFF header prefix without requiring a file extension.
pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let mut relation = false;
    let mut attribute = false;
    for line in text.lines() {
        let line = strip_comment(line).trim();
        let keyword = line
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        relation |= keyword == "@relation";
        attribute |= keyword == "@attribute";
    }
    relation && attribute
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ARFF_BYTES),
        "ARFF input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("ARFF input must be UTF-8/ASCII: {error}")))?;
    let (mut table, relation, warnings) = parse_arff(&text)?;
    let mut page_sink = ArffPageSink {
        inner: sink,
        relation: &relation,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "arff", options, &mut page_sink)?;
    Ok(warnings)
}

struct ArffPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    relation: &'a str,
    warnings: &'a [String],
}

impl PageConsumer for ArffPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "arff".into();
        page.title = if self.relation.is_empty() {
            "ARFF dataset".into()
        } else {
            format!("ARFF — {}", self.relation)
        };
        if !self.relation.is_empty() {
            page.description = format!("ARFF relation '{}' dataset table", self.relation);
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Clone, Debug)]
struct Attribute {
    name: String,
    kind: String,
}

fn parse_arff(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_ARFF_BYTES {
        return Err(Error::LimitExceeded(format!(
            "ARFF input exceeds {MAX_ARFF_BYTES} bytes"
        )));
    }
    let mut relation = String::new();
    let mut attributes = Vec::<Attribute>::new();
    let mut in_data = false;
    let mut saw_data = false;
    let mut rows = Vec::<Vec<String>>::new();
    let mut warnings = Vec::new();
    let mut total_cells = 0usize;
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_ARFF_LINES {
        return Err(Error::LimitExceeded(format!(
            "ARFF input exceeds {MAX_ARFF_LINES} lines"
        )));
    }

    for (line_number, original) in lines.iter().enumerate() {
        let line = original.trim_end_matches('\r');
        if line.len() > MAX_ARFF_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "ARFF line {} exceeds {MAX_ARFF_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let content = strip_comment(line).trim();
        let content = if line_number == 0 {
            content.strip_prefix('\u{feff}').unwrap_or(content).trim()
        } else {
            content
        };
        if content.is_empty() {
            continue;
        }
        if !in_data {
            let Some((keyword, rest)) = directive(content) else {
                return Err(Error::InvalidInput(format!(
                    "ARFF line {} must contain a header directive",
                    line_number + 1
                )));
            };
            match keyword.as_str() {
                "@relation" => {
                    if !relation.is_empty() {
                        return Err(Error::InvalidInput(
                            "ARFF declares @relation more than once".into(),
                        ));
                    }
                    relation = parse_name(rest, "relation", line_number + 1)?;
                }
                "@attribute" => {
                    if relation.is_empty() {
                        return Err(Error::InvalidInput(format!(
                            "ARFF @attribute appears before @relation at line {}",
                            line_number + 1
                        )));
                    }
                    if attributes.len() >= MAX_ARFF_ATTRIBUTES {
                        return Err(Error::LimitExceeded(format!(
                            "ARFF exceeds {MAX_ARFF_ATTRIBUTES} attributes"
                        )));
                    }
                    let (name, kind) = parse_attribute(rest, line_number + 1)?;
                    if attributes.iter().any(|attribute| attribute.name == name) {
                        return Err(Error::InvalidInput(format!(
                            "ARFF declares duplicate attribute '{name}'"
                        )));
                    }
                    if kind.eq_ignore_ascii_case("relational") {
                        return Err(Error::Unsupported(
                            "ARFF relational attributes are unsupported; flat attributes are required".into(),
                        ));
                    }
                    attributes.push(Attribute { name, kind });
                }
                "@data" => {
                    if relation.is_empty() || attributes.is_empty() {
                        return Err(Error::InvalidInput(
                            "ARFF @data requires a relation and at least one attribute".into(),
                        ));
                    }
                    if saw_data {
                        return Err(Error::InvalidInput(
                            "ARFF declares @data more than once".into(),
                        ));
                    }
                    saw_data = true;
                    in_data = true;
                }
                "@end" => {
                    return Err(Error::Unsupported(
                        "ARFF relational attribute sections are unsupported".into(),
                    ));
                }
                _ => warnings.push(format!("ARFF header directive '{}' was ignored", keyword)),
            }
            continue;
        }

        if rows.len() >= MAX_ARFF_RECORDS {
            return Err(Error::LimitExceeded(format!(
                "ARFF exceeds {MAX_ARFF_RECORDS} data records"
            )));
        }
        let row = if content.starts_with('{') {
            parse_sparse_record(content, attributes.len(), line_number + 1)?
        } else {
            parse_dense_record(content, attributes.len(), line_number + 1)?
        };
        total_cells = total_cells
            .checked_add(row.len())
            .ok_or_else(|| Error::LimitExceeded("ARFF cell count overflowed".into()))?;
        if total_cells > MAX_ARFF_CELLS {
            return Err(Error::LimitExceeded(format!(
                "ARFF exceeds {MAX_ARFF_CELLS} cells"
            )));
        }
        rows.push(row);
    }

    if relation.is_empty() {
        return Err(Error::InvalidInput("ARFF is missing @relation".into()));
    }
    if attributes.is_empty() {
        return Err(Error::InvalidInput(
            "ARFF is missing @attribute declarations".into(),
        ));
    }
    if !saw_data {
        return Err(Error::InvalidInput("ARFF is missing @data".into()));
    }
    let headers = attributes
        .iter()
        .map(|attribute| attribute.name.clone())
        .collect::<Vec<_>>();
    let mut table = TableData {
        headers,
        rows,
        alignments: attributes
            .iter()
            .map(|attribute| {
                let kind = attribute.kind.to_ascii_lowercase();
                if matches!(kind.as_str(), "numeric" | "real" | "integer")
                    || kind.starts_with("date")
                {
                    TableAlign::Right
                } else {
                    TableAlign::Left
                }
            })
            .collect(),
        raw_source: String::new(),
    };
    // Keep the table renderer's own cell truncation warning, while making the
    // type visible in the header without changing the underlying column name.
    for (header, attribute) in table.headers.iter_mut().zip(attributes.iter()) {
        if !attribute.kind.is_empty() {
            *header = format!("{} ({})", header, attribute.kind);
        }
    }
    Ok((table, relation, warnings))
}

fn directive(line: &str) -> Option<(String, &str)> {
    let at = line.find('@')?;
    if at != 0 {
        return None;
    }
    let end = line
        .char_indices()
        .find(|(_, character)| character.is_ascii_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(line.len());
    Some((line[..end].to_ascii_lowercase(), line[end..].trim()))
}

fn parse_attribute(rest: &str, line_number: usize) -> Result<(String, String)> {
    let (name, remainder) = parse_token(rest, line_number, "attribute name")?;
    let kind = remainder.trim();
    if kind.is_empty() {
        return Err(Error::InvalidInput(format!(
            "ARFF attribute '{name}' has no type at line {line_number}"
        )));
    }
    let kind = if kind.starts_with('{') {
        if !kind.ends_with('}') {
            return Err(Error::InvalidInput(format!(
                "ARFF nominal type is not closed at line {line_number}"
            )));
        }
        // Keep nominal values as a compact type label; data values remain inert text.
        format!("nominal {}", kind)
    } else {
        kind.to_string()
    };
    Ok((name, kind))
}

fn parse_name(rest: &str, label: &str, line_number: usize) -> Result<String> {
    let (name, remainder) = parse_token(rest, line_number, label)?;
    if !remainder.trim().is_empty() {
        return Err(Error::InvalidInput(format!(
            "ARFF {label} has trailing content at line {line_number}"
        )));
    }
    Ok(name)
}

fn parse_token<'a>(text: &'a str, line_number: usize, label: &str) -> Result<(String, &'a str)> {
    let text = text.trim_start();
    if text.is_empty() {
        return Err(Error::InvalidInput(format!(
            "ARFF {label} is missing at line {line_number}"
        )));
    }
    let first = text.as_bytes()[0];
    if first == b'\'' || first == b'"' {
        let quote = first as char;
        let mut escaped = false;
        for (offset, character) in text[1..].char_indices() {
            if escaped {
                escaped = false;
                continue;
            }
            if character == '\\' {
                escaped = true;
                continue;
            }
            if character == quote {
                let end = 1 + offset + character.len_utf8();
                let name = decode_quoted(&text[1..1 + offset], line_number)?;
                if name.len() > MAX_ARFF_NAME_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "ARFF {label} exceeds {MAX_ARFF_NAME_BYTES} bytes at line {line_number}"
                    )));
                }
                return Ok((name, &text[end..]));
            }
        }
        return Err(Error::InvalidInput(format!(
            "ARFF {label} quote is not closed at line {line_number}"
        )));
    }
    let end = text
        .char_indices()
        .find(|(_, character)| character.is_ascii_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    if end > MAX_ARFF_NAME_BYTES {
        return Err(Error::LimitExceeded(format!(
            "ARFF {label} exceeds {MAX_ARFF_NAME_BYTES} bytes at line {line_number}"
        )));
    }
    Ok((text[..end].to_string(), &text[end..]))
}

fn parse_dense_record(line: &str, columns: usize, line_number: usize) -> Result<Vec<String>> {
    let values = split_fields(line, ',')?;
    if values.len() != columns {
        return Err(Error::InvalidInput(format!(
            "ARFF dense record at line {line_number} has {} values; expected {columns}",
            values.len()
        )));
    }
    values
        .into_iter()
        .map(|value| normalize_value(&value, line_number))
        .collect()
}

fn parse_sparse_record(line: &str, columns: usize, line_number: usize) -> Result<Vec<String>> {
    if !line.ends_with('}') {
        return Err(Error::InvalidInput(format!(
            "ARFF sparse record at line {line_number} is not closed"
        )));
    }
    let inner = line[1..line.len() - 1].trim();
    let mut values = vec!["0".to_string(); columns];
    if inner.is_empty() {
        return Ok(values);
    }
    let mut seen = vec![false; columns];
    for entry in split_fields(inner, ',')? {
        let entry = entry.trim();
        let split = entry
            .char_indices()
            .find(|(_, character)| character.is_ascii_whitespace())
            .map(|(index, _)| index)
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "ARFF sparse entry at line {line_number} is missing a value"
                ))
            })?;
        let index = entry[..split].parse::<usize>().map_err(|_| {
            Error::InvalidInput(format!(
                "ARFF sparse entry has invalid index at line {line_number}"
            ))
        })?;
        if index >= columns {
            return Err(Error::InvalidInput(format!(
                "ARFF sparse index {index} is outside {columns} columns at line {line_number}"
            )));
        }
        if seen[index] {
            return Err(Error::InvalidInput(format!(
                "ARFF sparse index {index} is repeated at line {line_number}"
            )));
        }
        seen[index] = true;
        values[index] = normalize_value(entry[split..].trim(), line_number)?;
    }
    Ok(values)
}

fn split_fields(text: &str, delimiter: char) -> Result<Vec<String>> {
    let mut fields = Vec::new();
    let mut start = 0usize;
    let mut quote = None::<char>;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote.is_some() && character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character == delimiter {
            fields.push(text[start..index].trim().to_string());
            start = index + character.len_utf8();
        }
    }
    if quote.is_some() {
        return Err(Error::InvalidInput(
            "ARFF data value quote is not closed".into(),
        ));
    }
    fields.push(text[start..].trim().to_string());
    Ok(fields)
}

fn normalize_value(value: &str, line_number: usize) -> Result<String> {
    let value = value.trim();
    let decoded = if value.len() >= 2
        && ((value.starts_with('\'') && value.ends_with('\''))
            || (value.starts_with('"') && value.ends_with('"')))
    {
        decode_quoted(&value[1..value.len() - 1], line_number)?
    } else {
        value.to_string()
    };
    if decoded.len() > MAX_ARFF_VALUE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "ARFF value at line {line_number} exceeds {MAX_ARFF_VALUE_BYTES} bytes"
        )));
    }
    Ok(decoded)
}

fn decode_quoted(value: &str, line_number: usize) -> Result<String> {
    let mut output = String::with_capacity(value.len());
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            output.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            output.push(character);
        }
        if output.len() > MAX_ARFF_VALUE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "ARFF value at line {line_number} exceeds {MAX_ARFF_VALUE_BYTES} bytes"
            )));
        }
    }
    if escaped {
        return Err(Error::InvalidInput(format!(
            "ARFF value has a trailing escape at line {line_number}"
        )));
    }
    Ok(output)
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None::<char>;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote.is_some() && character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character == '%' {
            return &line[..index];
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::{looks_like_prefix, parse_arff};

    #[test]
    fn parses_dense_nominal_and_quoted_values() {
        let source = r#"% comment
@relation weather
@attribute outlook {sunny, overcast, rainy}
@attribute temperature numeric
@attribute note string
@data
sunny,25,'windy, warm'
?,18,"clear"
"#;
        let (table, relation, warnings) = parse_arff(source).unwrap();
        assert_eq!(relation, "weather");
        assert!(warnings.is_empty());
        assert_eq!(table.rows[0][2], "windy, warm");
        assert_eq!(table.rows[1][0], "?");
        assert!(table.headers[1].contains("numeric"));
    }

    #[test]
    fn expands_sparse_records_with_zero_defaults() {
        let source = "@relation sparse\n@attribute a numeric\n@attribute b string\n@attribute c {x,y}\n@data\n{1 'hello', 2 y}\n";
        let (table, _, _) = parse_arff(source).unwrap();
        assert_eq!(table.rows[0], vec!["0", "hello", "y"]);
    }

    #[test]
    fn requires_arff_header_for_content_sniffing() {
        assert!(looks_like_prefix(b"@relation r\n@attribute x numeric\n"));
        assert!(!looks_like_prefix(b"@relation r\n@data\n1\n"));
    }

    #[test]
    fn accepts_a_utf8_bom_before_the_relation_directive() {
        let source = "\u{feff}@relation r\n@attribute x numeric\n@data\n1\n";
        let (_, relation, _) = parse_arff(source).unwrap();
        assert_eq!(relation, "r");
    }

    #[test]
    fn rejects_relational_attributes_instead_of_flattening_them() {
        let source =
            "@relation r\n@attribute bag relational\n@attribute x numeric\n@end bag\n@data\n\n";
        let error = parse_arff(source).unwrap_err();
        assert!(error.to_string().contains("relational attributes"));
    }
}
