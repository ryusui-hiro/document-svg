//! Bounded W3C XML Schema (XSD) previews.
//!
//! XSD documents define elements and type components and may import other
//! schemas. This adapter reports schema structure without fetching imports,
//! validating instance documents or exposing schema documentation payloads.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XSD_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XSD_EVENTS: usize = 1_000_000;
const MAX_XSD_NODES: usize = 500_000;
const MAX_XSD_DEPTH: usize = 128;
const MAX_XSD_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XSD_ROWS: usize = 200_000;
const MAX_XSD_DISPLAY_BYTES: usize = 512;
const XSD_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";

#[derive(Default)]
struct Summary {
    elements: usize,
    attributes: usize,
    complex_types: usize,
    simple_types: usize,
    groups: usize,
    attribute_groups: usize,
    choices: usize,
    sequences: usize,
    restrictions: usize,
    extensions: usize,
    imports: usize,
    includes: usize,
    redefines: usize,
    overrides: usize,
    annotations: usize,
    rows: Vec<Vec<String>>,
}

struct XsdPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for XsdPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xsd".into();
        if page.title.is_empty() {
            page.title = "XML Schema definition".into();
        }
        page.description = "W3C XML Schema structure is rendered as bounded inert metadata; imports and validation are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"schema", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("www.w3.org/2001/xmlschema")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XSD_BYTES),
        "XSD input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XSD_EVENTS),
            max_nodes: MAX_XSD_NODES,
            max_depth: MAX_XSD_DEPTH,
            max_text_bytes: MAX_XSD_TEXT_BYTES,
        },
        "XSD",
    )?;
    if !root.name.eq_ignore_ascii_case("schema") {
        return Err(Error::InvalidInput("XSD root must be <schema>".into()));
    }
    if root.namespace.as_deref() != Some(XSD_NAMESPACE) {
        return Err(Error::InvalidInput(
            "XSD root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        elements: count_named(&root, "element"),
        attributes: count_named(&root, "attribute"),
        complex_types: count_named(&root, "complexType"),
        simple_types: count_named(&root, "simpleType"),
        groups: count_named(&root, "group"),
        attribute_groups: count_named(&root, "attributeGroup"),
        choices: count_named(&root, "choice"),
        sequences: count_named(&root, "sequence"),
        restrictions: count_named(&root, "restriction"),
        extensions: count_named(&root, "extension"),
        imports: count_named(&root, "import"),
        includes: count_named(&root, "include"),
        redefines: count_named(&root, "redefine"),
        overrides: count_named(&root, "override"),
        annotations: count_named(&root, "annotation") + count_named(&root, "documentation"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Declarations",
        &summary.elements.to_string(),
        &format!(
            "attributes={} complexTypes={} simpleTypes={}",
            summary.attributes, summary.complex_types, summary.simple_types
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Compositors",
        &summary.sequences.to_string(),
        &format!(
            "choices={} groups={} attributeGroups={}",
            summary.choices, summary.groups, summary.attribute_groups
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Derivation",
        &summary.restrictions.to_string(),
        &format!(
            "extensions={} annotations={}",
            summary.extensions, summary.annotations
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Dependencies",
        &format!("imports={}", summary.imports),
        &format!(
            "includes={} redefines={} overrides={}",
            summary.includes, summary.redefines, summary.overrides
        ),
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "XML Schema definition".into() }, HtmlBlock::Paragraph { text: "W3C XML Schema declarations and type structure are summarized without fetching imports or validating an instance.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["XSD names, documentation text, type facets, values, schema locations and instance data are omitted or redacted".into(), "XSD include/import/redefine/override resources, validation, code generation and external URLs are never loaded or executed".into()];
    let mut page_sink = XsdPageSink {
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
    if rows.len() >= MAX_XSD_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XSD rows exceed {MAX_XSD_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_XSD_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XSD_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
