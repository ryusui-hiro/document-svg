//! Bounded OCEL 2.0 XML event-log previews.
//!
//! OCEL XML contains object-centric events, object types and relationships.
//! This adapter renders log structure only; event/object identifiers,
//! timestamps, attributes and process-mining operations remain inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_OCEL_XML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OCEL_XML_EVENTS: usize = 1_000_000;
const MAX_OCEL_XML_NODES: usize = 500_000;
const MAX_OCEL_XML_DEPTH: usize = 128;
const MAX_OCEL_XML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_OCEL_XML_ROWS: usize = 200_000;
const MAX_OCEL_XML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    logs: usize,
    event_types: usize,
    events: usize,
    object_types: usize,
    objects: usize,
    attributes: usize,
    relationships: usize,
    value_types: usize,
    rows: Vec<Vec<String>>,
}

struct OcelXmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OcelXmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "ocel-xml".into();
        if page.title.is_empty() {
            page.title = "OCEL XML event log".into();
        }
        page.description =
            "OCEL XML event/object structure is rendered as bounded inert metadata; identifiers, values and process operations are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"log", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("event-types")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_OCEL_XML_BYTES),
        "OCEL XML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_OCEL_XML_EVENTS),
            max_nodes: MAX_OCEL_XML_NODES,
            max_depth: MAX_OCEL_XML_DEPTH,
            max_text_bytes: MAX_OCEL_XML_TEXT_BYTES,
        },
        "OCEL XML",
    )?;
    if !root.name.eq_ignore_ascii_case("log") {
        return Err(Error::InvalidInput("OCEL XML root must be <log>".into()));
    }
    let mut summary = Summary {
        logs: 1,
        event_types: count_named(&root, "event-type"),
        events: count_named(&root, "event"),
        object_types: count_named(&root, "object-type"),
        objects: count_named(&root, "object"),
        attributes: count_named(&root, "attribute"),
        relationships: count_named(&root, "relationship"),
        value_types: count_named(&root, "string")
            + count_named(&root, "integer")
            + count_named(&root, "float")
            + count_named(&root, "time")
            + count_named(&root, "boolean"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Log",
        &summary.logs.to_string(),
        &format!(
            "eventTypes={} objectTypes={}",
            summary.event_types, summary.object_types
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Events",
        &format!("events={}", summary.events),
        &format!(
            "attributes={} relationships={}",
            summary.attributes, summary.relationships
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Objects",
        &format!("objects={}", summary.objects),
        &format!("attributeTypes={}", summary.value_types),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OCEL XML event log".into(),
        },
        HtmlBlock::Paragraph {
            text: "Object-Centric Event Log 2.0 XML structure is summarized without exposing identifiers, timestamps or attribute values.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "OCEL event/object IDs, types, timestamps, attribute names/values, relationships and private log data are omitted or redacted".into(),
        "OCEL XML schema locations, external resources, process-mining discovery, filtering and event-log operations never run".into(),
    ];
    let mut page_sink = OcelXmlPageSink {
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
    if rows.len() >= MAX_OCEL_XML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OCEL XML rows exceed {MAX_OCEL_XML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OCEL_XML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_OCEL_XML_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
