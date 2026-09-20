//! Bounded ACIS SAT ASCII solid-model previews.
//!
//! ACIS SAT is a text exchange format for solids and surfaces. This adapter
//! intentionally records entity inventory only; NURBS geometry, topology,
//! attributes and external references are not tessellated or evaluated.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_SAT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SAT_LINES: usize = 2_000_000;
const MAX_SAT_LINE_BYTES: usize = 1024 * 1024;
const MAX_SAT_ENTITIES: usize = 500_000;
const MAX_SAT_ROWS: usize = 200_000;
const MAX_SAT_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    header: String,
    lines: usize,
    entities: usize,
    types: BTreeMap<String, usize>,
    rows: Vec<Vec<String>>,
}

struct SatPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SatPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "sat".into();
        if page.title.is_empty() {
            page.title = "ACIS SAT model".into();
        }
        page.description =
            "ACIS SAT entity inventory is rendered as bounded inert metadata; geometry is not tessellated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    (text.contains("acis") || text.contains("sat")) && text.lines().skip(1).any(is_entity_line)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SAT_BYTES),
        "ACIS SAT input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("ACIS SAT must be UTF-8/ASCII text: {error}"))
    })?;
    let summary = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "ACIS SAT model".into(),
        },
        HtmlBlock::Paragraph {
            text: "ACIS SAT solid/surface entity structure is summarized without tessellating or exposing geometry payloads.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Entity".into(), "Count".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "ACIS SAT coordinates, NURBS coefficients, topology links, attributes, colors, IDs and model values are omitted or redacted; only entity counts are shown".into(),
        "ACIS tessellation, healing, Boolean operations, external references and SAB/binary payloads never run".into(),
    ];
    let mut page_sink = SatPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<Summary> {
    let mut summary = Summary::default();
    for (line_no, line) in text.lines().enumerate() {
        if line_no >= MAX_SAT_LINES {
            return Err(Error::LimitExceeded(format!(
                "ACIS SAT lines exceed {MAX_SAT_LINES}"
            )));
        }
        if line.len() > MAX_SAT_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "ACIS SAT line exceeds {MAX_SAT_LINE_BYTES} bytes"
            )));
        }
        if line_no == 0 {
            summary.header = truncate(line.trim());
            continue;
        }
        if let Some(kind) = entity_kind(line) {
            summary.entities = summary.entities.saturating_add(1);
            if summary.entities > MAX_SAT_ENTITIES {
                return Err(Error::LimitExceeded(format!(
                    "ACIS SAT entities exceed {MAX_SAT_ENTITIES}"
                )));
            }
            *summary.types.entry(kind.to_owned()).or_default() += 1;
        }
        summary.lines = summary.lines.saturating_add(1);
    }
    if summary.entities == 0 {
        return Err(Error::InvalidInput(
            "ACIS SAT contains no supported entity records".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Model",
        &summary.entities.to_string(),
        &format!("lines={} header={}", summary.lines, summary.header),
    )?;
    for (kind, count) in summary.types.iter().take(MAX_SAT_ROWS.saturating_sub(1)) {
        push_row(
            &mut summary.rows,
            kind,
            &count.to_string(),
            "geometry omitted",
        )?;
    }
    Ok(summary)
}

fn is_entity_line(line: &str) -> bool {
    entity_kind(line).is_some()
}

fn entity_kind(line: &str) -> Option<&str> {
    let token = line.split_whitespace().next()?;
    let token = token.trim_matches(|ch: char| !ch.is_ascii_alphabetic() && ch != '_');
    matches!(
        token.to_ascii_lowercase().as_str(),
        "body"
            | "solid"
            | "lump"
            | "shell"
            | "face"
            | "loop"
            | "coedge"
            | "edge"
            | "vertex"
            | "plane"
            | "curve"
            | "surface"
            | "transform"
            | "wire"
            | "point"
    )
    .then_some(token)
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_SAT_ROWS {
        return Err(Error::LimitExceeded(format!(
            "ACIS SAT rows exceed {MAX_SAT_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_SAT_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_SAT_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
