//! Bounded HL7 Clinical Document Architecture (CDA) previews.
//!
//! CDA documents contain protected patient summaries and narrative sections.
//! This adapter validates the document envelope and reports structural counts
//! only; PHI, identifiers, narrative text, coded values and references remain
//! inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_CDA_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CDA_EVENTS: usize = 1_000_000;
const MAX_CDA_NODES: usize = 500_000;
const MAX_CDA_DEPTH: usize = 128;
const MAX_CDA_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_CDA_ROWS: usize = 200_000;
const MAX_CDA_DISPLAY_BYTES: usize = 512;
const CDA_NAMESPACE: &str = "urn:hl7-org:v3";

#[derive(Default)]
struct Summary {
    sections: usize,
    entries: usize,
    observations: usize,
    acts: usize,
    encounters: usize,
    procedures: usize,
    organizers: usize,
    authors: usize,
    participants: usize,
    assigned_entities: usize,
    narrative_blocks: usize,
    templates: usize,
    rows: Vec<Vec<String>>,
}

struct CdaPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CdaPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "cda".into();
        if page.title.is_empty() {
            page.title = "CDA clinical document".into();
        }
        page.description =
            "HL7 CDA structure is rendered as bounded inert metadata; patient data and narrative content are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"ClinicalDocument", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("urn:hl7-org:v3")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_CDA_BYTES),
        "CDA input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_CDA_EVENTS),
            max_nodes: MAX_CDA_NODES,
            max_depth: MAX_CDA_DEPTH,
            max_text_bytes: MAX_CDA_TEXT_BYTES,
        },
        "CDA",
    )?;
    if !root.name.eq_ignore_ascii_case("ClinicalDocument") {
        return Err(Error::InvalidInput(
            "CDA root must be <ClinicalDocument>".into(),
        ));
    }
    if root.namespace.as_deref() != Some(CDA_NAMESPACE) {
        return Err(Error::InvalidInput(
            "CDA root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        sections: count_named(&root, "section"),
        entries: count_named(&root, "entry"),
        observations: count_named(&root, "observation"),
        acts: count_named(&root, "act"),
        encounters: count_named(&root, "encounter"),
        procedures: count_named(&root, "procedure"),
        organizers: count_named(&root, "organizer"),
        authors: count_named(&root, "author"),
        participants: count_named(&root, "participant"),
        assigned_entities: count_named(&root, "assignedEntity"),
        narrative_blocks: count_named(&root, "text")
            + count_named(&root, "list")
            + count_named(&root, "table"),
        templates: count_named(&root, "templateId"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Document",
        &root.name,
        &format!(
            "sections={} templates={}",
            summary.sections, summary.templates
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Clinical entries",
        &summary.entries.to_string(),
        &format!(
            "observations={} acts={} organizers={}",
            summary.observations, summary.acts, summary.organizers
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Care context",
        &summary.encounters.to_string(),
        &format!(
            "procedures={} authors={} participants={} entities={}",
            summary.procedures, summary.authors, summary.participants, summary.assigned_entities
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Narrative",
        &summary.narrative_blocks.to_string(),
        "narrative values omitted",
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "CDA clinical document".into(),
        },
        HtmlBlock::Paragraph {
            text: "Clinical Document Architecture structure is summarized without displaying protected health information or narrative text.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "CDA patient names, identifiers, dates, narrative XHTML, coded values, addresses, providers, references and clinical payloads are omitted or redacted; this preview is not de-identification".into(),
        "CDA templates, vocabulary/schema locations, XSLT stylesheets, external documents and clinical operations are never fetched or executed".into(),
    ];
    let mut page_sink = CdaPageSink {
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
    if rows.len() >= MAX_CDA_ROWS {
        return Err(Error::LimitExceeded(format!(
            "CDA rows exceed {MAX_CDA_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_CDA_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_CDA_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
