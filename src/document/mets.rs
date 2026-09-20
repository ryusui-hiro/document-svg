//! Bounded METS archive-structure previews.
//!
//! METS describes digitized objects with file sections and nested structural
//! maps. This adapter renders those relationships and safe MIME/size metadata
//! only; FLocat/MDRef/MPTR targets, embedded payloads and external resources
//! are never dereferenced.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_METS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_METS_XML_EVENTS: usize = 1_000_000;
const MAX_METS_XML_NODES: usize = 500_000;
const MAX_METS_XML_DEPTH: usize = 96;
const MAX_METS_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_METS_ROWS: usize = 200_000;
const MAX_METS_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    if !crate::geospatial::xml_tree::looks_like_root(bytes, b"mets", None) {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<filesec") || text.contains("<structmap")
}

struct MetsPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for MetsPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "mets".into();
        if page.title.is_empty() {
            page.title = "METS archive structure".into();
        }
        page.description =
            "METS file and structural metadata is rendered inertly; locations, metadata references, payloads and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    label: String,
    objid: String,
    file_sections: usize,
    file_groups: usize,
    files: usize,
    struct_maps: usize,
    divisions: usize,
    file_pointers: usize,
    mets_pointers: usize,
    dmd_sections: usize,
    amd_sections: usize,
    struct_links: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_METS_BYTES),
        "METS input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_METS_XML_EVENTS),
            max_nodes: MAX_METS_XML_NODES,
            max_depth: MAX_METS_XML_DEPTH,
            max_text_bytes: MAX_METS_TEXT_BYTES,
        },
        "METS",
    )?;
    if !root.name.eq_ignore_ascii_case("mets") {
        return Err(Error::InvalidInput("METS XML root must be mets".into()));
    }
    let mut summary = Summary {
        label: root
            .attribute("LABEL")
            .or_else(|| root.attribute("label"))
            .unwrap_or("—")
            .to_owned(),
        objid: root
            .attribute("OBJID")
            .or_else(|| root.attribute("objid"))
            .unwrap_or("—")
            .to_owned(),
        ..Summary::default()
    };
    summary.file_sections = count_named(&root, "fileSec");
    summary.file_groups = count_named(&root, "fileGrp");
    summary.files = count_named(&root, "file");
    summary.struct_maps = count_named(&root, "structMap");
    summary.divisions = count_named(&root, "div");
    summary.file_pointers = count_named(&root, "fptr");
    summary.mets_pointers = count_named(&root, "mptr");
    summary.dmd_sections = count_named(&root, "dmdSec");
    summary.amd_sections = count_named(&root, "amdSec");
    summary.struct_links = count_named(&root, "structLink");
    for map in descendants_named(&root, "structMap") {
        let map_type = map
            .attribute("TYPE")
            .or_else(|| map.attribute("type"))
            .unwrap_or("—");
        push_row(
            &mut summary,
            "structMap",
            map_type,
            "hierarchy".into(),
            "—".into(),
        )?;
        for div in map.children_named("div") {
            collect_div(div, 1, &mut summary)?;
        }
    }
    for file in descendants_named(&root, "file") {
        let mime = file
            .attribute("MIMETYPE")
            .or_else(|| file.attribute("mimetype"))
            .unwrap_or("—");
        let size = file
            .attribute("SIZE")
            .or_else(|| file.attribute("size"))
            .unwrap_or("—");
        push_row(
            &mut summary,
            "file",
            mime,
            format!("size {size}"),
            "—".into(),
        )?;
    }
    if summary.rows.is_empty() {
        push_row(
            &mut summary,
            "METS",
            "—",
            "empty structure".into(),
            "—".into(),
        )?;
    }
    let metadata = format!(
        "Label: {}\nObject ID: {}\nFile sections: {}\nFile groups: {}\nFiles: {}\nStructural maps: {}\nDivisions: {}\nFile pointers: {}\nMETS pointers: {}\nDescriptive sections: {}\nAdministrative sections: {}\nStructural links: {}",
        truncate(&summary.label),
        truncate(&summary.objid),
        summary.file_sections,
        summary.file_groups,
        summary.files,
        summary.struct_maps,
        summary.divisions,
        summary.file_pointers,
        summary.mets_pointers,
        summary.dmd_sections,
        summary.amd_sections,
        summary.struct_links,
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "METS archive structure".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Kind".into(),
                "Name/Type".into(),
                "Detail".into(),
                "Value".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "METS file/structural metadata is shown; FLocat, MDRef, mptr/fptr targets, URLs and embedded payloads are omitted".into(),
        "METS XML traversal and rendered rows are bounded; no ALTO/image/file resource, script or external entity is opened".into(),
    ];
    let mut page_sink = MetsPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_div(element: &XmlElement, depth: usize, summary: &mut Summary) -> Result<()> {
    let kind = element
        .attribute("TYPE")
        .or_else(|| element.attribute("type"))
        .unwrap_or("div");
    let label = element
        .attribute("LABEL")
        .or_else(|| element.attribute("label"))
        .unwrap_or("—");
    let pointers = count_named(element, "fptr") + count_named(element, "mptr");
    push_row(
        summary,
        "div",
        kind,
        format!("depth {depth}, {pointers} pointers"),
        label.to_owned(),
    )?;
    for child in element.children_named("div") {
        collect_div(child, depth.saturating_add(1), summary)?;
    }
    Ok(())
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| {
            usize::from(child.name.eq_ignore_ascii_case(name))
                .saturating_add(count_named(child, name))
        })
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

fn push_row(
    summary: &mut Summary,
    kind: &str,
    name: &str,
    detail: String,
    value: String,
) -> Result<()> {
    if summary.rows.len() >= MAX_METS_ROWS {
        return Err(Error::LimitExceeded(format!(
            "METS rendered rows exceed {MAX_METS_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(kind),
        truncate(name),
        truncate(&detail),
        truncate(&value),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_METS_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_METS_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_mets_structural_root() {
        assert!(looks_like_prefix(
            br#"<mets xmlns="http://www.loc.gov/METS/"><fileSec/><structMap TYPE="PHYSICAL"><div/></structMap></mets>"#
        ));
        assert!(!looks_like_prefix(br#"<mets><metsHdr/></mets>"#));
    }

    #[test]
    fn counts_files_divisions_and_pointers() {
        let xml = br#"<mets LABEL="Book"><fileSec><fileGrp><file ID="F1" MIMETYPE="image/tiff" SIZE="12"/></fileGrp></fileSec><structMap TYPE="PHYSICAL"><div TYPE="page" LABEL="1"><fptr FILEID="F1"/><div TYPE="ocr"><mptr xlink:href="https://private.example.invalid/ocr"/></div></div></structMap></mets>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "METS",
        )
        .unwrap();
        assert_eq!(count_named(&root, "file"), 1);
        assert_eq!(count_named(&root, "div"), 2);
        assert_eq!(count_named(&root, "fptr"), 1);
        assert_eq!(count_named(&root, "mptr"), 1);
    }
}
