//! Bounded Variant Call Format (VCF) variant-table preview.
//!
//! VCF metadata, fixed variant columns and sample fields are displayed as
//! inert text. Reference URLs, genotype interpretation, phasing and variant
//! normalization are never performed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData, convert_table_pages};

const MAX_VCF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_VCF_LINES: usize = 2_000_000;
const MAX_VCF_LINE_BYTES: usize = 1 << 20;
const MAX_VCF_VARIANTS: usize = 100_000;
const MAX_VCF_COLUMNS: usize = 256;
const MAX_VCF_VALUE_BYTES: usize = 64 * 1024;
const MAX_VCF_CELLS: usize = 2_000_000;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim_start();
        line.to_ascii_lowercase().starts_with("##fileformat=vcf") || line.starts_with("#CHROM\t")
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_VCF_BYTES),
        "VCF input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("VCF input must be UTF-8/ASCII: {error}")))?;
    let (mut table, warnings) = parse_vcf(&text)?;
    let mut page_sink = VcfPageSink {
        inner: sink,
        warnings: &warnings,
    };
    convert_table_pages(&mut table, "vcf", options, &mut page_sink)?;
    Ok(warnings)
}

struct VcfPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for VcfPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "vcf".into();
        page.title = "VCF variant annotations".into();
        page.description = "Variant and sample fields are displayed inertly; no reference or genotype analysis is performed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

fn parse_vcf(text: &str) -> Result<(TableData, Vec<String>)> {
    if text.len() as u64 > MAX_VCF_BYTES {
        return Err(Error::LimitExceeded(format!(
            "VCF input exceeds {MAX_VCF_BYTES} bytes"
        )));
    }
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() > MAX_VCF_LINES {
        return Err(Error::LimitExceeded(format!(
            "VCF input exceeds {MAX_VCF_LINES} lines"
        )));
    }
    let mut headers = None::<Vec<String>>;
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut metadata_seen = false;
    let mut fileformat_seen = false;
    let mut embedded_fasta = false;
    let mut cells = 0usize;
    for (line_number, original) in lines.iter().enumerate() {
        if original.len() > MAX_VCF_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "VCF line {} exceeds {MAX_VCF_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = original.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.starts_with("##FASTA") {
            embedded_fasta = true;
            break;
        }
        if line.starts_with("##") {
            metadata_seen = true;
            if line.to_ascii_lowercase().starts_with("##fileformat=vcf") {
                fileformat_seen = true;
            }
            continue;
        }
        if line.starts_with('#') {
            if headers.is_some() {
                return Err(Error::InvalidInput(format!(
                    "VCF has duplicate column header at line {}",
                    line_number + 1
                )));
            }
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() < 8
                || fields.len() > MAX_VCF_COLUMNS
                || fields[..8]
                    != [
                        "#CHROM", "POS", "ID", "REF", "ALT", "QUAL", "FILTER", "INFO",
                    ]
            {
                return Err(Error::InvalidInput(format!(
                    "VCF column header at line {} is invalid",
                    line_number + 1
                )));
            }
            headers = Some(fields.into_iter().map(str::to_owned).collect());
            continue;
        }
        let header = headers
            .as_ref()
            .ok_or_else(|| Error::InvalidInput("VCF data appears before #CHROM header".into()))?;
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != header.len() {
            return Err(Error::InvalidInput(format!(
                "VCF line {} has {} columns; expected {}",
                line_number + 1,
                fields.len(),
                header.len()
            )));
        }
        if rows.len() >= MAX_VCF_VARIANTS {
            return Err(Error::LimitExceeded(format!(
                "VCF exceeds {MAX_VCF_VARIANTS} variants"
            )));
        }
        if fields[0].is_empty()
            || fields[2].is_empty()
            || fields[3].is_empty()
            || fields[4].is_empty()
        {
            return Err(Error::InvalidInput(format!(
                "VCF line {} has an empty required field",
                line_number + 1
            )));
        }
        let pos = fields[1].parse::<u64>().map_err(|_| {
            Error::InvalidInput(format!("VCF line {} POS is invalid", line_number + 1))
        })?;
        if pos == 0 {
            return Err(Error::InvalidInput(format!(
                "VCF line {} POS must be 1-based",
                line_number + 1
            )));
        }
        if fields[5] != "." {
            let qual = fields[5].parse::<f64>().map_err(|_| {
                Error::InvalidInput(format!("VCF line {} QUAL is invalid", line_number + 1))
            })?;
            if !qual.is_finite() {
                return Err(Error::InvalidInput("VCF QUAL is non-finite".into()));
            }
        }
        for field in &fields {
            if field.len() > MAX_VCF_VALUE_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "VCF line {} field exceeds {MAX_VCF_VALUE_BYTES} bytes",
                    line_number + 1
                )));
            }
            if field.chars().any(char::is_control) {
                return Err(Error::InvalidInput(format!(
                    "VCF line {} contains a control character",
                    line_number + 1
                )));
            }
        }
        cells = cells
            .checked_add(fields.len())
            .ok_or_else(|| Error::LimitExceeded("VCF cell count overflowed".into()))?;
        if cells > MAX_VCF_CELLS {
            return Err(Error::LimitExceeded(format!(
                "VCF exceeds {MAX_VCF_CELLS} cells"
            )));
        }
        rows.push(fields.into_iter().map(str::to_owned).collect());
    }
    let headers =
        headers.ok_or_else(|| Error::InvalidInput("VCF #CHROM header is missing".into()))?;
    if rows.is_empty() {
        return Err(Error::InvalidInput("VCF contains no variant rows".into()));
    }
    if metadata_seen {
        warnings.push(
            "VCF metadata directives were ignored; reference URLs and descriptions were not loaded"
                .into(),
        );
    }
    if embedded_fasta {
        warnings.push("embedded VCF ##FASTA sequence data was omitted".into());
    }
    if !fileformat_seen {
        warnings
            .push("VCF ##fileformat directive was missing; the tabular header was accepted".into());
    }
    let alignments = (0..headers.len())
        .map(|index| {
            if matches!(index, 1 | 5) {
                TableAlign::Right
            } else {
                TableAlign::Left
            }
        })
        .collect();
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
