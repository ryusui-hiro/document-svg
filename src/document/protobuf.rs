//! Bounded Protocol Buffers `.proto` schema previews.
//!
//! Protobuf files describe serialization and RPC schemas. This adapter renders
//! syntax/package metadata plus message fields, enum values and service RPCs as
//! inert rows. Imports, options, annotations and custom URLs remain text; no
//! `protoc`, code generator, RPC client, or external file is invoked.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_PROTO_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PROTO_LINES: usize = 500_000;
const MAX_PROTO_LINE_BYTES: usize = 1024 * 1024;
const MAX_PROTO_DEFINITIONS: usize = 100_000;
const MAX_PROTO_FIELDS: usize = 300_000;
const MAX_PROTO_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_PROTO_TEXT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let has_syntax = text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("syntax = \"proto")
    });
    if has_syntax {
        return true;
    }
    let has_package = text
        .lines()
        .any(|line| line.trim_start().starts_with("package "));
    let has_message_or_service = text.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("message ") || line.starts_with("service ")
    });
    let has_field_assignment = text.lines().any(|line| {
        let line = line.trim();
        line.contains(" = ") && line.ends_with(';')
    });
    has_package
        && (has_message_or_service
            || text
                .lines()
                .any(|line| line.trim_start().starts_with("enum ")))
        || has_message_or_service && has_field_assignment
}

struct ProtoPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for ProtoPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "protobuf".into();
        if page.title.is_empty() {
            page.title = "Protocol Buffers schema".into();
        }
        page.description = "Protocol Buffers declarations are rendered inertly; imports, code generation and RPC calls are not executed".into();
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
        options.max_input_bytes.min(MAX_PROTO_BYTES),
        "Protocol Buffers input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Protocol Buffers input must be UTF-8: {error}"))
    })?;
    let (table, metadata, warnings) = parse_proto(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Protocol Buffers schema".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = ProtoPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_proto(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_PROTO_BYTES || text.len() > MAX_PROTO_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Protocol Buffers input exceeds {MAX_PROTO_BYTES} bytes"
        )));
    }
    let mut rows = Vec::new();
    let mut current = None::<(String, String, String)>;
    let mut brace_depth = 0usize;
    let mut definitions = 0usize;
    let mut fields = 0usize;
    let mut unsupported = 0usize;
    let mut syntax = String::new();
    let mut package = String::new();
    let mut imports = 0usize;
    for (line_number, raw) in text.lines().enumerate() {
        if line_number >= MAX_PROTO_LINES {
            return Err(Error::LimitExceeded(format!(
                "Protocol Buffers input exceeds {MAX_PROTO_LINES} lines"
            )));
        }
        if raw.len() > MAX_PROTO_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Protocol Buffers line {} exceeds {MAX_PROTO_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if brace_depth == 0 {
            if line.starts_with("syntax") {
                syntax = line.trim_end_matches(';').to_owned();
                continue;
            }
            if line.starts_with("package") {
                package = line.trim_end_matches(';').to_owned();
                continue;
            }
            if line.starts_with("import") {
                imports += 1;
                continue;
            }
            if line.starts_with("option") {
                unsupported += 1;
                continue;
            }
            if let Some((kind, name, header, has_open)) = definition_header(line) {
                definitions += 1;
                if definitions > MAX_PROTO_DEFINITIONS {
                    return Err(Error::LimitExceeded(format!(
                        "Protocol Buffers definitions exceed {MAX_PROTO_DEFINITIONS}"
                    )));
                }
                brace_depth = line
                    .matches('{')
                    .count()
                    .saturating_sub(line.matches('}').count());
                current = Some((kind, name, header));
                if let Some(open) = line.find('{') {
                    let body = line[open + 1..].trim_end_matches('}').trim();
                    fields += parse_members(body, current.as_ref().unwrap(), &mut rows)?;
                }
                if brace_depth == 0 {
                    flush_current(&mut current, &mut rows);
                }
                if has_open {
                    continue;
                }
            }
            continue;
        }
        let closes = line.matches('}').count();
        let opens = line.matches('{').count();
        let body = line.replace(['{', '}'], " ");
        if let Some(ref descriptor) = current {
            fields += parse_members(&body, descriptor, &mut rows)?;
            if fields > MAX_PROTO_FIELDS {
                return Err(Error::LimitExceeded(format!(
                    "Protocol Buffers fields exceed {MAX_PROTO_FIELDS}"
                )));
            }
        }
        brace_depth = brace_depth.saturating_add(opens).saturating_sub(closes);
        if brace_depth == 0 {
            flush_current(&mut current, &mut rows);
        }
    }
    if brace_depth != 0 || current.is_some() {
        return Err(Error::InvalidInput(
            "Protocol Buffers input has an unclosed declaration".into(),
        ));
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "Protocol Buffers input contains no message, enum or service members".into(),
        ));
    }
    let mut warnings = vec!["Protocol Buffers imports, options, annotations, default values and URLs remain inert; no protoc/code generation, external file access, RPC call or message validation is performed".into()];
    if imports > 0 {
        warnings.push(format!("{imports} protobuf import(s) were not opened"));
    }
    if unsupported > 0 {
        warnings.push(format!(
            "{unsupported} protobuf option line(s) were omitted from the table"
        ));
    }
    let metadata = format!(
        "{}{}Definitions: {definitions}\nMembers: {fields}",
        if syntax.is_empty() {
            String::new()
        } else {
            format!("{syntax}\n")
        },
        if package.is_empty() {
            String::new()
        } else {
            format!("{package}\n")
        }
    );
    Ok((
        TableData {
            headers: vec![
                "Container".into(),
                "Kind".into(),
                "Name".into(),
                "Type / RPC".into(),
                "Number / options".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn definition_header(line: &str) -> Option<(String, String, String, bool)> {
    let mut tokens = line.split_whitespace();
    let mut kind = tokens.next()?.to_owned();
    if kind == "extend" {
        kind = format!("extend {}", tokens.next()?);
    }
    if !matches!(kind.as_str(), "message" | "enum" | "service" | "oneof")
        && !kind.starts_with("extend ")
    {
        return None;
    }
    let name = tokens.next()?.trim_end_matches('{').to_owned();
    let has_open = line.contains('{');
    Some((kind, name, tokens.collect::<Vec<_>>().join(" "), has_open))
}

fn parse_members(
    body: &str,
    descriptor: &(String, String, String),
    rows: &mut Vec<Vec<String>>,
) -> Result<usize> {
    let mut count = 0usize;
    for raw in body.split(';') {
        let line = raw.trim().trim_matches(',');
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with("option ")
            || line.starts_with("reserved ")
            || line.starts_with("extensions ")
        {
            continue;
        }
        if descriptor.0 == "enum" {
            let Some((name, number)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            let number = number
                .trim()
                .trim_matches(|character| character == '[' || character == ']');
            if !is_name(name) {
                continue;
            }
            rows.push(vec![
                descriptor.1.clone(),
                "enum value".into(),
                name.to_owned(),
                String::new(),
                number.to_owned(),
            ]);
            count += 1;
            continue;
        }
        if descriptor.0 == "service" {
            if !line.starts_with("rpc ") {
                continue;
            }
            let rest = line.trim_start_matches("rpc ").trim();
            let name = rest.split(['(', ' ']).next().unwrap_or_default();
            if !is_name(name) {
                continue;
            }
            let signature = rest.to_owned();
            rows.push(vec![
                descriptor.1.clone(),
                "rpc".into(),
                name.to_owned(),
                truncate(&signature),
                String::new(),
            ]);
            count += 1;
            continue;
        }
        if line.starts_with("message ") || line.starts_with("enum ") || line.starts_with("service ")
        {
            continue;
        }
        let Some(eq) = line.find('=') else {
            continue;
        };
        let left = line[..eq].trim();
        let number_and_options = line[eq + 1..].trim();
        let number = number_and_options
            .split(['[', ' ', '\n'])
            .next()
            .unwrap_or_default();
        if number.is_empty() {
            continue;
        }
        let mut tokens = left.split_whitespace().collect::<Vec<_>>();
        let field_name = tokens.pop().unwrap_or_default();
        let field_type = tokens.join(" ");
        if !is_name(field_name) || field_type.is_empty() {
            continue;
        }
        let options = number_and_options
            .split_once('[')
            .map(|(_, value)| value.trim_end_matches(']').trim())
            .unwrap_or_default();
        rows.push(vec![
            descriptor.1.clone(),
            if descriptor.0 == "oneof" {
                "oneof field".into()
            } else {
                "field".into()
            },
            field_name.to_owned(),
            truncate(&field_type),
            if options.is_empty() {
                number.to_owned()
            } else {
                format!("{number} [{options}]")
            },
        ]);
        count += 1;
        if rows.len() > MAX_PROTO_FIELDS {
            return Err(Error::LimitExceeded(format!(
                "Protocol Buffers fields exceed {MAX_PROTO_FIELDS}"
            )));
        }
    }
    Ok(count)
}

fn flush_current(current: &mut Option<(String, String, String)>, rows: &mut Vec<Vec<String>>) {
    let Some((kind, name, details)) = current.take() else {
        return;
    };
    if !rows.iter().any(|row| row[0] == name) {
        rows.push(vec![
            name,
            kind,
            String::new(),
            String::new(),
            truncate(&details),
        ]);
    }
}

fn is_name(value: &str) -> bool {
    !value.is_empty()
        && value.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character.is_ascii_alphanumeric()
                    && (index > 0 || character.is_ascii_alphabetic())
        })
}
fn strip_comment(line: &str) -> &str {
    line.split_once("//").map_or(line, |(head, _)| head)
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_PROTO_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_PROTO_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previews_proto_messages_enums_and_services() {
        let source = "syntax = \"proto3\";\npackage catalog.v1;\nimport \"google/protobuf/empty.proto\";\nmessage Pet {\n  string name = 1 [(validate.rules).string.min_len = 1];\n}\nenum Status { UNKNOWN = 0; ACTIVE = 1; }\nservice Catalog { rpc GetPet(GetPetRequest) returns (Pet); }\n";
        let (table, metadata, warnings) = parse_proto(source).unwrap();
        assert!(metadata.contains("catalog.v1"));
        assert!(table.rows.iter().any(|row| row[2] == "name"));
        assert!(table.rows.iter().any(|row| row[1] == "rpc"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not opened"))
        );
    }
    #[test]
    fn rejects_unclosed_proto_declaration() {
        assert!(parse_proto("message Pet { string name = 1;\n").is_err());
    }
}
