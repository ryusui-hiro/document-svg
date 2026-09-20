//! Bounded UCSC Wiggle (`.wig`) continuous-signal preview.
//!
//! The fixedStep and variableStep text forms are converted into inert interval
//! rows. Coordinates remain one-based and fully closed as defined by UCSC;
//! track/browser directives and remote track references are never evaluated.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_WIG_BYTES: u64 = 128 * 1024 * 1024;
const MAX_WIG_LINES: usize = 2_000_000;
const MAX_WIG_LINE_BYTES: usize = 1 << 20;
const MAX_WIG_VALUES: usize = 100_000;
const MAX_WIG_VALUE_BYTES: usize = 64 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines().map(str::trim).any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("fixedstep ")
            || lower.starts_with("variablestep ")
            || lower.contains("type=wiggle_0")
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_WIG_BYTES),
        "WIG input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("WIG input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse_wig(&text)?;
    let mut page_sink = WigPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "wig", options, &mut page_sink)?;
    Ok(warnings)
}

struct WigPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for WigPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "wig".into();
        page.title = "WIG continuous signal".into();
        page.description =
            "UCSC fixedStep/variableStep values are displayed as inert one-based closed intervals"
                .into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Clone, Debug)]
enum Mode {
    Fixed {
        chrom: String,
        next_start: u64,
        step: u64,
        span: u64,
    },
    Variable {
        chrom: String,
        span: u64,
        previous_start: Option<u64>,
    },
}

fn parse_wig(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_WIG_BYTES {
        return Err(Error::LimitExceeded(format!(
            "WIG input exceeds {MAX_WIG_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_WIG_LINES {
        return Err(Error::LimitExceeded(format!(
            "WIG input exceeds {MAX_WIG_LINES} lines"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut mode = None::<Mode>;
    let mut directives = false;
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_WIG_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "WIG line {} exceeds {MAX_WIG_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("track") || lower.starts_with("browser") {
            directives = true;
            continue;
        }
        if lower.starts_with("fixedstep") {
            let fields = parse_declaration(line, "fixedStep")?;
            let chrom = required_field(&fields, "chrom")?;
            let start = parse_positive(&fields, "start")?;
            let step = parse_positive_or_default(&fields, "step", 1)?;
            let span = parse_positive_or_default(&fields, "span", 1)?;
            mode = Some(Mode::Fixed {
                chrom,
                next_start: start,
                step,
                span,
            });
            continue;
        }
        if lower.starts_with("variablestep") {
            let fields = parse_declaration(line, "variableStep")?;
            let chrom = required_field(&fields, "chrom")?;
            let span = parse_positive_or_default(&fields, "span", 1)?;
            mode = Some(Mode::Variable {
                chrom,
                span,
                previous_start: None,
            });
            continue;
        }
        let active = mode.as_mut().ok_or_else(|| {
            Error::InvalidInput(format!(
                "WIG data line {} appears before fixedStep/variableStep declaration",
                line_number + 1
            ))
        })?;
        match active {
            Mode::Fixed {
                chrom,
                next_start,
                step,
                span,
            } => {
                for token in line.split_ascii_whitespace() {
                    let value = parse_signal(token, line_number + 1)?;
                    let start = *next_start;
                    let end = start.checked_add(*span - 1).ok_or_else(|| {
                        Error::LimitExceeded("WIG fixedStep coordinate overflowed".into())
                    })?;
                    rows.push(vec![
                        chrom.clone(),
                        start.to_string(),
                        end.to_string(),
                        value.to_string(),
                    ]);
                    *next_start = start.checked_add(*step).ok_or_else(|| {
                        Error::LimitExceeded("WIG fixedStep step overflowed".into())
                    })?;
                    if rows.len() > MAX_WIG_VALUES {
                        return Err(Error::LimitExceeded(format!(
                            "WIG exceeds {MAX_WIG_VALUES} values"
                        )));
                    }
                }
            }
            Mode::Variable {
                chrom,
                span,
                previous_start,
            } => {
                let tokens = line.split_ascii_whitespace().collect::<Vec<_>>();
                if tokens.len() != 2 {
                    return Err(Error::InvalidInput(format!(
                        "WIG variableStep line {} must contain position and value",
                        line_number + 1
                    )));
                }
                let start = tokens[0].parse::<u64>().map_err(|_| {
                    Error::InvalidInput(format!("WIG line {} position is invalid", line_number + 1))
                })?;
                if start == 0 {
                    return Err(Error::InvalidInput(format!(
                        "WIG line {} position must be one-based",
                        line_number + 1
                    )));
                }
                if previous_start.is_some_and(|previous| start <= previous) {
                    return Err(Error::InvalidInput(format!(
                        "WIG line {} variableStep positions are not increasing",
                        line_number + 1
                    )));
                }
                let end = start.checked_add(*span - 1).ok_or_else(|| {
                    Error::LimitExceeded("WIG variableStep coordinate overflowed".into())
                })?;
                let value = parse_signal(tokens[1], line_number + 1)?;
                rows.push(vec![
                    chrom.clone(),
                    start.to_string(),
                    end.to_string(),
                    value.to_string(),
                ]);
                *previous_start = Some(start);
                if rows.len() > MAX_WIG_VALUES {
                    return Err(Error::LimitExceeded(format!(
                        "WIG exceeds {MAX_WIG_VALUES} values"
                    )));
                }
            }
        }
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput("WIG contains no signal values".into()));
    }
    if directives {
        warnings.push("WIG track/browser directives were ignored".into());
    }
    let headers = ["chrom", "chromStart", "chromEnd", "dataValue"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let alignments = vec![
        TableAlign::Left,
        TableAlign::Right,
        TableAlign::Right,
        TableAlign::Right,
    ];
    Ok((
        TableData {
            headers,
            rows,
            alignments,
            raw_source: String::new(),
        },
        warnings,
    ))
}

fn parse_declaration(
    line: &str,
    keyword: &str,
) -> Result<std::collections::HashMap<String, String>> {
    let mut parts = line.split_ascii_whitespace();
    let found = parts.next().unwrap_or_default();
    if !found.eq_ignore_ascii_case(keyword) {
        return Err(Error::InvalidInput(format!(
            "WIG declaration {found:?} is invalid"
        )));
    }
    let mut fields = std::collections::HashMap::new();
    for token in parts {
        let Some((key, value)) = token.split_once('=') else {
            return Err(Error::InvalidInput(format!(
                "WIG declaration token {token:?} is invalid"
            )));
        };
        if key.is_empty() || value.is_empty() {
            return Err(Error::InvalidInput(
                "WIG declaration has an empty key/value".into(),
            ));
        }
        fields.insert(key.to_ascii_lowercase(), value.trim_matches('"').to_owned());
    }
    Ok(fields)
}

fn required_field(fields: &std::collections::HashMap<String, String>, key: &str) -> Result<String> {
    fields
        .get(key)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| Error::InvalidInput(format!("WIG declaration is missing {key}")))
}
fn parse_positive(fields: &std::collections::HashMap<String, String>, key: &str) -> Result<u64> {
    let value = fields
        .get(key)
        .ok_or_else(|| Error::InvalidInput(format!("WIG declaration is missing {key}")))?;
    let value = value
        .parse::<u64>()
        .map_err(|_| Error::InvalidInput(format!("WIG {key} is invalid")))?;
    if value == 0 {
        return Err(Error::InvalidInput(format!("WIG {key} must be positive")));
    }
    Ok(value)
}
fn parse_positive_or_default(
    fields: &std::collections::HashMap<String, String>,
    key: &str,
    default: u64,
) -> Result<u64> {
    match fields.get(key) {
        None => Ok(default),
        Some(value) => {
            let value = value
                .parse::<u64>()
                .map_err(|_| Error::InvalidInput(format!("WIG {key} is invalid")))?;
            if value == 0 {
                return Err(Error::InvalidInput(format!("WIG {key} must be positive")));
            }
            Ok(value)
        }
    }
}
fn parse_signal(value: &str, line: usize) -> Result<f64> {
    if value.len() > MAX_WIG_VALUE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "WIG line {line} value exceeds {MAX_WIG_VALUE_BYTES} bytes"
        )));
    }
    let value = value
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("WIG line {line} signal value is invalid")))?;
    if !value.is_finite() {
        return Err(Error::InvalidInput(format!(
            "WIG line {line} signal is non-finite"
        )));
    }
    Ok(value)
}
