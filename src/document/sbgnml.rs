//! Bounded SBGN-ML pathway-diagram previews.
//!
//! SBGN-ML serializes Systems Biology Graphical Notation maps. This adapter
//! reports glyph/arc structure without rendering arbitrary layout payloads,
//! evaluating biology or following external resources.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_SBGNML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SBGNML_EVENTS: usize = 1_000_000;
const MAX_SBGNML_NODES: usize = 500_000;
const MAX_SBGNML_DEPTH: usize = 128;
const MAX_SBGNML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SBGNML_ROWS: usize = 200_000;
const MAX_SBGNML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    language: String,
    glyphs: usize,
    arcs: usize,
    labels: usize,
    ports: usize,
    bboxes: usize,
    clone_markers: usize,
    states: usize,
    terminals: usize,
    callouts: usize,
    submaps: usize,
    rows: Vec<Vec<String>>,
}

struct SbgnmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SbgnmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "sbgnml".into();
        if page.title.is_empty() {
            page.title = "SBGN-ML map".into();
        }
        page.description =
            "SBGN-ML glyph and arc structure is rendered as bounded inert metadata; biological semantics and external resources are not evaluated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"map", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("sbgn.org/libsbgn")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SBGNML_BYTES),
        "SBGN-ML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_SBGNML_EVENTS),
            max_nodes: MAX_SBGNML_NODES,
            max_depth: MAX_SBGNML_DEPTH,
            max_text_bytes: MAX_SBGNML_TEXT_BYTES,
        },
        "SBGN-ML",
    )?;
    if !root.name.eq_ignore_ascii_case("map") {
        return Err(Error::InvalidInput("SBGN-ML root must be <map>".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| !namespace.to_ascii_lowercase().contains("sbgn.org/libsbgn"))
    {
        return Err(Error::InvalidInput(
            "SBGN-ML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        language: attr_local(&root, "language")
            .map(truncate)
            .unwrap_or_default(),
        glyphs: count_named(&root, "glyph"),
        arcs: count_named(&root, "arc"),
        labels: count_named(&root, "label"),
        ports: count_named(&root, "port"),
        bboxes: count_named(&root, "bbox"),
        clone_markers: count_named(&root, "clone") + count_named(&root, "cloneMarker"),
        states: count_named(&root, "state"),
        terminals: count_named(&root, "terminal"),
        callouts: count_named(&root, "callout"),
        submaps: count_named(&root, "submap"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Map",
        &summary.glyphs.to_string(),
        &format!(
            "language={} arcs={}",
            display_or_dash(&summary.language),
            summary.arcs
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Geometry",
        &summary.bboxes.to_string(),
        &format!(
            "labels={} ports={} callouts={}",
            summary.labels, summary.ports, summary.callouts
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Glyph state",
        &summary.states.to_string(),
        &format!(
            "cloneMarkers={} terminals={} submaps={}",
            summary.clone_markers, summary.terminals, summary.submaps
        ),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "SBGN-ML map".into(),
        },
        HtmlBlock::Paragraph {
            text: "Systems Biology Graphical Notation map structure is summarized without evaluating biological semantics or external resources.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "SBGN-ML glyph classes, IDs, labels, coordinates, ports, states, arcs and annotations are omitted or redacted; only bounded counts are shown".into(),
        "SBGN-ML biological inference, layout engines, external references, style resources and embedded data never run".into(),
    ];
    let mut page_sink = SbgnmlPageSink {
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
    if rows.len() >= MAX_SBGNML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "SBGN-ML rows exceed {MAX_SBGNML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_SBGNML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_SBGNML_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
