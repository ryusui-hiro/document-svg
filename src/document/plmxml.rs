//! Bounded Siemens PLM XML product-structure metadata preview.
//!
//! PLMXML carries product definitions, assembly structures and references to
//! CAD representations. The converter reports that structure without opening
//! referenced XT/Parasolid files, URLs or geometry payloads.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_PLMXML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PLMXML_EVENTS: usize = 1_000_000;
const MAX_PLMXML_NODES: usize = 500_000;
const MAX_PLMXML_DEPTH: usize = 128;
const MAX_PLMXML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PLMXML_ROWS: usize = 200_000;
const MAX_PLMXML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    schema_version: String,
    product_defs: usize,
    parts: usize,
    structures: usize,
    instances: usize,
    representations: usize,
    geometry: usize,
    construction_geometry: usize,
    attributes: usize,
    external_refs: usize,
    rows: Vec<Vec<String>>,
}

struct PlmXmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for PlmXmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "plmxml".into();
        if page.title.is_empty() {
            page.title = "PLMXML product structure".into();
        }
        page.description = "PLMXML product and assembly metadata are rendered as bounded inert rows; referenced CAD geometry and external resources are not opened".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("<plmxml") && text.contains("plmxml.org/schemas/plmxmlschema")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PLMXML_BYTES),
        "PLMXML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_PLMXML_EVENTS),
            max_nodes: MAX_PLMXML_NODES,
            max_depth: MAX_PLMXML_DEPTH,
            max_text_bytes: MAX_PLMXML_TEXT_BYTES,
        },
        "PLMXML",
    )?;
    if !root.name.eq_ignore_ascii_case("PLMXML") {
        return Err(Error::InvalidInput(
            "PLMXML document must have a PLMXML root".into(),
        ));
    }
    if !root.namespace.as_deref().is_some_and(|namespace| {
        namespace.eq_ignore_ascii_case("http://www.plmxml.org/Schemas/PLMXMLSchema")
    }) {
        return Err(Error::InvalidInput(
            "PLMXML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        schema_version: attr_local(&root, "schemaVersion")
            .unwrap_or_default()
            .to_owned(),
        ..Summary::default()
    };
    summary.product_defs = count_named(&root, "ProductDef");
    summary.parts = count_named(&root, "Part");
    summary.structures = count_named(&root, "Structure");
    summary.instances = count_named(&root, "Instance");
    summary.representations = count_named(&root, "Representation");
    summary.geometry = count_named(&root, "Geometry");
    summary.construction_geometry = count_named(&root, "ConstructionGeometry");
    summary.attributes = count_named(&root, "Attribute");
    summary.external_refs =
        count_named(&root, "ExternalReference").saturating_add(count_named(&root, "ExternalData"));
    push_row(
        &mut summary.rows,
        "Document",
        "PLMXML",
        &format!(
            "schemaVersion={} units=metres",
            display_or_dash(&summary.schema_version)
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Product structure",
        &summary.product_defs.to_string(),
        &format!(
            "parts={} structures={} instances={}",
            summary.parts, summary.structures, summary.instances
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Representations",
        &summary.representations.to_string(),
        &format!(
            "geometry={} constructionGeometry={}",
            summary.geometry, summary.construction_geometry
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Metadata",
        &summary.attributes.to_string(),
        &format!("externalReferences={}", summary.external_refs),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "PLMXML product structure".into(),
        },
        HtmlBlock::Paragraph {
            text: "PLMXML references product lifecycle and CAD data. This preview keeps the structure inert and does not resolve referenced geometry.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "PLMXML product/assembly structure and representation counts are shown; IDs, attribute values, URLs and external reference payloads are omitted or redacted".into(),
        "PLMXML geometry, XT/Parasolid files, tessellation, formulas, signatures and network resources are never opened or evaluated".into(),
    ];
    let mut page_sink = PlmXmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| usize::from(child.name.eq_ignore_ascii_case(name)) + count_named(child, name))
        .sum()
}

fn display_or_dash(value: &str) -> String {
    if value.is_empty() {
        "-".into()
    } else {
        truncate(value)
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_PLMXML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_PLMXML_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_PLMXML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "PLMXML rows exceed {MAX_PLMXML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
