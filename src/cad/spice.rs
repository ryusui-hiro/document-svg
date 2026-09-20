//! Bounded SPICE/ngspice netlist preview.
//!
//! Element cards and node names are shown as inert tables. Dot commands,
//! model/include paths, expressions, and control sections are never executed
//! or opened.

use std::collections::HashSet;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_SPICE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SPICE_LINES: usize = 2_000_000;
const MAX_SPICE_LINE_BYTES: usize = 1 << 20;
const MAX_SPICE_ELEMENTS: usize = 500_000;
const MAX_SPICE_NODES: usize = 1_000_000;
const MAX_SPICE_FIELD_BYTES: usize = 64 * 1024;
const MAX_SPICE_RENDERED_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    lines.len() >= 2
        && lines.iter().any(|line| line.eq_ignore_ascii_case(".end"))
        && lines.iter().skip(1).any(|line| is_element_line(line))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SPICE_BYTES),
        "SPICE input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("SPICE input must be UTF-8/ASCII: {error}"))
    })?;
    let (mut table, warnings) = parse_spice(&text)?;
    let mut page_sink = SpicePageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "spice", options, &mut page_sink)?;
    Ok(warnings)
}

struct SpicePageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SpicePageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "spice".into();
        page.title = "SPICE netlist".into();
        page.description = "SPICE element cards and node names are displayed inertly; no simulation or include is executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse_spice(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_SPICE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SPICE input exceeds {MAX_SPICE_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_SPICE_LINES {
        return Err(Error::LimitExceeded(format!(
            "SPICE input exceeds {MAX_SPICE_LINES} lines"
        )));
    }
    let mut logical = Vec::<(usize, String)>::new();
    let mut warnings = Vec::new();
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_SPICE_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "SPICE line {} exceeds {MAX_SPICE_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let trimmed = original.trim_end_matches('\r').trim_end();
        if trimmed.trim().is_empty()
            || trimmed.trim_start().starts_with('*')
            || trimmed.trim_start().starts_with(';')
        {
            continue;
        }
        if trimmed.trim_start().starts_with('+') {
            let continuation = trimmed
                .trim_start()
                .strip_prefix('+')
                .unwrap_or_default()
                .trim();
            let Some((_, previous)) = logical.last_mut() else {
                return Err(Error::InvalidInput(format!(
                    "SPICE continuation on line {} has no previous card",
                    line_number + 1
                )));
            };
            if !continuation.is_empty() {
                previous.push(' ');
                previous.push_str(continuation);
            }
            if previous.len() > MAX_SPICE_LINE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "SPICE logical line exceeds {MAX_SPICE_LINE_BYTES} bytes"
                )));
            }
        } else {
            let content = trimmed
                .split_once(';')
                .map(|(value, _)| value)
                .unwrap_or(trimmed)
                .trim();
            if !content.is_empty() {
                logical.push((line_number + 1, content.to_owned()));
            }
        }
    }
    if logical.is_empty() {
        return Err(Error::InvalidInput(
            "SPICE input contains no netlist cards".into(),
        ));
    }
    let title = logical
        .first()
        .map(|(_, line)| line.clone())
        .unwrap_or_default();
    let mut saw_end = false;
    let mut rows = Vec::new();
    let mut nodes = HashSet::new();
    let mut directives = 0usize;
    let mut element_count = 0usize;
    let mut in_control = false;
    for (index, (line_number, line)) in logical.iter().enumerate() {
        if index == 0 {
            continue;
        }
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let first = fields.first().copied().unwrap_or_default();
        if first.eq_ignore_ascii_case(".end") {
            saw_end = true;
            break;
        }
        if first.eq_ignore_ascii_case(".control") {
            in_control = true;
            directives += 1;
            continue;
        }
        if in_control {
            directives += 1;
            if first.eq_ignore_ascii_case(".endc") {
                in_control = false;
            }
            continue;
        }
        if first.starts_with('.') {
            directives += 1;
            if first.eq_ignore_ascii_case(".include")
                || first.eq_ignore_ascii_case(".lib")
                || first.eq_ignore_ascii_case(".inc")
            {
                warnings.push(format!(
                    "SPICE directive {first} on line {line_number} was not opened or executed"
                ));
            }
            continue;
        }
        if !is_element_line(line) {
            return Err(Error::InvalidInput(format!(
                "SPICE line {line_number} has an invalid element card"
            )));
        }
        if fields.len() < 3 {
            return Err(Error::InvalidInput(format!(
                "SPICE element on line {line_number} has too few fields"
            )));
        }
        element_count = element_count
            .checked_add(1)
            .ok_or_else(|| Error::LimitExceeded("SPICE element count overflowed".into()))?;
        if element_count > MAX_SPICE_ELEMENTS {
            return Err(Error::LimitExceeded(format!(
                "SPICE exceeds {MAX_SPICE_ELEMENTS} elements"
            )));
        }
        let kind = first
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .to_string();
        let node_count = required_node_count(kind.as_bytes()[0] as char);
        if fields.len() < 1 + node_count + 1 {
            return Err(Error::InvalidInput(format!(
                "SPICE element {first:?} on line {line_number} is missing nodes or value"
            )));
        }
        let node_slice = if kind == "X" && fields.len() > 3 {
            &fields[1..fields.len() - 1]
        } else {
            &fields[1..1 + node_count]
        };
        if node_slice.is_empty() {
            return Err(Error::InvalidInput(format!(
                "SPICE element {first:?} on line {line_number} has no nodes"
            )));
        }
        for node in node_slice {
            validate_field(node, "SPICE node")?;
            nodes.insert((*node).to_owned());
            if nodes.len() > MAX_SPICE_NODES {
                return Err(Error::LimitExceeded(format!(
                    "SPICE exceeds {MAX_SPICE_NODES} nodes"
                )));
            }
        }
        let value_start = if kind == "X" && fields.len() > 3 {
            fields.len() - 1
        } else {
            1 + node_count
        };
        let value = fields[value_start..].join(" ");
        validate_field(&value, "SPICE value/model")?;
        rows.push(vec![
            first.to_owned(),
            kind,
            node_slice.join(" "),
            value,
            line_number.to_string(),
        ]);
    }
    if !saw_end {
        warnings.push("SPICE netlist has no .END card; rendering stopped at EOF".into());
    }
    if directives > 0 {
        warnings.push("SPICE dot commands and model/control metadata were kept inert".into());
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "SPICE input contains no element cards".into(),
        ));
    }
    let mut rendered = title.len();
    for row in &rows {
        rendered = rendered
            .checked_add(row.iter().map(String::len).sum::<usize>())
            .ok_or_else(|| {
                Error::LimitExceeded("SPICE rendered text byte count overflowed".into())
            })?;
    }
    if rendered > MAX_SPICE_RENDERED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SPICE rendered text exceeds {MAX_SPICE_RENDERED_BYTES} bytes"
        )));
    }
    let headers = ["Element", "Type", "Nodes", "Value / model", "Source line"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let alignments = vec![
        TableAlign::Left,
        TableAlign::Center,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Right,
    ];
    Ok((
        TableData {
            headers,
            rows,
            alignments,
            raw_source: String::new(),
        },
        dedup_warnings(warnings),
    ))
}

fn is_element_line(line: &str) -> bool {
    let first = line.split_ascii_whitespace().next().unwrap_or_default();
    let mut chars = first.chars();
    chars
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && first.len() <= MAX_SPICE_FIELD_BYTES
}

fn required_node_count(kind: char) -> usize {
    match kind.to_ascii_uppercase() {
        'R' | 'C' | 'L' | 'V' | 'I' | 'D' | 'B' => 2,
        'Q' | 'J' => 3,
        'M' => 4,
        'E' | 'F' | 'G' | 'H' | 'T' | 'O' => 4,
        'K' => 2,
        'S' | 'W' => 3,
        _ => 2,
    }
}

fn validate_field(value: &str, context: &str) -> Result<()> {
    if value.len() > MAX_SPICE_FIELD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "{context} exceeds {MAX_SPICE_FIELD_BYTES} bytes"
        )));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(Error::InvalidInput(format!(
            "{context} contains a control character"
        )));
    }
    Ok(())
}

fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
