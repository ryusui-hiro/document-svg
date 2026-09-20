//! Bounded RDF Turtle/N-Triples/N-Quads table preview.
//!
//! This adapter displays RDF statements as inert subject/predicate/object
//! rows. Prefix declarations are shown as metadata, while inference, imports,
//! remote resolution, blank-node expansion, and SPARQL execution are omitted.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_TURTLE_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TURTLE_LINES: usize = 1_000_000;
const MAX_TURTLE_LINE_BYTES: usize = 1024 * 1024;
const MAX_TURTLE_TRIPLES: usize = 200_000;
const MAX_TURTLE_CELL_CHARS: usize = 512;
const MAX_TURTLE_TEXT_BYTES: usize = 64 * 1024 * 1024;
type ParsedRdf = (Vec<String>, Vec<Vec<String>>, bool, Vec<String>);

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.lines().take(64).any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("@prefix")
            || trimmed.starts_with("@base")
            || trimmed.starts_with("PREFIX ")
            || trimmed.starts_with("BASE ")
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_TURTLE_INPUT_BYTES),
        "RDF input",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("RDF input is not UTF-8: {error}")))?;
    let (prefixes, mut rows, has_graph, mut warnings) = parse_statements(text)?;
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "RDF input contains no renderable statements".into(),
        ));
    }
    if !prefixes.is_empty() {
        warnings.push(format!(
            "RDF prefix declarations are displayed without namespace expansion ({})",
            prefixes.len()
        ));
    }
    let mut headers = vec!["Subject".into(), "Predicate".into(), "Object".into()];
    if has_graph {
        headers.push("Graph".into());
        for row in &mut rows {
            row.resize(4, String::new());
        }
    }
    let table = TableData {
        headers,
        rows,
        alignments: vec![TableAlign::Left; if has_graph { 4 } else { 3 }],
        raw_source: String::new(),
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "RDF statements".into(),
        },
        HtmlBlock::Table(table),
    ];
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(dedup_warnings(warnings))
}

fn parse_statements(text: &str) -> Result<ParsedRdf> {
    let mut prefixes = Vec::new();
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut has_graph = false;
    let mut subject = None::<String>;
    let mut predicate = None::<String>;
    let mut text_bytes = 0usize;
    for (line_number, line) in text.lines().enumerate() {
        if line_number >= MAX_TURTLE_LINES {
            return Err(Error::LimitExceeded(format!(
                "RDF input exceeds {MAX_TURTLE_LINES} lines"
            )));
        }
        if line.len() > MAX_TURTLE_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "RDF line exceeds {MAX_TURTLE_LINE_BYTES} bytes"
            )));
        }
        text_bytes = text_bytes.saturating_add(line.len());
        if text_bytes > MAX_TURTLE_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "RDF text exceeds {MAX_TURTLE_TEXT_BYTES} bytes"
            )));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("@prefix")
            || trimmed.starts_with("@base")
            || trimmed.starts_with("PREFIX ")
            || trimmed.starts_with("BASE ")
        {
            prefixes.push(truncate(trimmed));
            continue;
        }
        let mut tokens = tokenize(trimmed);
        if subject.is_none()
            && !trimmed.contains(';')
            && !trimmed.contains(',')
            && tokens.len() >= 4
        {
            if tokens.last().is_some_and(|token| token == ".") {
                tokens.pop();
            }
            if tokens.len() >= 4 {
                if rows.len() >= MAX_TURTLE_TRIPLES {
                    return Err(Error::LimitExceeded(format!(
                        "RDF exceeds {MAX_TURTLE_TRIPLES} statements"
                    )));
                }
                has_graph = true;
                rows.push(
                    tokens
                        .into_iter()
                        .take(4)
                        .map(|value| truncate(&value))
                        .collect(),
                );
                continue;
            }
        }
        let mut delimiter = None::<char>;
        for token in tokens {
            let (term, token_delimiter) = split_delimiter(&token);
            if term.is_empty() && token_delimiter.is_some() {
                if token_delimiter == Some('.') {
                    subject = None;
                    predicate = None;
                }
                delimiter = token_delimiter;
                continue;
            }
            if delimiter.is_none() && subject.is_none() {
                subject = Some(term);
                delimiter = token_delimiter;
                continue;
            }
            if delimiter == Some('.') {
                subject = Some(term);
                delimiter = token_delimiter;
                continue;
            }
            if predicate.is_none() {
                predicate = Some(term);
                delimiter = token_delimiter;
                continue;
            }
            let Some(subject_value) = subject.clone() else {
                continue;
            };
            let Some(predicate_value) = predicate.clone() else {
                continue;
            };
            if rows.len() >= MAX_TURTLE_TRIPLES {
                return Err(Error::LimitExceeded(format!(
                    "RDF exceeds {MAX_TURTLE_TRIPLES} statements"
                )));
            }
            rows.push(vec![
                truncate(&subject_value),
                truncate(&predicate_value),
                truncate(&term),
            ]);
            match token_delimiter.or(delimiter) {
                Some('.') => {
                    subject = None;
                    predicate = None;
                    delimiter = None;
                }
                Some(';') => {
                    predicate = None;
                    delimiter = None;
                }
                Some(',') => {
                    delimiter = None;
                }
                _ => {
                    delimiter = None;
                }
            }
        }
        if let Some('.') = delimiter {
            subject = None;
            predicate = None;
        }
        if predicate.is_some() && subject.is_some() && !trimmed.ends_with('.') {
            warnings.push(format!(
                "RDF line {} continues a statement without a terminator",
                line_number + 1
            ));
        }
    }
    if subject.is_some() || predicate.is_some() {
        warnings.push("RDF input ended with an incomplete statement".into());
    }
    Ok((prefixes, rows, has_graph, warnings))
}

fn split_delimiter(token: &str) -> (String, Option<char>) {
    let Some(last) = token.chars().last() else {
        return (String::new(), None);
    };
    if matches!(last, '.' | ';' | ',') && !token.ends_with("...") {
        let mut term = token.to_owned();
        term.pop();
        return (term, Some(last));
    }
    (token.to_owned(), None)
}

fn tokenize(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None::<char>;
    let mut angle = false;
    for character in line.chars() {
        if let Some(delimiter) = quote {
            current.push(character);
            if character == delimiter {
                quote = None;
            }
        } else if angle {
            current.push(character);
            if character == '>' {
                angle = false;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
            current.push(character);
        } else if character == '<' {
            angle = true;
            current.push(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn truncate(value: &str) -> String {
    value.chars().take(MAX_TURTLE_CELL_CHARS).collect()
}

fn dedup_warnings(mut warnings: Vec<String>) -> Vec<String> {
    warnings.sort();
    warnings.dedup();
    warnings
}
