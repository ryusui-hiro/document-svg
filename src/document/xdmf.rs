//! Bounded XDMF mesh-metadata previews.
//!
//! XDMF describes light XML metadata for heavy HDF5/Binary mesh arrays. This
//! adapter renders the XML model inventory only and never opens referenced
//! HDF5 files, evaluates functions or loads external paths.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XDMF_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XDMF_EVENTS: usize = 1_000_000;
const MAX_XDMF_NODES: usize = 500_000;
const MAX_XDMF_DEPTH: usize = 128;
const MAX_XDMF_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XDMF_ROWS: usize = 200_000;
const MAX_XDMF_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    domains: usize,
    grids: usize,
    topologies: usize,
    geometries: usize,
    attributes: usize,
    data_items: usize,
    times: usize,
    references: usize,
    functions: usize,
    rows: Vec<Vec<String>>,
}

struct XdmfPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for XdmfPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xdmf".into();
        if page.title.is_empty() {
            page.title = "XDMF mesh metadata".into();
        }
        page.description =
            "XDMF mesh metadata is rendered as bounded inert structure; referenced HDF5/Binary data are not opened".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"Xdmf", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("version=")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XDMF_BYTES),
        "XDMF input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XDMF_EVENTS),
            max_nodes: MAX_XDMF_NODES,
            max_depth: MAX_XDMF_DEPTH,
            max_text_bytes: MAX_XDMF_TEXT_BYTES,
        },
        "XDMF",
    )?;
    if !root.name.eq_ignore_ascii_case("Xdmf") {
        return Err(Error::InvalidInput("XDMF root must be <Xdmf>".into()));
    }
    let mut summary = Summary {
        version: root.attribute("Version").map(truncate).unwrap_or_default(),
        domains: count_named(&root, "Domain"),
        grids: count_named(&root, "Grid"),
        topologies: count_named(&root, "Topology"),
        geometries: count_named(&root, "Geometry"),
        attributes: count_named(&root, "Attribute"),
        data_items: count_named(&root, "DataItem"),
        times: count_named(&root, "Time"),
        references: count_named(&root, "Reference"),
        functions: count_functions(&root),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Model",
        &summary.version,
        &format!("domains={} grids={}", summary.domains, summary.grids),
    )?;
    push_row(
        &mut summary.rows,
        "Mesh",
        &summary.topologies.to_string(),
        &format!(
            "geometries={} attributes={}",
            summary.geometries, summary.attributes
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Data",
        &summary.data_items.to_string(),
        &format!("times={} references={}", summary.times, summary.references),
    )?;
    push_row(
        &mut summary.rows,
        "Functions",
        &summary.functions.to_string(),
        "DataItem function expressions omitted",
    )?;
    let blocks = vec![
        HtmlBlock::Heading { level: 1, text: "XDMF mesh metadata".into() },
        HtmlBlock::Paragraph { text: "eXtensible Data Model and Format structure is summarized without opening HDF5/Binary arrays or evaluating mesh functions.".into() },
        HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() }),
    ];
    let warnings = vec![
        "XDMF IDs, coordinates, topology arrays, attribute values, DataItem text, HDF5/Binary paths and functions are omitted or redacted".into(),
        "XDMF external HDF5/Binary references, XPath references, function evaluation, mesh loading and visualization engines never run".into(),
    ];
    let mut page_sink = XdmfPageSink {
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

fn count_functions(element: &XmlElement) -> usize {
    let own = element
        .attribute("ItemType")
        .is_some_and(|value| value.eq_ignore_ascii_case("Function"));
    usize::from(own) + element.children.iter().map(count_functions).sum::<usize>()
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_XDMF_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XDMF rows exceed {MAX_XDMF_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_XDMF_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XDMF_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
