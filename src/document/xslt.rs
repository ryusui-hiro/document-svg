//! Bounded W3C XSLT stylesheet previews.
//!
//! XSLT stylesheets contain executable template rules. This adapter parses
//! only the stylesheet structure and never evaluates XPath, calls extension
//! functions, reads documents or writes transformed output.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XSLT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XSLT_EVENTS: usize = 1_000_000;
const MAX_XSLT_NODES: usize = 500_000;
const MAX_XSLT_DEPTH: usize = 128;
const MAX_XSLT_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XSLT_ROWS: usize = 200_000;
const MAX_XSLT_DISPLAY_BYTES: usize = 512;
const XSLT_NAMESPACE: &str = "http://www.w3.org/1999/XSL/Transform";

#[derive(Default)]
struct Summary {
    version: String,
    templates: usize,
    apply_templates: usize,
    call_templates: usize,
    variables: usize,
    params: usize,
    for_each: usize,
    conditions: usize,
    choices: usize,
    keys: usize,
    includes: usize,
    imports: usize,
    outputs: usize,
    scripts: usize,
    functions: usize,
    rows: Vec<Vec<String>>,
}

struct XsltPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for XsltPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xslt".into();
        if page.title.is_empty() {
            page.title = "XSLT stylesheet".into();
        }
        page.description = "XSLT stylesheet structure is rendered as bounded inert metadata; XPath and template execution are disabled".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    (text.contains("<xsl:stylesheet")
        || text.contains("<xsl:transform")
        || text.contains("<stylesheet"))
        && text.contains("w3.org/1999/xsl/transform")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XSLT_BYTES),
        "XSLT input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XSLT_EVENTS),
            max_nodes: MAX_XSLT_NODES,
            max_depth: MAX_XSLT_DEPTH,
            max_text_bytes: MAX_XSLT_TEXT_BYTES,
        },
        "XSLT",
    )?;
    if !root.name.eq_ignore_ascii_case("stylesheet") && !root.name.eq_ignore_ascii_case("transform")
    {
        return Err(Error::InvalidInput(
            "XSLT root must be xsl:stylesheet or xsl:transform".into(),
        ));
    }
    if root.namespace.as_deref() != Some(XSLT_NAMESPACE) {
        return Err(Error::InvalidInput(
            "XSLT root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        version: root.attribute("version").map(truncate).unwrap_or_default(),
        templates: count_named(&root, "template"),
        apply_templates: count_named(&root, "apply-templates"),
        call_templates: count_named(&root, "call-template"),
        variables: count_named(&root, "variable"),
        params: count_named(&root, "param"),
        for_each: count_named(&root, "for-each"),
        conditions: count_named(&root, "if") + count_named(&root, "when"),
        choices: count_named(&root, "choose"),
        keys: count_named(&root, "key"),
        includes: count_named(&root, "include"),
        imports: count_named(&root, "import"),
        outputs: count_named(&root, "output"),
        scripts: count_named(&root, "script"),
        functions: count_named(&root, "function"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Stylesheet",
        &summary.version,
        &format!(
            "templates={} applyTemplates={}",
            summary.templates, summary.apply_templates
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Control",
        &summary.for_each.to_string(),
        &format!(
            "conditions={} choices={} calls={}",
            summary.conditions, summary.choices, summary.call_templates
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Bindings",
        &summary.variables.to_string(),
        &format!("params={} keys={}", summary.params, summary.keys),
    )?;
    push_row(
        &mut summary.rows,
        "Dependencies",
        &summary.imports.to_string(),
        &format!(
            "includes={} outputs={} scripts={} functions={}",
            summary.includes, summary.outputs, summary.scripts, summary.functions
        ),
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "XSLT stylesheet".into() }, HtmlBlock::Paragraph { text: "XSL Transformations stylesheet structure is summarized without evaluating XPath, templates or external documents.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["XSLT match/select expressions, variables, parameters, URLs, scripts and generated output are omitted or redacted".into(), "XSLT templates, XPath, document()/collection() access, extension functions, include/import resources and transformation execution never run".into()];
    let mut page_sink = XsltPageSink {
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
    if rows.len() >= MAX_XSLT_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XSLT rows exceed {MAX_XSLT_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_XSLT_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XSLT_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
