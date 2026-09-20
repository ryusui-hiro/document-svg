//! Bounded WADL REST-application description previews.
//!
//! WADL describes HTTP resources, methods and representations. This adapter
//! renders the description structure only; endpoints, schemas, credentials
//! and HTTP requests remain inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_WADL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_WADL_EVENTS: usize = 1_000_000;
const MAX_WADL_NODES: usize = 500_000;
const MAX_WADL_DEPTH: usize = 128;
const MAX_WADL_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_WADL_ROWS: usize = 200_000;
const MAX_WADL_DISPLAY_BYTES: usize = 512;
const WADL_NAMESPACE: &str = "http://wadl.dev.java.net/2009/02";

#[derive(Default)]
struct Summary {
    resources: usize,
    resource_types: usize,
    methods: usize,
    requests: usize,
    responses: usize,
    representations: usize,
    params: usize,
    grammars: usize,
    includes: usize,
    links: usize,
    docs: usize,
    rows: Vec<Vec<String>>,
}

struct WadlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for WadlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "wadl".into();
        if page.title.is_empty() {
            page.title = "WADL web application".into();
        }
        page.description = "WADL REST description structure is rendered as bounded inert metadata; HTTP endpoints and schemas are not accessed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"application", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("wadl.dev.java.net/2009/02")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_WADL_BYTES),
        "WADL input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_WADL_EVENTS),
            max_nodes: MAX_WADL_NODES,
            max_depth: MAX_WADL_DEPTH,
            max_text_bytes: MAX_WADL_TEXT_BYTES,
        },
        "WADL",
    )?;
    if !root.name.eq_ignore_ascii_case("application") {
        return Err(Error::InvalidInput(
            "WADL root must be <application>".into(),
        ));
    }
    if root.namespace.as_deref() != Some(WADL_NAMESPACE) {
        return Err(Error::InvalidInput(
            "WADL root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        resources: count_named(&root, "resource"),
        resource_types: count_named(&root, "resource_type"),
        methods: count_named(&root, "method"),
        requests: count_named(&root, "request"),
        responses: count_named(&root, "response"),
        representations: count_named(&root, "representation"),
        params: count_named(&root, "param"),
        grammars: count_named(&root, "grammars") + count_named(&root, "grammer"),
        includes: count_named(&root, "include"),
        links: count_named(&root, "link"),
        docs: count_named(&root, "doc"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Resources",
        &summary.resources.to_string(),
        &format!(
            "resourceTypes={} methods={}",
            summary.resource_types, summary.methods
        ),
    )?;
    push_row(
        &mut summary.rows,
        "HTTP",
        &summary.requests.to_string(),
        &format!(
            "responses={} representations={} params={}",
            summary.responses, summary.representations, summary.params
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Schemas",
        &summary.grammars.to_string(),
        &format!("includes={} links={}", summary.includes, summary.links),
    )?;
    push_row(
        &mut summary.rows,
        "Documentation",
        &summary.docs.to_string(),
        "titles and payloads omitted",
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "WADL web application".into() }, HtmlBlock::Paragraph { text: "Web Application Description Language resources and HTTP method structure are summarized without contacting endpoints.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["WADL resource identifiers, method names, parameters, representations, schema locations, documentation and endpoint URLs are omitted or redacted".into(), "WADL HTTP requests, external grammars, schemas, links, credentials and code generation never run".into()];
    let mut page_sink = WadlPageSink {
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
    if rows.len() >= MAX_WADL_ROWS {
        return Err(Error::LimitExceeded(format!(
            "WADL rows exceed {MAX_WADL_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_WADL_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_WADL_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
