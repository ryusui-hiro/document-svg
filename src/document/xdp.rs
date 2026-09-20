//! Bounded Adobe XDP/XFA XML data-package previews.
//!
//! XDP packages combine XFA template, datasets, configuration and optional PDF
//! packets. This adapter reports packet and field structure without executing
//! XFA calculation/script/event logic or unpacking embedded documents.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XDP_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XDP_XML_EVENTS: usize = 1_000_000;
const MAX_XDP_XML_NODES: usize = 500_000;
const MAX_XDP_XML_DEPTH: usize = 128;
const MAX_XDP_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_XDP_ROWS: usize = 100_000;
const MAX_XDP_DISPLAY_BYTES: usize = 512;
const XDP_NAMESPACE: &str = "http://ns.adobe.com/xdp/";

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"xdp", None)
        && String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains("ns.adobe.com/xdp")
}

struct XdpPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for XdpPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xdp".into();
        if page.title.is_empty() {
            page.title = "XDP/XFA data package".into();
        }
        page.description = "XDP/XFA packet and field structure is rendered as a bounded inert summary; embedded PDF, scripts and form actions are not executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    packets: usize,
    template_packets: usize,
    dataset_packets: usize,
    pdf_packets: usize,
    fields: usize,
    subforms: usize,
    data_values: usize,
    scripts: usize,
    events: usize,
    submits: usize,
    external_refs: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XDP_BYTES),
        "XDP input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XDP_XML_EVENTS),
            max_nodes: MAX_XDP_XML_NODES,
            max_depth: MAX_XDP_XML_DEPTH,
            max_text_bytes: MAX_XDP_TEXT_BYTES,
        },
        "XDP",
    )?;
    if !root.name.eq_ignore_ascii_case("xdp") {
        return Err(Error::InvalidInput("XDP XML root must xdp".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace != XDP_NAMESPACE)
    {
        return Err(Error::InvalidInput(
            "XDP namespace is missing or unsupported".into(),
        ));
    }
    let mut summary = Summary::default();
    for packet in &root.children {
        if !matches!(
            packet.name.as_str(),
            "template"
                | "datasets"
                | "config"
                | "localeSet"
                | "connectionSet"
                | "sourceSet"
                | "pdf"
                | "xmpmeta"
                | "acroform"
        ) {
            continue;
        }
        summary.packets = summary.packets.saturating_add(1);
        if packet.name == "template" {
            summary.template_packets = summary.template_packets.saturating_add(1);
        }
        if packet.name == "datasets" {
            summary.dataset_packets = summary.dataset_packets.saturating_add(1);
        }
        if packet.name == "pdf" {
            summary.pdf_packets = summary.pdf_packets.saturating_add(1);
        }
        let detail = format!(
            "children={} attributes={}",
            packet.children.len(),
            packet.attributes.len()
        );
        push_row(&mut summary.rows, "Packet", &packet.name, &detail)?;
    }
    summary.fields = count_named(&root, "field");
    summary.subforms = count_named(&root, "subform");
    summary.data_values = count_named(&root, "value") + count_named(&root, "dataValue");
    summary.scripts = count_named(&root, "script")
        + count_named(&root, "calculate")
        + count_named(&root, "validate");
    summary.events = count_named(&root, "event");
    summary.submits = count_named(&root, "submit") + count_named(&root, "submitUrl");
    summary.external_refs =
        count_named(&root, "uri") + count_named(&root, "connection") + count_named(&root, "proto");
    for field in descendants_named(&root, "field")
        .into_iter()
        .take(MAX_XDP_ROWS.saturating_sub(summary.rows.len()))
    {
        let name = field.attribute("name").unwrap_or("[unnamed]");
        push_row(&mut summary.rows, "Field", name, "template field name only")?;
    }
    if summary.packets == 0 && summary.fields == 0 {
        return Err(Error::InvalidInput(
            "XDP package contains no template, dataset or field structure".into(),
        ));
    }
    let metadata = format!(
        "Packets: {} (template={} datasets={} pdf={})\nFields: {}\nSubforms: {}\nData values: {}\nScripts/calculations: {}\nEvents: {}\nSubmit/action nodes: {}\nExternal/reference nodes: {}",
        summary.packets,
        summary.template_packets,
        summary.dataset_packets,
        summary.pdf_packets,
        summary.fields,
        summary.subforms,
        summary.data_values,
        summary.scripts,
        summary.events,
        summary.submits,
        summary.external_refs
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "XDP/XFA data package".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "XDP packet, template/dataset field names and structural counts are shown; field values, embedded PDF/XML/binary packets, URLs, connection data and private payloads are omitted or redacted".into(),
        "XDP/XFA XML traversal and rows are bounded; DTD/entities, calculate/validate scripts, events, submit/uri actions, external resources, PDF rendering and XFA layout/recalculation never run".into(),
    ];
    let mut page_sink = XdpPageSink {
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
fn descendants_named<'a>(element: &'a XmlElement, name: &str) -> Vec<&'a XmlElement> {
    let mut result = Vec::new();
    for child in &element.children {
        if child.name.eq_ignore_ascii_case(name) {
            result.push(child);
        }
        result.extend(descendants_named(child, name));
    }
    result
}
fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_XDP_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XDP rows exceed {MAX_XDP_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_XDP_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XDP_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_xdp_namespace() {
        assert!(looks_like_prefix(
            br#"<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><xdp:template/></xdp:xdp>"#
        ));
    }
    #[test]
    fn rejects_generic_xdp() {
        assert!(!looks_like_prefix(br#"<xdp><xdp:template/></xdp>"#));
    }
}
