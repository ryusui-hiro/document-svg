//! Bounded GraphQL Schema Definition Language (SDL) previews.
//!
//! The SDL describes a type system, while resolvers and execution live outside
//! the document. This adapter renders type definitions and field signatures as
//! inert rows; no introspection, resolver, query, mutation, or subscription is run.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_GRAPHQL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GRAPHQL_LINES: usize = 500_000;
const MAX_GRAPHQL_LINE_BYTES: usize = 1024 * 1024;
const MAX_GRAPHQL_DEFINITIONS: usize = 100_000;
const MAX_GRAPHQL_FIELDS: usize = 300_000;
const MAX_GRAPHQL_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_GRAPHQL_TEXT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    text.lines().any(|line| {
        let line = line.trim_start();
        [
            "type ",
            "interface ",
            "input ",
            "enum ",
            "scalar ",
            "union ",
            "schema {",
            "directive @",
        ]
        .iter()
        .any(|needle| line.starts_with(needle))
    })
}

struct GraphqlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for GraphqlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "graphql".into();
        if page.title.is_empty() {
            page.title = "GraphQL schema".into();
        }
        page.description = "GraphQL SDL definitions are rendered inertly; resolvers and operations are not executed".into();
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
        options.max_input_bytes.min(MAX_GRAPHQL_BYTES),
        "GraphQL SDL input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("GraphQL SDL must be UTF-8: {error}")))?;
    let (table, metadata, warnings) = parse_graphql(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "GraphQL schema".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = GraphqlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct Definition {
    kind: String,
    name: String,
    details: String,
    fields: Vec<Vec<String>>,
}

fn parse_graphql(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_GRAPHQL_BYTES || text.len() > MAX_GRAPHQL_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GraphQL SDL exceeds {MAX_GRAPHQL_BYTES} bytes"
        )));
    }
    let mut definitions = 0usize;
    let mut fields = 0usize;
    let mut rows = Vec::new();
    let mut current = None::<Definition>;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut pending = String::new();
    let mut unsupported = 0usize;
    for (line_number, raw) in text.lines().enumerate() {
        if line_number >= MAX_GRAPHQL_LINES {
            return Err(Error::LimitExceeded(format!(
                "GraphQL SDL exceeds {MAX_GRAPHQL_LINES} lines"
            )));
        }
        if raw.len() > MAX_GRAPHQL_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "GraphQL line {} exceeds {MAX_GRAPHQL_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = strip_comment(raw).trim().to_owned();
        if line.is_empty() || line.starts_with("\"\"\"") || line.starts_with('"') {
            continue;
        }
        let content = if pending.is_empty() {
            line
        } else {
            format!("{pending} {line}")
        };
        paren_depth += content
            .chars()
            .filter(|character| *character == '(')
            .count();
        paren_depth = paren_depth.saturating_sub(
            content
                .chars()
                .filter(|character| *character == ')')
                .count(),
        );
        if paren_depth > 0 {
            pending = content;
            continue;
        }
        pending.clear();
        if brace_depth == 0 {
            if let Some((kind, name, details, rest, opens, closes)) = definition_header(&content) {
                definitions += 1;
                if definitions > MAX_GRAPHQL_DEFINITIONS {
                    return Err(Error::LimitExceeded(format!(
                        "GraphQL definitions exceed {MAX_GRAPHQL_DEFINITIONS}"
                    )));
                }
                let mut definition = Definition {
                    kind,
                    name,
                    details,
                    fields: Vec::new(),
                };
                brace_depth = opens.saturating_sub(closes);
                if !rest.is_empty() {
                    fields += parse_fields(&rest, &mut definition, &mut rows)?;
                }
                if brace_depth == 0 {
                    flush_definition(&mut definition, &mut rows);
                } else {
                    current = Some(definition);
                }
                continue;
            }
            if let Some((kind, name, details)) = standalone_definition(&content) {
                definitions += 1;
                rows.push(vec![
                    truncate(&name),
                    kind,
                    String::new(),
                    truncate(&details),
                ]);
                continue;
            }
            if content.starts_with("extend ") || content.starts_with("schema ") {
                unsupported += 1;
            }
            continue;
        }
        let closes = content.matches('}').count();
        let opens = content.matches('{').count();
        let body = content.replace(['{', '}'], " ");
        if let Some(definition) = current.as_mut() {
            fields += parse_fields(&body, definition, &mut rows)?;
            brace_depth = brace_depth.saturating_add(opens).saturating_sub(closes);
            if brace_depth == 0 {
                let mut complete = current.take().expect("definition exists");
                flush_definition(&mut complete, &mut rows);
            }
        } else {
            brace_depth = brace_depth.saturating_add(opens).saturating_sub(closes);
        }
    }
    if current.is_some() || brace_depth != 0 || paren_depth != 0 {
        return Err(Error::InvalidInput(
            "GraphQL SDL has an unclosed definition or argument list".into(),
        ));
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "GraphQL SDL contains no type definitions or fields".into(),
        ));
    }
    let mut warnings = vec!["GraphQL SDL descriptions, directives, default values, URLs, schema extensions and custom scalars are displayed inertly; no introspection, resolver, query, mutation or subscription is executed".into()];
    if unsupported > 0 {
        warnings.push(format!(
            "{unsupported} GraphQL extension/unsupported top-level line(s) were omitted"
        ));
    }
    Ok((
        TableData {
            headers: vec![
                "Type".into(),
                "Kind".into(),
                "Field".into(),
                "Signature".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        },
        format!("Definitions: {definitions}\nFields: {fields}"),
        warnings,
    ))
}

fn definition_header(content: &str) -> Option<(String, String, String, String, usize, usize)> {
    let open = content.find('{');
    let (header, rest) = open.map_or((content, ""), |index| {
        (&content[..index], &content[index + 1..])
    });
    let mut tokens = header.split_whitespace();
    let mut kind = tokens.next()?.to_owned();
    if kind == "extend" {
        kind = format!("extend {}", tokens.next()?);
    }
    if !matches!(
        kind.as_str(),
        "type" | "interface" | "input" | "enum" | "schema"
    ) && !kind.starts_with("extend ")
    {
        return None;
    }
    let name = if kind == "schema" {
        "(schema)".to_owned()
    } else {
        tokens.next()?.to_owned()
    };
    let details = tokens.collect::<Vec<_>>().join(" ");
    Some((
        kind,
        name,
        details,
        rest.trim_end_matches('}').trim().to_owned(),
        content.matches('{').count(),
        content.matches('}').count(),
    ))
}

fn standalone_definition(content: &str) -> Option<(String, String, String)> {
    let mut tokens = content.split_whitespace();
    let kind = tokens.next()?;
    if kind == "scalar" || kind == "union" {
        return Some((
            kind.to_owned(),
            tokens.next()?.trim_start_matches('@').to_owned(),
            tokens.collect::<Vec<_>>().join(" "),
        ));
    }
    if kind == "directive" {
        let token = tokens.next()?.trim_start_matches('@');
        let (name, suffix) = token
            .split_once('(')
            .map_or((token, ""), |(name, rest)| (name, rest));
        let details = if suffix.is_empty() {
            tokens.collect::<Vec<_>>().join(" ")
        } else {
            format!("({suffix} {}", tokens.collect::<Vec<_>>().join(" "))
        };
        return Some((kind.to_owned(), name.to_owned(), details));
    }
    None
}

#[allow(clippy::ptr_arg)]
fn parse_fields(
    content: &str,
    definition: &mut Definition,
    rows: &mut Vec<Vec<String>>,
) -> Result<usize> {
    let mut count = 0usize;
    for line in content.lines() {
        let line = line.trim().trim_matches(',');
        if line.is_empty() || line.starts_with('@') || line.starts_with("\"\"") {
            continue;
        }
        if definition.kind == "enum" {
            let name = line.split_whitespace().next().unwrap_or_default();
            if !is_name(name) {
                continue;
            }
            definition.fields.push(vec![
                definition.name.clone(),
                definition.kind.clone(),
                name.to_owned(),
                truncate(line),
            ]);
            count += 1;
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let left = line[..colon].trim();
        let right = line[colon + 1..].trim();
        let field_name = left.split(['(', ' ', '=']).next().unwrap_or_default();
        if !is_name(field_name) || right.is_empty() {
            continue;
        }
        definition.fields.push(vec![
            definition.name.clone(),
            definition.kind.clone(),
            field_name.to_owned(),
            truncate(&format!("{left}: {right}")),
        ]);
        count += 1;
        if count > MAX_GRAPHQL_FIELDS
            || rows.len().saturating_add(definition.fields.len()) > MAX_GRAPHQL_FIELDS
        {
            return Err(Error::LimitExceeded(format!(
                "GraphQL fields exceed {MAX_GRAPHQL_FIELDS}"
            )));
        }
    }
    Ok(count)
}

fn flush_definition(definition: &mut Definition, rows: &mut Vec<Vec<String>>) {
    if definition.fields.is_empty() {
        rows.push(vec![
            truncate(&definition.name),
            definition.kind.clone(),
            String::new(),
            truncate(&definition.details),
        ]);
    } else {
        rows.append(&mut definition.fields);
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
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
        } else if character == '#' {
            return &line[..index];
        }
    }
    line
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_GRAPHQL_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_GRAPHQL_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn previews_graphql_types_and_fields_inertly() {
        let source = "type Query {\n  pets(limit: Int = 10): [Pet!]!\n}\ntype Pet implements Node {\n  id: ID!\n  name: String @deprecated(reason: \"old\")\n}\ninterface Node { id: ID! }\nenum Color { RED GREEN }\nscalar Date\ndirective @auth on FIELD_DEFINITION\n";
        let (table, metadata, warnings) = parse_graphql(source).unwrap();
        assert!(metadata.contains("Definitions"));
        assert!(table.rows.iter().any(|row| row[2] == "pets"));
        assert!(table.rows.iter().any(|row| row[1] == "scalar"));
        assert!(warnings.iter().any(|warning| warning.contains("executed")));
    }
    #[test]
    fn rejects_unclosed_definition() {
        assert!(parse_graphql("type Query { hello: String\n").is_err());
    }
}
