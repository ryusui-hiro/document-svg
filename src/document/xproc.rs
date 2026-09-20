//! Bounded XProc 3.0 XML-pipeline previews.
//!
//! XProc describes operations on XML documents. This adapter reports pipeline
//! structure only; steps, file/network operations, XPath and external imports
//! are never executed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XPROC_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XPROC_EVENTS: usize = 1_000_000;
const MAX_XPROC_NODES: usize = 500_000;
const MAX_XPROC_DEPTH: usize = 128;
const MAX_XPROC_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XPROC_ROWS: usize = 200_000;
const MAX_XPROC_DISPLAY_BYTES: usize = 512;
const XPROC_NAMESPACE: &str = "http://www.w3.org/ns/xproc";

#[derive(Default)]
struct Summary {
    declarations: usize,
    steps: usize,
    inputs: usize,
    outputs: usize,
    options: usize,
    variables: usize,
    pipes: usize,
    conditionals: usize,
    loops: usize,
    groups: usize,
    errors: usize,
    imports: usize,
    includes: usize,
    file_ops: usize,
    http_ops: usize,
    rows: Vec<Vec<String>>,
}

struct XprocPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for XprocPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xproc".into();
        if page.title.is_empty() {
            page.title = "XProc pipeline".into();
        }
        page.description = "XProc pipeline structure is rendered as bounded inert metadata; pipeline steps and external resources are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    (text.contains("<p:declare-step") || text.contains("<p:pipeline"))
        && text.contains("www.w3.org/ns/xproc")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XPROC_BYTES),
        "XProc input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XPROC_EVENTS),
            max_nodes: MAX_XPROC_NODES,
            max_depth: MAX_XPROC_DEPTH,
            max_text_bytes: MAX_XPROC_TEXT_BYTES,
        },
        "XProc",
    )?;
    if !root.name.eq_ignore_ascii_case("declare-step")
        && !root.name.eq_ignore_ascii_case("pipeline")
    {
        return Err(Error::InvalidInput(
            "XProc root must be p:declare-step or p:pipeline".into(),
        ));
    }
    if root.namespace.as_deref() != Some(XPROC_NAMESPACE) {
        return Err(Error::InvalidInput(
            "XProc root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        declarations: usize::from(root.name.eq_ignore_ascii_case("declare-step")),
        steps: count_named(&root, "declare-step")
            + count_named(&root, "pipeline")
            + count_named(&root, "step")
            + count_named(&root, "identity"),
        inputs: count_named(&root, "input") + count_named(&root, "with-input"),
        outputs: count_named(&root, "output") + count_named(&root, "with-output"),
        options: count_named(&root, "option"),
        variables: count_named(&root, "variable"),
        pipes: count_named(&root, "pipe"),
        conditionals: count_named(&root, "choose")
            + count_named(&root, "when")
            + count_named(&root, "if"),
        loops: count_named(&root, "for-each") + count_named(&root, "viewport"),
        groups: count_named(&root, "group") + count_named(&root, "try"),
        errors: count_named(&root, "catch") + count_named(&root, "when-error"),
        imports: count_named(&root, "import"),
        includes: count_named(&root, "include"),
        file_ops: count_named(&root, "load") + count_named(&root, "store"),
        http_ops: count_named(&root, "http-request"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Pipeline",
        &summary.steps.to_string(),
        &format!(
            "declarations={} inputs={} outputs={}",
            summary.declarations, summary.inputs, summary.outputs
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Bindings",
        &summary.options.to_string(),
        &format!("variables={} pipes={}", summary.variables, summary.pipes),
    )?;
    push_row(
        &mut summary.rows,
        "Control",
        &summary.conditionals.to_string(),
        &format!(
            "loops={} groups={} errors={}",
            summary.loops, summary.groups, summary.errors
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Resources",
        &summary.file_ops.to_string(),
        &format!(
            "httpOps={} imports={} includes={}",
            summary.http_ops, summary.imports, summary.includes
        ),
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "XProc pipeline".into() }, HtmlBlock::Paragraph { text: "XProc XML pipeline declarations are summarized without running steps, file operations, HTTP requests or XPath.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["XProc step names, options, XPath expressions, file paths, HTTP URLs, variables and XML payloads are omitted or redacted".into(), "XProc load/store/http-request, pipeline imports/includes, extension steps, scripts and external resources never run".into()];
    let mut page_sink = XprocPageSink {
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
    if rows.len() >= MAX_XPROC_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XProc rows exceed {MAX_XPROC_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_XPROC_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XPROC_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
