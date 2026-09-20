//! Bounded Fire Dynamics Simulator (FDS) input previews.
//!
//! FDS input uses Fortran-style namelist blocks. This adapter counts block
//! types and lines without executing FDS, reading meshes, resolving paths or
//! evaluating fire/flow equations.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_FDS_BYTES: u64 = 128 * 1024 * 1024;
const MAX_FDS_LINES: usize = 2_000_000;
const MAX_FDS_LINE_BYTES: usize = 1024 * 1024;
const MAX_FDS_BLOCKS: usize = 500_000;
const MAX_FDS_ROWS: usize = 200_000;
const MAX_FDS_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    lines: usize,
    blocks: usize,
    types: BTreeMap<String, usize>,
    rows: Vec<Vec<String>>,
}

struct FdsPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for FdsPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "fds".into();
        if page.title.is_empty() {
            page.title = "FDS input deck".into();
        }
        page.description = "Fire Dynamics Simulator namelist structure is rendered as bounded inert metadata; no CFD or fire calculation runs".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_uppercase();
    text.contains("&HEAD") && text.contains("CHID")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_FDS_BYTES),
        "FDS input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("FDS input must be UTF-8/ASCII: {error}")))?;
    let summary = parse(&text)?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "FDS input deck".into() }, HtmlBlock::Paragraph { text: "Fire Dynamics Simulator namelist blocks are summarized without executing fire/flow calculations or opening referenced files.".into() }, HtmlBlock::Table(TableData { headers: vec!["Block".into(), "Count".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["FDS block names are shown but CHID, paths, coordinates, fire parameters, device values and solver settings are omitted or redacted".into(), "FDS meshes, CSV/SMV output, external files, scripts, MPI execution and CFD simulation never run".into()];
    let mut page_sink = FdsPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<Summary> {
    let mut summary = Summary::default();
    for (line_no, raw) in text.lines().enumerate() {
        if line_no >= MAX_FDS_LINES {
            return Err(Error::LimitExceeded(format!(
                "FDS lines exceed {MAX_FDS_LINES}"
            )));
        }
        if raw.len() > MAX_FDS_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "FDS line exceeds {MAX_FDS_LINE_BYTES} bytes"
            )));
        }
        summary.lines = summary.lines.saturating_add(1);
        let line = raw.split('!').next().unwrap_or("").trim();
        if !line.starts_with('&') {
            continue;
        }
        let name = line[1..]
            .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
            .next()
            .unwrap_or("")
            .trim();
        if name.is_empty() {
            continue;
        }
        summary.blocks = summary.blocks.saturating_add(1);
        if summary.blocks > MAX_FDS_BLOCKS {
            return Err(Error::LimitExceeded(format!(
                "FDS blocks exceed {MAX_FDS_BLOCKS}"
            )));
        }
        *summary.types.entry(name.to_ascii_uppercase()).or_default() += 1;
    }
    if summary.blocks == 0 {
        return Err(Error::InvalidInput(
            "FDS input contains no namelist blocks".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Blocks",
        &summary.blocks.to_string(),
        &format!("lines={} types={}", summary.lines, summary.types.len()),
    )?;
    for (name, count) in summary.types.iter().take(MAX_FDS_ROWS.saturating_sub(1)) {
        push_row(
            &mut summary.rows,
            name,
            &count.to_string(),
            "values omitted",
        )?;
    }
    Ok(summary)
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_FDS_ROWS {
        return Err(Error::LimitExceeded(format!(
            "FDS rows exceed {MAX_FDS_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_FDS_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_FDS_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
