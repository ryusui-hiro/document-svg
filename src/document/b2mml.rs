//! Bounded B2MML / ISA-95 manufacturing metadata previews.
//!
//! B2MML is a family of XML schemas for exchanging enterprise-control and
//! manufacturing information. This adapter reports document and model
//! structure without executing production operations or exposing values.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_B2MML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_B2MML_EVENTS: usize = 1_000_000;
const MAX_B2MML_NODES: usize = 500_000;
const MAX_B2MML_DEPTH: usize = 128;
const MAX_B2MML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_B2MML_ROWS: usize = 200_000;
const MAX_B2MML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    schedules: usize,
    performances: usize,
    requests: usize,
    responses: usize,
    products: usize,
    materials: usize,
    equipment: usize,
    personnel: usize,
    process_segments: usize,
    capabilities: usize,
    maintenance: usize,
    transactions: usize,
    external_refs: usize,
    rows: Vec<Vec<String>>,
}

struct B2mmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for B2mmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "b2mml".into();
        if page.title.is_empty() {
            page.title = "B2MML manufacturing data".into();
        }
        page.description = "B2MML manufacturing metadata is rendered as bounded inert rows; production operations and external resources are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("mesa.org/xml/b2mml")
        && (text.contains("productionschedule")
            || text.contains("productionperformance")
            || text.contains("materialdefinition")
            || text.contains("equipment"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_B2MML_BYTES),
        "B2MML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_B2MML_EVENTS),
            max_nodes: MAX_B2MML_NODES,
            max_depth: MAX_B2MML_DEPTH,
            max_text_bytes: MAX_B2MML_TEXT_BYTES,
        },
        "B2MML",
    )?;
    if !root.namespace.as_deref().is_some_and(|namespace| {
        namespace
            .to_ascii_lowercase()
            .contains("mesa.org/xml/b2mml")
    }) {
        return Err(Error::InvalidInput(
            "B2MML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        schedules: count_named(&root, "ProductionSchedule"),
        performances: count_named(&root, "ProductionPerformance"),
        requests: count_named(&root, "ProductionRequest"),
        responses: count_named(&root, "ProductionResponse"),
        products: count_named(&root, "ProductDefinition")
            .saturating_add(count_named(&root, "ProductSegment")),
        materials: count_named(&root, "MaterialDefinition")
            .saturating_add(count_named(&root, "MaterialLot"))
            .saturating_add(count_named(&root, "MaterialActual")),
        equipment: count_named(&root, "Equipment")
            .saturating_add(count_named(&root, "EquipmentClass"))
            .saturating_add(count_named(&root, "EquipmentActual")),
        personnel: count_named(&root, "Personnel")
            .saturating_add(count_named(&root, "Person"))
            .saturating_add(count_named(&root, "PersonnelActual")),
        process_segments: count_named(&root, "ProcessSegment")
            .saturating_add(count_named(&root, "SegmentRequirement"))
            .saturating_add(count_named(&root, "SegmentResponse")),
        capabilities: count_named(&root, "ProductionCapability")
            .saturating_add(count_named(&root, "EquipmentCapability")),
        maintenance: count_named(&root, "Maintenance"),
        transactions: count_named(&root, "ApplicationArea")
            .saturating_add(count_named(&root, "Transaction")),
        external_refs: count_named(&root, "ExternalReference")
            .saturating_add(count_named(&root, "File")),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Schedules",
        &summary.schedules.to_string(),
        &format!(
            "performances={} requests={} responses={}",
            summary.performances, summary.requests, summary.responses
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Resources",
        &summary.equipment.to_string(),
        &format!(
            "materials={} personnel={}",
            summary.materials, summary.personnel
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Process",
        &summary.process_segments.to_string(),
        &format!(
            "products={} capabilities={} maintenance={}",
            summary.products, summary.capabilities, summary.maintenance
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Transactions",
        &summary.transactions.to_string(),
        &format!("externalReferences={}", summary.external_refs),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "B2MML manufacturing data".into(),
        },
        HtmlBlock::Paragraph {
            text: "B2MML aligns XML exchange with ISA-95 enterprise-control and manufacturing models. This preview keeps production and resource data inert.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "B2MML schedule, performance, resource and process structure is shown; values, IDs, credentials, URLs and sensitive production payloads are omitted or redacted".into(),
        "B2MML operations, transactions, schema includes, external files and network resources are never executed or fetched".into(),
    ];
    let mut page_sink = B2mmlPageSink {
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

fn truncate(value: &str) -> String {
    if value.len() <= MAX_B2MML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_B2MML_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_B2MML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "B2MML rows exceed {MAX_B2MML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
