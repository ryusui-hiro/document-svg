//! Bounded BioPAX Level 3 RDF/XML pathway previews.
//!
//! BioPAX exchanges biological pathway entities and interactions. This
//! adapter counts BioPAX classes and links without following OWL imports,
//! resolving RDF resources or performing inference and pathway analysis.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_BIOPAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BIOPAX_EVENTS: usize = 1_000_000;
const MAX_BIOPAX_NODES: usize = 500_000;
const MAX_BIOPAX_DEPTH: usize = 128;
const MAX_BIOPAX_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_BIOPAX_ROWS: usize = 200_000;
const MAX_BIOPAX_CLASSES: usize = 200_000;
const MAX_BIOPAX_DISPLAY_BYTES: usize = 512;
const RDF_NAMESPACE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const BIOPAX_NAMESPACE: &str = "http://www.biopax.org/release/biopax-level";

#[derive(Default)]
struct Summary {
    entities: usize,
    pathways: usize,
    interactions: usize,
    physical_entities: usize,
    proteins: usize,
    small_molecules: usize,
    complexes: usize,
    reactions: usize,
    controls: usize,
    xrefs: usize,
    imports: usize,
    classes: BTreeMap<String, usize>,
    rows: Vec<Vec<String>>,
}

struct BiopaxPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for BiopaxPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "biopax".into();
        if page.title.is_empty() {
            page.title = "BioPAX pathway".into();
        }
        page.description = "BioPAX RDF/XML pathway structure is rendered as bounded inert metadata; ontology imports and inference are not run".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("biopax.org/release/biopax-level")
        && (text.contains("rdf:rdf") || text.contains("<rdf"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_BIOPAX_BYTES),
        "BioPAX input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_BIOPAX_EVENTS),
            max_nodes: MAX_BIOPAX_NODES,
            max_depth: MAX_BIOPAX_DEPTH,
            max_text_bytes: MAX_BIOPAX_TEXT_BYTES,
        },
        "BioPAX",
    )?;
    if !root.name.eq_ignore_ascii_case("RDF") {
        return Err(Error::InvalidInput("BioPAX root must be rdf:RDF".into()));
    }
    if root.namespace.as_deref() != Some(RDF_NAMESPACE) {
        return Err(Error::InvalidInput(
            "BioPAX root uses an unsupported RDF namespace".into(),
        ));
    }
    if !contains_biopax_namespace(&root) {
        return Err(Error::InvalidInput(
            "BioPAX RDF/XML contains no BioPAX namespace".into(),
        ));
    }
    let mut summary = Summary::default();
    summarize(&root, &mut summary)?;
    push_row(
        &mut summary.rows,
        "Entities",
        &summary.entities.to_string(),
        &format!(
            "pathways={} interactions={}",
            summary.pathways, summary.interactions
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Physical",
        &summary.physical_entities.to_string(),
        &format!(
            "proteins={} smallMolecules={} complexes={}",
            summary.proteins, summary.small_molecules, summary.complexes
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Reactions",
        &summary.reactions.to_string(),
        &format!("controls={} xrefs={}", summary.controls, summary.xrefs),
    )?;
    push_row(
        &mut summary.rows,
        "Ontology",
        &summary.classes.len().to_string(),
        &format!("imports={} classValues omitted", summary.imports),
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "BioPAX pathway".into() }, HtmlBlock::Paragraph { text: "Biological Pathway Exchange RDF/XML structure is summarized without exposing pathway identifiers or running inference.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["BioPAX resource IDs, labels, names, pathway values, RDF links, OWL imports and external ontology URIs are omitted or redacted".into(), "BioPAX OWL inference, SPARQL, pathway analysis, external references and network resources never run".into()];
    let mut page_sink = BiopaxPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn contains_biopax_namespace(element: &XmlElement) -> bool {
    element
        .namespace
        .as_deref()
        .is_some_and(|namespace| namespace.contains(BIOPAX_NAMESPACE))
        || element.children.iter().any(contains_biopax_namespace)
}

fn summarize(element: &XmlElement, summary: &mut Summary) -> Result<()> {
    if element
        .namespace
        .as_deref()
        .is_some_and(|namespace| namespace.contains(BIOPAX_NAMESPACE))
    {
        summary.entities = summary.entities.saturating_add(1);
        if summary.entities > MAX_BIOPAX_CLASSES {
            return Err(Error::LimitExceeded(format!(
                "BioPAX entities exceed {MAX_BIOPAX_CLASSES}"
            )));
        }
        *summary.classes.entry(element.name.clone()).or_default() += 1;
        match element.name.to_ascii_lowercase().as_str() {
            "pathway" => summary.pathways += 1,
            "interaction" | "conversion" => summary.interactions += 1,
            "physicalentity" => summary.physical_entities += 1,
            "protein" => summary.proteins += 1,
            "smallmolecule" => summary.small_molecules += 1,
            "complex" => summary.complexes += 1,
            "biochemicalreaction" | "transport" | "templatereaction" => summary.reactions += 1,
            "control" | "catalysis" => summary.controls += 1,
            "publicationxref" | "unificationxref" | "relationshipxref" => summary.xrefs += 1,
            _ => {}
        }
    }
    if element.name.eq_ignore_ascii_case("imports") {
        summary.imports = summary.imports.saturating_add(1);
    }
    for child in &element.children {
        summarize(child, summary)?;
    }
    Ok(())
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_BIOPAX_ROWS {
        return Err(Error::LimitExceeded(format!(
            "BioPAX rows exceed {MAX_BIOPAX_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_BIOPAX_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_BIOPAX_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
