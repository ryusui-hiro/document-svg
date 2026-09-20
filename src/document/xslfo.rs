//! Bounded XSL-FO formatting-object previews.
//!
//! XSL-FO is commonly used as a PDF/print-layout intermediate. This adapter
//! reports formatting-object structure only; it never resolves external
//! graphics, follows links or runs an FO formatter.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XSLFO_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XSLFO_EVENTS: usize = 1_000_000;
const MAX_XSLFO_NODES: usize = 500_000;
const MAX_XSLFO_DEPTH: usize = 128;
const MAX_XSLFO_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XSLFO_ROWS: usize = 200_000;
const MAX_XSLFO_DISPLAY_BYTES: usize = 512;
const XSLFO_NAMESPACE: &str = "http://www.w3.org/1999/XSL/Format";

#[derive(Default)]
struct Summary {
    page_sequences: usize,
    layout_masters: usize,
    page_masters: usize,
    flows: usize,
    static_content: usize,
    regions: usize,
    blocks: usize,
    inlines: usize,
    tables: usize,
    table_rows: usize,
    table_cells: usize,
    graphics: usize,
    links: usize,
    bookmarks: usize,
    page_numbers: usize,
    lists: usize,
    footnotes: usize,
    rows: Vec<Vec<String>>,
}

struct XslfoPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for XslfoPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xsl-fo".into();
        if page.title.is_empty() {
            page.title = "XSL-FO layout".into();
        }
        page.description = "XSL-FO formatting-object structure is rendered as bounded inert metadata; FO formatting and external resources are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    (text.contains("<fo:root") || text.contains("<root")) && text.contains("w3.org/1999/xsl/format")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XSLFO_BYTES),
        "XSL-FO input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XSLFO_EVENTS),
            max_nodes: MAX_XSLFO_NODES,
            max_depth: MAX_XSLFO_DEPTH,
            max_text_bytes: MAX_XSLFO_TEXT_BYTES,
        },
        "XSL-FO",
    )?;
    if !root.name.eq_ignore_ascii_case("root") {
        return Err(Error::InvalidInput("XSL-FO root must be fo:root".into()));
    }
    if root.namespace.as_deref() != Some(XSLFO_NAMESPACE) {
        return Err(Error::InvalidInput(
            "XSL-FO root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        page_sequences: count_named(&root, "page-sequence"),
        layout_masters: count_named(&root, "layout-master-set"),
        page_masters: count_named(&root, "simple-page-master")
            + count_named(&root, "page-sequence-master"),
        flows: count_named(&root, "flow"),
        static_content: count_named(&root, "static-content"),
        regions: count_named(&root, "region-body")
            + count_named(&root, "region-before")
            + count_named(&root, "region-after")
            + count_named(&root, "region-start")
            + count_named(&root, "region-end"),
        blocks: count_named(&root, "block") + count_named(&root, "block-container"),
        inlines: count_named(&root, "inline") + count_named(&root, "inline-container"),
        tables: count_named(&root, "table") + count_named(&root, "table-and-caption"),
        table_rows: count_named(&root, "table-row"),
        table_cells: count_named(&root, "table-cell"),
        graphics: count_named(&root, "external-graphic")
            + count_named(&root, "instream-foreign-object"),
        links: count_named(&root, "basic-link"),
        bookmarks: count_named(&root, "bookmark-tree") + count_named(&root, "bookmark"),
        page_numbers: count_named(&root, "page-number")
            + count_named(&root, "page-number-citation"),
        lists: count_named(&root, "list-block"),
        footnotes: count_named(&root, "footnote"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Pages",
        &summary.page_sequences.to_string(),
        &format!(
            "layoutMasters={} pageMasters={}",
            summary.layout_masters, summary.page_masters
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Flows",
        &summary.flows.to_string(),
        &format!(
            "staticContent={} regions={}",
            summary.static_content, summary.regions
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Text",
        &summary.blocks.to_string(),
        &format!(
            "inlines={} lists={} footnotes={}",
            summary.inlines, summary.lists, summary.footnotes
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Tables",
        &summary.tables.to_string(),
        &format!("rows={} cells={}", summary.table_rows, summary.table_cells),
    )?;
    push_row(
        &mut summary.rows,
        "Media",
        &summary.graphics.to_string(),
        &format!(
            "links={} bookmarks={} pageNumbers={}",
            summary.links, summary.bookmarks, summary.page_numbers
        ),
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "XSL-FO layout".into() }, HtmlBlock::Paragraph { text: "XSL Formatting Objects structure is summarized without running an FO formatter or loading external media.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["XSL-FO properties, text, coordinates, links, graphics, fonts and generated PDF output are omitted or redacted".into(), "XSL-FO external graphics, links, resource URLs, pagination, layout engines and PDF/PostScript formatting never run".into()];
    let mut page_sink = XslfoPageSink {
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
    if rows.len() >= MAX_XSLFO_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XSL-FO rows exceed {MAX_XSLFO_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_XSLFO_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XSLFO_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
