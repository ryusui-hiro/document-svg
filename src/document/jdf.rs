//! Bounded CIP4 Job Definition Format (JDF/JMF) workflow previews.
//!
//! JDF is an XML job-ticket and process-automation format. This adapter
//! exposes node/resource structure without executing jobs, contacting devices,
//! following URLs or opening linked files.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_JDF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_JDF_EVENTS: usize = 1_000_000;
const MAX_JDF_NODES: usize = 500_000;
const MAX_JDF_DEPTH: usize = 128;
const MAX_JDF_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_JDF_ROWS: usize = 200_000;
const MAX_JDF_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    root: String,
    version: String,
    status: String,
    jdf_nodes: usize,
    resources: usize,
    resource_pools: usize,
    resource_links: usize,
    products: usize,
    processes: usize,
    devices: usize,
    media: usize,
    layouts: usize,
    run_lists: usize,
    audits: usize,
    files: usize,
    urls: usize,
    rows: Vec<Vec<String>>,
}

struct JdfPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
    source_format: &'static str,
}

impl PageConsumer for JdfPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = self.source_format.into();
        if page.title.is_empty() {
            page.title = if self.source_format == "xjdf" {
                "XJDF job ticket".into()
            } else {
                "JDF job ticket".into()
            };
        }
        page.description = "JDF/JMF workflow metadata is rendered as bounded inert rows; jobs, devices, URLs and external files are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("cip4.org/jdfschema")
        && (text.contains("<jdf") || text.contains("<jmf") || text.contains("<xjdf"))
}

pub(crate) fn looks_like_xjdf_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("cip4.org/jdfschema") && text.contains("<xjdf")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_inner(path, options, sink, false)
}

pub(crate) fn convert_xjdf(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    convert_inner(path, options, sink, true)
}

fn convert_inner(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
    xjdf: bool,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_JDF_BYTES),
        "JDF input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_JDF_EVENTS),
            max_nodes: MAX_JDF_NODES,
            max_depth: MAX_JDF_DEPTH,
            max_text_bytes: MAX_JDF_TEXT_BYTES,
        },
        "JDF",
    )?;
    let root_matches = if xjdf {
        root.name.eq_ignore_ascii_case("XJDF")
    } else {
        root.name.eq_ignore_ascii_case("JDF") || root.name.eq_ignore_ascii_case("JMF")
    };
    if !root_matches {
        return Err(Error::InvalidInput(
            if xjdf {
                "XJDF document must have an XJDF root"
            } else {
                "JDF document must have a JDF or JMF root"
            }
            .into(),
        ));
    }
    if !root.namespace.as_deref().is_some_and(|namespace| {
        namespace
            .to_ascii_lowercase()
            .contains("cip4.org/jdfschema")
    }) {
        return Err(Error::InvalidInput(
            "JDF root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        root: root.name.clone(),
        version: attr_local(&root, "Version").unwrap_or_default().to_owned(),
        status: attr_local(&root, "Status").unwrap_or_default().to_owned(),
        jdf_nodes: 1usize.saturating_add(count_named(&root, "JDF")),
        resources: count_named(&root, "Resource")
            .saturating_add(count_named(&root, "Component"))
            .saturating_add(count_named(&root, "Media"))
            .saturating_add(count_named(&root, "RunList"))
            .saturating_add(count_named(&root, "Device"))
            .saturating_add(count_named(&root, "FileSpec")),
        resource_pools: count_named(&root, "ResourcePool"),
        resource_links: count_named(&root, "ResourceLink"),
        products: count_named(&root, "Product"),
        processes: count_named(&root, "Process")
            .saturating_add(count_named(&root, "ProcessGroup"))
            .saturating_add(count_named(&root, "JDF")),
        devices: count_named(&root, "Device"),
        media: count_named(&root, "Media"),
        layouts: count_named(&root, "Layout"),
        run_lists: count_named(&root, "RunList"),
        audits: count_named(&root, "Audit"),
        files: count_named(&root, "FileSpec"),
        urls: count_named(&root, "URLLink")
            .saturating_add(count_named(&root, "URL"))
            .saturating_add(count_named(&root, "FileSpec")),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Document",
        &summary.root,
        &format!(
            "version={} status={}",
            display_or_dash(&summary.version),
            display_or_dash(&summary.status)
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Nodes",
        &summary.jdf_nodes.to_string(),
        &format!(
            "products={} processes={} audits={}",
            summary.products, summary.processes, summary.audits
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Resources",
        &summary.resources.to_string(),
        &format!(
            "pools={} links={} devices={} media={}",
            summary.resource_pools, summary.resource_links, summary.devices, summary.media
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Print data",
        &summary.layouts.to_string(),
        &format!("runLists={} files={}", summary.run_lists, summary.files),
    )?;
    push_row(
        &mut summary.rows,
        "References",
        &summary.urls.to_string(),
        "URL/file links counted; targets not opened",
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: if xjdf {
                "XJDF job ticket".into()
            } else {
                "JDF job ticket".into()
            },
        },
        HtmlBlock::Paragraph {
            text: "CIP4 Job Definition Format describes print and production workflows. This preview reports inert structure and never sends commands to devices.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "JDF/JMF nodes, resources and process metadata are shown; job IDs, customer values, credentials, URLs and file payloads are omitted or redacted".into(),
        "JDF jobs, JMF messages, device commands, workflow execution, network requests and external files never run".into(),
    ];
    let mut page_sink = JdfPageSink {
        inner: sink,
        warnings: &warnings,
        source_format: if xjdf { "xjdf" } else { "jdf" },
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
    if value.len() <= MAX_JDF_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_JDF_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_JDF_ROWS {
        return Err(Error::LimitExceeded(format!(
            "JDF rows exceed {MAX_JDF_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
