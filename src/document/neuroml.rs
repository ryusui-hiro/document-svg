//! Bounded NeuroML 2 neuroscience-model previews.
//!
//! NeuroML documents describe neuronal cells, morphologies and networks.
//! Structure is counted without exposing parameters, coordinates, equations,
//! imported files or running LEMS/simulation engines.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_NEUROML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_NEUROML_EVENTS: usize = 1_000_000;
const MAX_NEUROML_NODES: usize = 500_000;
const MAX_NEUROML_DEPTH: usize = 128;
const MAX_NEUROML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_NEUROML_ROWS: usize = 200_000;
const MAX_NEUROML_DISPLAY_BYTES: usize = 512;
const NEUROML_NAMESPACE: &str = "http://www.neuroml.org/schema/neuroml2";

#[derive(Default)]
struct Summary {
    cells: usize,
    morphologies: usize,
    segments: usize,
    segment_groups: usize,
    networks: usize,
    populations: usize,
    projections: usize,
    connections: usize,
    synapses: usize,
    inputs: usize,
    channels: usize,
    includes: usize,
    rows: Vec<Vec<String>>,
}

struct NeuromlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for NeuromlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "neuroml".into();
        if page.title.is_empty() {
            page.title = "NeuroML model".into();
        }
        page.description = "NeuroML neuroscience-model structure is rendered as bounded inert metadata; parameters and simulations are not evaluated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"neuroml", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("neuroml.org/schema/neuroml2")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_NEUROML_BYTES),
        "NeuroML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_NEUROML_EVENTS),
            max_nodes: MAX_NEUROML_NODES,
            max_depth: MAX_NEUROML_DEPTH,
            max_text_bytes: MAX_NEUROML_TEXT_BYTES,
        },
        "NeuroML",
    )?;
    if !root.name.eq_ignore_ascii_case("neuroml") {
        return Err(Error::InvalidInput("NeuroML root must be <neuroml>".into()));
    }
    if root.namespace.as_deref() != Some(NEUROML_NAMESPACE) {
        return Err(Error::InvalidInput(
            "NeuroML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        cells: count_named(&root, "cell")
            + count_named(&root, "iafCell")
            + count_named(&root, "izhikevich2007Cell"),
        morphologies: count_named(&root, "morphology"),
        segments: count_named(&root, "segment"),
        segment_groups: count_named(&root, "segmentGroup"),
        networks: count_named(&root, "network"),
        populations: count_named(&root, "population"),
        projections: count_named(&root, "projection"),
        connections: count_named(&root, "connection") + count_named(&root, "connectionWD"),
        synapses: count_named(&root, "alphaSynapse")
            + count_named(&root, "expTwoSynapse")
            + count_named(&root, "synapse"),
        inputs: count_named(&root, "inputList") + count_named(&root, "input"),
        channels: count_named(&root, "ionChannel") + count_named(&root, "channelDensity"),
        includes: count_named(&root, "include"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Cells",
        &summary.cells.to_string(),
        &format!(
            "morphologies={} segments={} segmentGroups={}",
            summary.morphologies, summary.segments, summary.segment_groups
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Networks",
        &summary.networks.to_string(),
        &format!(
            "populations={} projections={} connections={}",
            summary.populations, summary.projections, summary.connections
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Synapses",
        &summary.synapses.to_string(),
        &format!("inputs={} channels={}", summary.inputs, summary.channels),
    )?;
    push_row(
        &mut summary.rows,
        "Imports",
        &summary.includes.to_string(),
        "external model paths omitted",
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "NeuroML model".into() }, HtmlBlock::Paragraph { text: "NeuroML 2 cells, morphologies and network structure are summarized without exposing model values or running LEMS.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["NeuroML IDs, parameter quantities, coordinates, equations, annotations, URLs and imported model payloads are omitted or redacted".into(), "NeuroML includes, LEMS component definitions, MathML, external resources and neuroscience simulations never run".into()];
    let mut page_sink = NeuromlPageSink {
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
    if rows.len() >= MAX_NEUROML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "NeuroML rows exceed {MAX_NEUROML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_NEUROML_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_NEUROML_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
