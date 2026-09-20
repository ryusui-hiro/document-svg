//! Bounded AbiWord AWML document previews.
//!
//! AbiWord's native `.abw` format is a single XML document. This adapter
//! reports document structure without exposing paragraph text, fields, links,
//! images or embedded payloads.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_ABW_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ABW_EVENTS: usize = 1_000_000;
const MAX_ABW_NODES: usize = 500_000;
const MAX_ABW_DEPTH: usize = 128;
const MAX_ABW_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ABW_ROWS: usize = 200_000;
const MAX_ABW_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    sections: usize,
    paragraphs: usize,
    characters: usize,
    tables: usize,
    cells: usize,
    images: usize,
    bookmarks: usize,
    hyperlinks: usize,
    fields: usize,
    equations: usize,
    rows: Vec<Vec<String>>,
}

struct AbiwordPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for AbiwordPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "abiword".into();
        if page.title.is_empty() {
            page.title = "AbiWord document".into();
        }
        page.description = "AbiWord AWML structure is rendered as bounded inert metadata; document text and embedded resources are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"abiword", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("abiword")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ABW_BYTES),
        "AbiWord input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_ABW_EVENTS),
            max_nodes: MAX_ABW_NODES,
            max_depth: MAX_ABW_DEPTH,
            max_text_bytes: MAX_ABW_TEXT_BYTES,
        },
        "AbiWord",
    )?;
    if !root.name.eq_ignore_ascii_case("abiword") {
        return Err(Error::InvalidInput("AbiWord root must be <abiword>".into()));
    }
    let mut summary = Summary {
        version: root
            .attribute("fileformat")
            .or_else(|| root.attribute("version"))
            .map(truncate)
            .unwrap_or_default(),
        sections: count_named(&root, "section"),
        paragraphs: count_named(&root, "p"),
        characters: count_named(&root, "c"),
        tables: count_named(&root, "table"),
        cells: count_named(&root, "cell"),
        images: count_named(&root, "image"),
        bookmarks: count_named(&root, "bookmark"),
        hyperlinks: count_named(&root, "hyperlink") + count_named(&root, "link"),
        fields: count_named(&root, "field"),
        equations: count_named(&root, "math") + count_named(&root, "equation"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Document",
        &summary.version,
        &format!(
            "sections={} paragraphs={}",
            summary.sections, summary.paragraphs
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Text",
        &summary.characters.to_string(),
        &format!("tables={} cells={}", summary.tables, summary.cells),
    )?;
    push_row(
        &mut summary.rows,
        "Resources",
        &summary.images.to_string(),
        &format!(
            "bookmarks={} hyperlinks={} fields={}",
            summary.bookmarks, summary.hyperlinks, summary.fields
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Equations",
        &summary.equations.to_string(),
        "text and field values omitted",
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "AbiWord document".into() }, HtmlBlock::Paragraph { text: "AbiWord XML document structure is summarized without displaying private text, links, fields or embedded resources.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["AbiWord paragraph/character text, style values, fields, links, image bytes and embedded objects are omitted or redacted".into(), "AWML DTD/entities, hyperlinks, fields, scripts, embedded resources and external files are never loaded or executed".into()];
    let mut page_sink = AbiwordPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| usize::from(child.name.eq_ignore_ascii_case(name)) + count_named(child, name))
        .sum()
}
fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_ABW_ROWS {
        return Err(Error::LimitExceeded(format!(
            "AbiWord rows exceed {MAX_ABW_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_ABW_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_ABW_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
