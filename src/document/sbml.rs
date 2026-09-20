//! Bounded Systems Biology Markup Language (SBML) previews.
//!
//! SBML models describe biological reaction networks and simulation metadata.
//! The preview reports model structure only; mathematical expressions,
//! parameter values, annotations and simulation engines remain inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_SBML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SBML_EVENTS: usize = 1_000_000;
const MAX_SBML_NODES: usize = 500_000;
const MAX_SBML_DEPTH: usize = 128;
const MAX_SBML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SBML_ROWS: usize = 200_000;
const MAX_SBML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    level: String,
    version: String,
    models: usize,
    compartments: usize,
    species: usize,
    reactions: usize,
    parameters: usize,
    rules: usize,
    events: usize,
    units: usize,
    functions: usize,
    annotations: usize,
    rows: Vec<Vec<String>>,
}

struct SbmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SbmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "sbml".into();
        if page.title.is_empty() {
            page.title = "SBML model".into();
        }
        page.description =
            "SBML model structure is rendered as bounded inert metadata; equations and simulation values are not evaluated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"sbml", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("sbml.org/sbml")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SBML_BYTES),
        "SBML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_SBML_EVENTS),
            max_nodes: MAX_SBML_NODES,
            max_depth: MAX_SBML_DEPTH,
            max_text_bytes: MAX_SBML_TEXT_BYTES,
        },
        "SBML",
    )?;
    if !root.name.eq_ignore_ascii_case("sbml") {
        return Err(Error::InvalidInput("SBML root must be <sbml>".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| !namespace.to_ascii_lowercase().contains("sbml.org/sbml"))
    {
        return Err(Error::InvalidInput(
            "SBML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        level: attr_local(&root, "level").map(truncate).unwrap_or_default(),
        version: attr_local(&root, "version")
            .map(truncate)
            .unwrap_or_default(),
        models: count_named(&root, "model"),
        compartments: count_named(&root, "compartment"),
        species: count_named(&root, "species"),
        reactions: count_named(&root, "reaction"),
        parameters: count_named(&root, "parameter"),
        rules: count_named(&root, "assignmentRule")
            + count_named(&root, "rateRule")
            + count_named(&root, "algebraicRule"),
        events: count_named(&root, "event"),
        units: count_named(&root, "unitDefinition"),
        functions: count_named(&root, "functionDefinition"),
        annotations: count_named(&root, "annotation") + count_named(&root, "notes"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Model",
        &summary.models.to_string(),
        &format!(
            "level={} version={}",
            display_or_dash(&summary.level),
            display_or_dash(&summary.version)
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Network",
        &format!("species={}", summary.species),
        &format!(
            "compartments={} reactions={}",
            summary.compartments, summary.reactions
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Parameters",
        &summary.parameters.to_string(),
        &format!(
            "rules={} events={} units={}",
            summary.rules, summary.events, summary.units
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Extensions",
        &summary.functions.to_string(),
        &format!("annotations={} package values omitted", summary.annotations),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "SBML model".into(),
        },
        HtmlBlock::Paragraph {
            text: "Systems Biology Markup Language model structure is summarized without evaluating equations, parameters or simulations.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "SBML species, parameter, compartment, equation, annotation, identifier and model values are omitted or redacted; only bounded structure is shown".into(),
        "SBML MathML, package schemas, annotations, external resources, rule evaluation and numerical simulation never run".into(),
    ];
    let mut page_sink = SbmlPageSink {
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

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_SBML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "SBML rows exceed {MAX_SBML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_SBML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_SBML_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
