//! Bounded ISO 10303-28 STEP-XML metadata previews.
//!
//! STEP-XML represents EXPRESS-governed product data using XML Schema. This
//! adapter inventories common product, assembly and geometry entities without
//! evaluating EXPRESS rules or resolving external schemas/references.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_STEPXML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_STEPXML_EVENTS: usize = 1_000_000;
const MAX_STEPXML_NODES: usize = 500_000;
const MAX_STEPXML_DEPTH: usize = 128;
const MAX_STEPXML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_STEPXML_ROWS: usize = 200_000;
const MAX_STEPXML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    products: usize,
    product_defs: usize,
    parts: usize,
    assemblies: usize,
    occurrences: usize,
    representations: usize,
    geometry: usize,
    points: usize,
    directions: usize,
    properties: usize,
    external_refs: usize,
    rows: Vec<Vec<String>>,
}

struct StepXmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for StepXmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "stepxml".into();
        if page.title.is_empty() {
            page.title = "STEP-XML product data".into();
        }
        page.description = "STEP-XML entity metadata is rendered as bounded inert rows; EXPRESS rules, external schemas and referenced geometry are not evaluated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("iso_10303_28")
        && text.contains("xmlschema")
        && (text.contains("product_definition") || text.contains("cartesian_point"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_STEPXML_BYTES),
        "STEP-XML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_STEPXML_EVENTS),
            max_nodes: MAX_STEPXML_NODES,
            max_depth: MAX_STEPXML_DEPTH,
            max_text_bytes: MAX_STEPXML_TEXT_BYTES,
        },
        "STEP-XML",
    )?;
    if !root.name.to_ascii_lowercase().contains("iso_10303_28") {
        return Err(Error::InvalidInput(
            "STEP-XML document must have an iso_10303_28 root".into(),
        ));
    }
    if !root.namespace.as_deref().is_some_and(|namespace| {
        let namespace = namespace.to_ascii_lowercase();
        namespace.contains("10303") && namespace.contains("xmlschema")
    }) {
        return Err(Error::InvalidInput(
            "STEP-XML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        version: attr_local(&root, "version").unwrap_or_default().to_owned(),
        ..Summary::default()
    };
    summary.products = count_named(&root, "product");
    summary.product_defs = count_named(&root, "product_definition");
    summary.parts = count_named(&root, "product_definition_formation");
    summary.assemblies = count_named(&root, "next_assembly_usage_occurrence")
        .saturating_add(count_named(&root, "assembly_component"));
    summary.occurrences = count_named(&root, "product_definition_relationship")
        .saturating_add(count_named(&root, "component_occurrence"));
    summary.representations = count_named(&root, "shape_representation")
        .saturating_add(count_named(&root, "representation"));
    summary.geometry = count_named(&root, "geometric_representation_item")
        .saturating_add(count_named(&root, "advanced_brep_shape_representation"))
        .saturating_add(count_named(&root, "manifold_surface_shape_representation"));
    summary.points = count_named(&root, "cartesian_point");
    summary.directions = count_named(&root, "direction");
    summary.properties = count_named(&root, "property_definition")
        .saturating_add(count_named(&root, "property_definition_representation"));
    summary.external_refs = count_named(&root, "external_reference")
        .saturating_add(count_named(&root, "document_file"));
    push_row(
        &mut summary.rows,
        "Document",
        "STEP-XML",
        &format!(
            "version={} units/context=not evaluated",
            display_or_dash(&summary.version)
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Products",
        &summary.products.to_string(),
        &format!(
            "product_definitions={} formations={}",
            summary.product_defs, summary.parts
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Assembly",
        &summary.assemblies.to_string(),
        &format!(
            "occurrences={} relationships={}",
            summary.occurrences, summary.occurrences
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Geometry",
        &summary.representations.to_string(),
        &format!(
            "items={} points={} directions={}",
            summary.geometry, summary.points, summary.directions
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Properties",
        &summary.properties.to_string(),
        &format!("externalReferences={}", summary.external_refs),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "STEP-XML product data".into(),
        },
        HtmlBlock::Paragraph {
            text: "ISO 10303-28 XML represents EXPRESS product data. This preview reports bounded structure and never evaluates schemas or referenced CAD geometry.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "STEP-XML product, assembly, representation and geometry counts are shown; entity IDs, attribute values, URLs and external references are omitted or redacted".into(),
        "EXPRESS/schema validation, external schema fetches, CAD geometry resolution, formulas and network operations never run".into(),
    ];
    let mut page_sink = StepXmlPageSink {
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
    if value.len() <= MAX_STEPXML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_STEPXML_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_STEPXML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "STEP-XML rows exceed {MAX_STEPXML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
