//! Bounded VTK PVD collection previews.
//!
//! A PVD file lists time steps and references to VTK datasets. This adapter
//! renders collection structure only; referenced `.vtu`/`.vtp`/`.vti` files and
//! remote paths are deliberately not opened.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_PVD_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PVD_EVENTS: usize = 1_000_000;
const MAX_PVD_NODES: usize = 500_000;
const MAX_PVD_DEPTH: usize = 128;
const MAX_PVD_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PVD_ROWS: usize = 200_000;
const MAX_PVD_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    version: String,
    collection_nodes: usize,
    datasets: usize,
    file_refs: usize,
    groups: usize,
    timesteps: usize,
    partitions: usize,
    remote_refs: usize,
    rows: Vec<Vec<String>>,
}

struct PvdPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for PvdPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "pvd".into();
        if page.title.is_empty() {
            page.title = "VTK PVD collection".into();
        }
        page.description = "VTK PVD collection structure is rendered as bounded inert metadata; referenced datasets are not opened".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("<vtkfile") && text.contains("type=\"collection\"")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PVD_BYTES),
        "PVD input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_PVD_EVENTS),
            max_nodes: MAX_PVD_NODES,
            max_depth: MAX_PVD_DEPTH,
            max_text_bytes: MAX_PVD_TEXT_BYTES,
        },
        "PVD",
    )?;
    if !root.name.eq_ignore_ascii_case("VTKFile") {
        return Err(Error::InvalidInput("PVD root must be <VTKFile>".into()));
    }
    if root
        .attribute("type")
        .is_none_or(|value| !value.eq_ignore_ascii_case("Collection"))
    {
        return Err(Error::InvalidInput(
            "PVD VTKFile type must be Collection".into(),
        ));
    }
    let mut summary = Summary {
        version: root.attribute("version").map(truncate).unwrap_or_default(),
        collection_nodes: count_named(&root, "Collection"),
        datasets: count_named(&root, "DataSet"),
        file_refs: count_attribute(&root, "file"),
        groups: count_attribute(&root, "group"),
        timesteps: count_attribute(&root, "timestep"),
        partitions: count_attribute(&root, "part"),
        remote_refs: count_remote_refs(&root),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Collection",
        &summary.datasets.to_string(),
        &format!(
            "version={} groups={}",
            display_or_dash(&summary.version),
            summary.groups
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Time",
        &summary.timesteps.to_string(),
        &format!(
            "partitions={} collectionNodes={}",
            summary.partitions, summary.collection_nodes
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Files",
        &summary.file_refs.to_string(),
        &format!("remoteRefs={} targets unopened", summary.remote_refs),
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "VTK PVD collection".into() }, HtmlBlock::Paragraph { text: "VTK collection metadata is summarized without opening referenced VTK datasets or following file paths.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["PVD timestep, group, part and file path values are omitted or redacted; referenced VTK data remain unopened".into(), "PVD external paths, URLs, dataset decoding and time-series rendering never run".into()];
    let mut page_sink = PvdPageSink {
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
fn count_attribute(element: &XmlElement, name: &str) -> usize {
    usize::from(
        element
            .attributes
            .keys()
            .any(|key| key.eq_ignore_ascii_case(name)),
    ) + element
        .children
        .iter()
        .map(|child| count_attribute(child, name))
        .sum::<usize>()
}
fn count_remote_refs(element: &XmlElement) -> usize {
    let own = element.attribute("file").is_some_and(|value| {
        value.starts_with("http://") || value.starts_with("https://") || value.starts_with("file:")
    });
    usize::from(own)
        + element
            .children
            .iter()
            .map(count_remote_refs)
            .sum::<usize>()
}
fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}
fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_PVD_ROWS {
        return Err(Error::LimitExceeded(format!(
            "PVD rows exceed {MAX_PVD_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_PVD_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_PVD_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
