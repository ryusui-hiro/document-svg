//! Bounded GFF3/GTF genome-feature annotation preview.
//!
//! The nine tab-separated feature columns are rendered as an inert table. A
//! `##FASTA` tail, directives, and attribute links remain text/diagnostics; no
//! sequence analysis, URL fetch, or feature hierarchy evaluation is performed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_GFF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GFF_LINES: usize = 5_000_000;
const MAX_GFF_LINE_BYTES: usize = 1 << 20;
const MAX_GFF_FEATURES: usize = 100_000;
const MAX_GFF_ATTRIBUTES: usize = 256;
const MAX_GFF_ATTRIBUTE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FeatureFormat {
    Gff3,
    Gtf,
}

pub(crate) fn looks_like_gff3_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.to_ascii_lowercase().starts_with("##gff-version"))
}

pub(crate) fn looks_like_gtf_prefix(prefix: &[u8]) -> bool {
    looks_like_feature_prefix(prefix, true)
}

pub(crate) fn looks_like_feature_prefix(prefix: &[u8], gtf: bool) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 9 {
            continue;
        }
        if fields[0].is_empty() || fields[2].is_empty() || fields[8].is_empty() {
            continue;
        }
        if fields[3].parse::<usize>().is_err() || fields[4].parse::<usize>().is_err() {
            continue;
        }
        if gtf {
            return fields[8].contains("gene_id") || fields[8].contains("transcript_id");
        }
        return true;
    }
    false
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    format: FeatureFormat,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GFF_BYTES),
        "GFF/GTF input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("GFF/GTF input must be UTF-8/ASCII: {error}"))
    })?;
    let (mut table, warnings) = parse(&text, format)?;
    let format_name = match format {
        FeatureFormat::Gff3 => "gff3",
        FeatureFormat::Gtf => "gtf",
    };
    let mut page_sink = FeaturePageSink {
        inner: sink,
        format_name,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, format_name, options, &mut page_sink)?;
    Ok(warnings)
}

struct FeaturePageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    format_name: &'static str,
    warnings: &'a [String],
}

impl PageConsumer for FeaturePageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = self.format_name.into();
        page.title = format!(
            "{} feature annotations",
            self.format_name.to_ascii_uppercase()
        );
        page.description = "Genome feature coordinates and attributes are displayed inertly".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse(text: &str, format: FeatureFormat) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_GFF_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GFF/GTF input exceeds {MAX_GFF_BYTES} bytes"
        )));
    }
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut directives = false;
    let mut embedded_fasta = false;
    let mut seen_gff_version = false;
    let mut features = 0usize;
    for (line_number, original) in text.lines().enumerate() {
        if line_number >= MAX_GFF_LINES {
            return Err(Error::LimitExceeded(format!(
                "GFF/GTF exceeds {MAX_GFF_LINES} lines"
            )));
        }
        if original.len() > MAX_GFF_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "GFF/GTF line {} exceeds {MAX_GFF_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with("##FASTA") {
            embedded_fasta = true;
            break;
        }
        if line.starts_with('#') {
            directives = true;
            if line.to_ascii_lowercase().starts_with("##gff-version") {
                seen_gff_version = true;
            }
            continue;
        }
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() != 9 {
            return Err(Error::InvalidInput(format!(
                "GFF/GTF feature line {} must contain 9 tab-separated columns",
                line_number + 1
            )));
        }
        if features >= MAX_GFF_FEATURES {
            return Err(Error::LimitExceeded(format!(
                "GFF/GTF exceeds {MAX_GFF_FEATURES} features"
            )));
        }
        let seqid = columns[0];
        let source = columns[1];
        let feature_type = columns[2];
        if seqid.is_empty() || source.is_empty() || feature_type.is_empty() {
            return Err(Error::InvalidInput(format!(
                "GFF/GTF feature line {} has an empty identity column",
                line_number + 1
            )));
        }
        let start = parse_coordinate(columns[3], "start", line_number + 1)?;
        let end = parse_coordinate(columns[4], "end", line_number + 1)?;
        if start == 0 || end == 0 || start > end {
            return Err(Error::InvalidInput(format!(
                "GFF/GTF feature line {} has invalid coordinate range",
                line_number + 1
            )));
        }
        let score = if columns[5] == "." {
            ".".to_owned()
        } else {
            let value = columns[5].parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!(
                    "GFF/GTF feature line {} has invalid score",
                    line_number + 1
                ))
            })?;
            if !value.is_finite() {
                return Err(Error::InvalidInput("GFF/GTF score is non-finite".into()));
            }
            columns[5].to_owned()
        };
        if !matches!(columns[6], "+" | "-" | "." | "?") {
            return Err(Error::InvalidInput(format!(
                "GFF/GTF feature line {} has invalid strand",
                line_number + 1
            )));
        }
        if columns[7] != "." && !matches!(columns[7], "0" | "1" | "2") {
            return Err(Error::InvalidInput(format!(
                "GFF/GTF feature line {} has invalid phase",
                line_number + 1
            )));
        }
        let attributes = validate_attributes(columns[8], format)?;
        rows.push(vec![
            seqid.to_owned(),
            source.to_owned(),
            feature_type.to_owned(),
            start.to_string(),
            end.to_string(),
            score,
            columns[6].to_owned(),
            columns[7].to_owned(),
            attributes,
        ]);
        features += 1;
    }
    if features == 0 {
        return Err(Error::InvalidInput(
            "GFF/GTF contains no feature rows".into(),
        ));
    }
    if directives {
        warnings.push("GFF/GTF directives and comments were ignored".into());
    }
    if embedded_fasta {
        warnings.push(
            "embedded ##FASTA sequence data was omitted; only feature rows were rendered".into(),
        );
    }
    if format == FeatureFormat::Gff3 && !seen_gff_version {
        warnings.push(
            "GFF3 version directive was missing; nine-column feature rows were accepted".into(),
        );
    }
    let headers = [
        "seqid",
        "source",
        "type",
        "start",
        "end",
        "score",
        "strand",
        "phase",
        "attributes",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let alignments = vec![
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Left,
        TableAlign::Right,
        TableAlign::Right,
        TableAlign::Right,
        TableAlign::Center,
        TableAlign::Center,
        TableAlign::Left,
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

fn parse_coordinate(value: &str, name: &str, line: usize) -> Result<usize> {
    value.parse::<usize>().map_err(|_| {
        Error::InvalidInput(format!("GFF/GTF line {line} has invalid {name} coordinate"))
    })
}

fn validate_attributes(value: &str, format: FeatureFormat) -> Result<String> {
    if value.len() > MAX_GFF_ATTRIBUTE_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GFF/GTF attributes exceed {MAX_GFF_ATTRIBUTE_BYTES} bytes"
        )));
    }
    if value == "." {
        return Ok(String::new());
    }
    let pieces = value
        .split(';')
        .filter(|piece| !piece.trim().is_empty())
        .collect::<Vec<_>>();
    if pieces.len() > MAX_GFF_ATTRIBUTES {
        return Err(Error::LimitExceeded(format!(
            "GFF/GTF attributes exceed {MAX_GFF_ATTRIBUTES} entries"
        )));
    }
    for piece in &pieces {
        let valid = match format {
            FeatureFormat::Gff3 => piece.contains('='),
            FeatureFormat::Gtf => piece.split_whitespace().next().is_some_and(|key| {
                let trimmed = piece.trim_start();
                trimmed[key.len()..].trim_start().starts_with('"')
                    || trimmed[key.len()..].trim_start().starts_with("'")
            }),
        };
        if !valid {
            return Err(Error::InvalidInput(
                "GFF/GTF attribute entry is malformed".into(),
            ));
        }
        if piece
            .chars()
            .any(|character| character.is_control() && character != '\t')
        {
            return Err(Error::InvalidInput(
                "GFF/GTF attributes contain a control character".into(),
            ));
        }
    }
    Ok(value.to_owned())
}
