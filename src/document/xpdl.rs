//! Bounded XPDL workflow-definition previews.
//!
//! XPDL exchanges workflow models between tools. The preview reports process
//! structure only; expressions, scripts, participant values and external
//! applications are never executed or resolved.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XPDL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XPDL_EVENTS: usize = 1_000_000;
const MAX_XPDL_NODES: usize = 500_000;
const MAX_XPDL_DEPTH: usize = 128;
const MAX_XPDL_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XPDL_ROWS: usize = 200_000;
const MAX_XPDL_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    packages: usize,
    processes: usize,
    activities: usize,
    transitions: usize,
    applications: usize,
    participants: usize,
    data_fields: usize,
    pools: usize,
    lanes: usize,
    connectors: usize,
    external_refs: usize,
    rows: Vec<Vec<String>>,
}

struct XpdlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for XpdlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xpdl".into();
        if page.title.is_empty() {
            page.title = "XPDL workflow definition".into();
        }
        page.description =
            "XPDL workflow structure is rendered as bounded inert metadata; expressions, scripts and external applications are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"Package", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("wfmc.org")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XPDL_BYTES),
        "XPDL input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XPDL_EVENTS),
            max_nodes: MAX_XPDL_NODES,
            max_depth: MAX_XPDL_DEPTH,
            max_text_bytes: MAX_XPDL_TEXT_BYTES,
        },
        "XPDL",
    )?;
    if !root.name.eq_ignore_ascii_case("Package") {
        return Err(Error::InvalidInput("XPDL root must be <Package>".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| !namespace.to_ascii_lowercase().contains("wfmc.org"))
    {
        return Err(Error::InvalidInput(
            "XPDL root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        version: attr_local(&root, "xpdlVersion")
            .or_else(|| attr_local(&root, "version"))
            .map(truncate)
            .unwrap_or_default(),
        packages: 1,
        processes: count_named(&root, "WorkflowProcess"),
        activities: count_named(&root, "Activity"),
        transitions: count_named(&root, "Transition"),
        applications: count_named(&root, "Application"),
        participants: count_named(&root, "Participant"),
        data_fields: count_named(&root, "DataField") + count_named(&root, "FormalParameter"),
        pools: count_named(&root, "Pool"),
        lanes: count_named(&root, "Lane"),
        connectors: count_named(&root, "Connector") + count_named(&root, "ExtendedAttribute"),
        external_refs: count_named(&root, "ExternalReference")
            + count_named(&root, "ExternalPackage"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Package",
        &summary.packages.to_string(),
        &format!("version={}", display_or_dash(&summary.version)),
    )?;
    push_row(
        &mut summary.rows,
        "Workflow",
        &summary.processes.to_string(),
        &format!(
            "activities={} transitions={}",
            summary.activities, summary.transitions
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Roles",
        &summary.participants.to_string(),
        &format!("pools={} lanes={}", summary.pools, summary.lanes),
    )?;
    push_row(
        &mut summary.rows,
        "Integration",
        &summary.applications.to_string(),
        &format!(
            "dataFields={} connectors={} externalRefs={}",
            summary.data_fields, summary.connectors, summary.external_refs
        ),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "XPDL workflow definition".into(),
        },
        HtmlBlock::Paragraph {
            text: "Workflow Process Definition Language structure is summarized for model review; expressions, scripts, applications and process execution remain inert.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "XPDL process IDs, labels, expressions, scripts, URLs, application parameters and private values are omitted or redacted; only bounded structural counts are shown".into(),
        "XPDL external packages, application services, scripts, expressions, schemas and workflow engines are never opened or executed".into(),
    ];
    let mut page_sink = XpdlPageSink {
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
    if rows.len() >= MAX_XPDL_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XPDL rows exceed {MAX_XPDL_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_XPDL_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_XPDL_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
