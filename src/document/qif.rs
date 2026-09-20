//! Bounded Quality Information Framework (QIF) inspection metadata previews.
//!
//! QIF is an XML framework for product definition, inspection plans and
//! measurement results. This adapter exposes structure and counts while
//! omitting measured values, identifiers, URLs and external measurement files.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_QIF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_QIF_EVENTS: usize = 1_000_000;
const MAX_QIF_NODES: usize = 500_000;
const MAX_QIF_DEPTH: usize = 128;
const MAX_QIF_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_QIF_ROWS: usize = 200_000;
const MAX_QIF_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    products: usize,
    product_definitions: usize,
    plans: usize,
    results: usize,
    features: usize,
    measurements: usize,
    characteristics: usize,
    datums: usize,
    external_files: usize,
    traceability: usize,
    rows: Vec<Vec<String>>,
}

struct QifPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for QifPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "qif".into();
        if page.title.is_empty() {
            page.title = "QIF inspection data".into();
        }
        page.description = "QIF product, inspection-plan and measurement metadata are rendered as bounded inert rows; measured values and external files are not opened".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("<qifdocument") && text.contains("qifstandards.org/xsd/qif")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_QIF_BYTES),
        "QIF input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_QIF_EVENTS),
            max_nodes: MAX_QIF_NODES,
            max_depth: MAX_QIF_DEPTH,
            max_text_bytes: MAX_QIF_TEXT_BYTES,
        },
        "QIF",
    )?;
    if !root.name.eq_ignore_ascii_case("QIFDocument") {
        return Err(Error::InvalidInput(
            "QIF document must have a QIFDocument root".into(),
        ));
    }
    if !root.namespace.as_deref().is_some_and(|namespace| {
        let namespace = namespace.to_ascii_lowercase();
        namespace.contains("qifstandards.org/xsd/qif")
    }) {
        return Err(Error::InvalidInput(
            "QIF root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        version: attr_local(&root, "version").unwrap_or_default().to_owned(),
        ..Summary::default()
    };
    summary.products = count_named(&root, "Product");
    summary.product_definitions =
        count_named(&root, "ProductDefinition").saturating_add(count_named(&root, "Part"));
    summary.plans =
        count_named(&root, "MeasurementPlan").saturating_add(count_named(&root, "InspectionPlan"));
    summary.results =
        count_named(&root, "MeasurementResults").saturating_add(count_named(&root, "QMResults"));
    summary.features = count_named(&root, "Feature")
        .saturating_add(count_named(&root, "MeasuredFeature"))
        .saturating_add(count_named(&root, "FeatureMeasurement"));
    summary.measurements =
        count_named(&root, "Measurement").saturating_add(count_named(&root, "MeasuredPointSet"));
    summary.characteristics = count_named(&root, "Characteristic")
        .saturating_add(count_named(&root, "CharacteristicDefinition"));
    summary.datums = count_named(&root, "Datum");
    summary.external_files = count_named(&root, "ExternalFileReference");
    summary.traceability = count_named(&root, "InspectionTraceability");
    push_row(
        &mut summary.rows,
        "Document",
        "QIF",
        &format!(
            "version={} namespace=qif",
            display_or_dash(&summary.version)
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Product",
        &summary.products.to_string(),
        &format!(
            "definitions={} datums={}",
            summary.product_definitions, summary.datums
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Inspection",
        &summary.plans.to_string(),
        &format!(
            "results={} traceability={}",
            summary.results, summary.traceability
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Measurements",
        &summary.measurements.to_string(),
        &format!(
            "features={} characteristics={}",
            summary.features, summary.characteristics
        ),
    )?;
    push_row(
        &mut summary.rows,
        "External files",
        &summary.external_files.to_string(),
        "references counted; payloads omitted",
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "QIF inspection data".into(),
        },
        HtmlBlock::Paragraph {
            text: "Quality Information Framework XML connects product definition, inspection planning and measurement results. This preview keeps all measurement payloads inert.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "QIF product, plan, result, feature and characteristic structure is shown; measured values, IDs, URLs, tolerances and sensitive inspection payloads are omitted or redacted".into(),
        "QIF schema includes, external measurement files, CAD/PMI resources, scripts and network operations are never fetched or executed".into(),
    ];
    let mut page_sink = QifPageSink {
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
    if value.len() <= MAX_QIF_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_QIF_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_QIF_ROWS {
        return Err(Error::LimitExceeded(format!(
            "QIF rows exceed {MAX_QIF_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
