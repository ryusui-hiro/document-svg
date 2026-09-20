//! Bounded LCOV tracefile previews.
//!
//! LCOV tracefiles contain per-source-file coverage counters and line records.
//! This adapter keeps only safe basenames and aggregate counters; source paths,
//! function names, line execution records and compiler/runtime data remain inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_LCOV_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LCOV_LINES: usize = 500_000;
const MAX_LCOV_LINE_BYTES: usize = 1024 * 1024;
const MAX_LCOV_FILES: usize = 200_000;
const MAX_LCOV_STRING_BYTES: usize = 512 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    text.contains("SF:") && text.contains("DA:") && text.contains("end_of_record")
}

struct LcovPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for LcovPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "lcov".into();
        if page.title.is_empty() {
            page.title = "LCOV coverage".into();
        }
        page.description =
            "LCOV tracefile coverage counters are rendered as inert file summaries; source paths and execution payloads are omitted".into();
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
        options.max_input_bytes.min(MAX_LCOV_BYTES),
        "LCOV tracefile input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("LCOV tracefile must be UTF-8: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "LCOV coverage".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = LcovPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default, Clone, Copy)]
struct Counter {
    found: usize,
    hit: usize,
}

impl Counter {
    fn missed(self) -> usize {
        self.found.saturating_sub(self.hit)
    }

    fn text(self) -> String {
        format!("{}/{}", self.hit, self.missed())
    }

    fn add(&mut self, other: Self) {
        self.found = self.found.saturating_add(other.found);
        self.hit = self.hit.saturating_add(other.hit);
    }
}

#[derive(Default)]
struct FileRecord {
    path: String,
    functions: Counter,
    lines: Counter,
    branches: Counter,
    da_found: usize,
    da_hit: usize,
    br_found: usize,
    br_hit: usize,
}

impl FileRecord {
    fn finalize(&mut self) {
        if self.lines.found == 0 && self.da_found > 0 {
            self.lines = Counter {
                found: self.da_found,
                hit: self.da_hit,
            };
        }
        if self.branches.found == 0 && self.br_found > 0 {
            self.branches = Counter {
                found: self.br_found,
                hit: self.br_hit,
            };
        }
    }
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_LCOV_BYTES {
        return Err(Error::LimitExceeded(format!(
            "LCOV tracefile exceeds {MAX_LCOV_BYTES} bytes"
        )));
    }
    let mut records = Vec::new();
    let mut current: Option<FileRecord> = None;
    let mut test_names = 0usize;
    let mut malformed = 0usize;
    for (line_number, raw) in text.lines().enumerate() {
        if line_number >= MAX_LCOV_LINES {
            return Err(Error::LimitExceeded(format!(
                "LCOV tracefile exceeds {MAX_LCOV_LINES} lines"
            )));
        }
        if raw.len() > MAX_LCOV_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "LCOV tracefile line {} exceeds {MAX_LCOV_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("TN:") {
            if !value.trim().is_empty() {
                test_names = test_names.saturating_add(1);
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("SF:") {
            flush_record(&mut current, &mut records)?;
            if records.len() >= MAX_LCOV_FILES {
                return Err(Error::LimitExceeded(format!(
                    "LCOV files exceed {MAX_LCOV_FILES}"
                )));
            }
            current = Some(FileRecord {
                path: safe_basename(value),
                ..FileRecord::default()
            });
            continue;
        }
        if line == "end_of_record" {
            flush_record(&mut current, &mut records)?;
            continue;
        }
        let Some(record) = current.as_mut() else {
            continue;
        };
        if let Some(value) = line.strip_prefix("FNF:") {
            record.functions.found = parse_count(value, &mut malformed);
        } else if let Some(value) = line.strip_prefix("FNH:") {
            record.functions.hit = parse_count(value, &mut malformed);
        } else if let Some(value) = line.strip_prefix("LF:") {
            record.lines.found = parse_count(value, &mut malformed);
        } else if let Some(value) = line.strip_prefix("LH:") {
            record.lines.hit = parse_count(value, &mut malformed);
        } else if let Some(value) = line.strip_prefix("BRF:") {
            record.branches.found = parse_count(value, &mut malformed);
        } else if let Some(value) = line.strip_prefix("BRH:") {
            record.branches.hit = parse_count(value, &mut malformed);
        } else if let Some(value) = line.strip_prefix("DA:") {
            let mut fields = value.split(',');
            let _line_number = fields.next();
            let hits = fields.next().and_then(|value| value.parse::<u64>().ok());
            record.da_found = record.da_found.saturating_add(1);
            if hits.is_some_and(|hits| hits > 0) {
                record.da_hit = record.da_hit.saturating_add(1);
            }
        } else if let Some(value) = line.strip_prefix("BRDA:") {
            let taken = value.split(',').nth(3).map(str::trim);
            record.br_found = record.br_found.saturating_add(1);
            if taken.is_some_and(|taken| {
                taken != "-" && taken.parse::<u64>().is_ok_and(|hits| hits > 0)
            }) {
                record.br_hit = record.br_hit.saturating_add(1);
            }
        }
    }
    flush_record(&mut current, &mut records)?;
    if records.is_empty() {
        return Err(Error::InvalidInput(
            "LCOV tracefile contains no SF/end_of_record file sections".into(),
        ));
    }
    let mut totals = Counter::default();
    let mut functions = Counter::default();
    let mut branches = Counter::default();
    let mut rows = Vec::with_capacity(records.len());
    for record in records {
        totals.add(record.lines);
        functions.add(record.functions);
        branches.add(record.branches);
        rows.push(vec![
            record.path,
            record.lines.text(),
            record.functions.text(),
            record.branches.text(),
            rate(record.lines),
        ]);
    }
    let mut warnings = vec![
        "LCOV absolute/source paths are reduced to basenames; function names, line execution records, branch details, comments and runtime payloads are omitted; no tests or external resources are executed".into(),
        "LCOV counters are summarized without evaluating thresholds or merging tracefiles".into(),
    ];
    if malformed > 0 {
        warnings.push(format!(
            "{malformed} malformed LCOV counter value(s) were treated as zero"
        ));
    }
    let metadata = format!(
        "Files: {}\nTest names: {test_names}\nLines covered/missed: {}\nFunctions covered/missed: {}\nBranches covered/missed: {}\nLine coverage: {}",
        rows.len(),
        totals.text(),
        functions.text(),
        branches.text(),
        rate(totals)
    );
    Ok((
        TableData {
            headers: vec![
                "File".into(),
                "Lines C/M".into(),
                "Funcs C/M".into(),
                "Br C/M".into(),
                "Line%".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn flush_record(current: &mut Option<FileRecord>, records: &mut Vec<FileRecord>) -> Result<()> {
    let Some(mut record) = current.take() else {
        return Ok(());
    };
    record.finalize();
    records.push(record);
    Ok(())
}

fn parse_count(value: &str, malformed: &mut usize) -> usize {
    match value.trim().parse::<usize>() {
        Ok(value) => value,
        Err(_) => {
            *malformed = malformed.saturating_add(1);
            0
        }
    }
}

fn safe_basename(value: &str) -> String {
    let trimmed = value.trim();
    let basename = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    if basename.is_empty() {
        "(unnamed file)".into()
    } else {
        truncate(basename)
    }
}

fn rate(counter: Counter) -> String {
    if counter.found == 0 {
        return "—".into();
    }
    format!(
        "{:.1}%",
        (counter.hit as f64 / counter.found as f64) * 100.0
    )
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_LCOV_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_LCOV_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_lcov_tracefile() {
        assert!(looks_like_prefix(
            b"TN:test\nSF:/private/src/lib.rs\nDA:1,1\nend_of_record\n"
        ));
        assert!(!looks_like_prefix(b"TN:test\nSF:x\n"));
    }

    #[test]
    fn summarizes_counters_and_redacts_paths() {
        let (table, metadata, warnings) = parse(
            "TN:unit\nSF:/private/workspace/lib.rs\nFN:1,private_fn\nFNF:2\nFNH:1\nDA:1,3\nDA:2,0\nBRDA:1,0,0,1\nBRDA:2,0,1,-\nLF:2\nLH:1\nBRF:2\nBRH:1\nend_of_record\n",
        )
        .unwrap();
        assert!(metadata.contains("Files: 1"));
        assert_eq!(table.rows[0][0], "lib.rs");
        assert_eq!(table.rows[0][1], "1/1");
        assert_eq!(table.rows[0][2], "1/1");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("private"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("reduced")));
    }
}
