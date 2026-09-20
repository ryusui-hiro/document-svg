//! Bounded Asset Administration Shell package (AASX) metadata previews.
//!
//! AASX is an OPC-style ZIP package. The root relationship points to an
//! aasx-origin part, whose relationships identify AAS specification parts
//! and supplementary files such as manuals or CAD models. This adapter reads
//! only the relationship metadata and bounded AAS XML/JSON specification
//! parts; supplementary files, URLs, signatures, encryption and active
//! content remain inert.

use std::collections::HashSet;
use std::io::{Cursor, Read, Seek};
use std::path::Path;

use serde_json::Value;
use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_AASX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_AASX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_AASX_EXPANDED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_AASX_ENTRIES: usize = 100_000;
const MAX_AASX_XML_EVENTS: usize = 500_000;
const MAX_AASX_XML_NODES: usize = 300_000;
const MAX_AASX_XML_DEPTH: usize = 96;
const MAX_AASX_JSON_DEPTH: usize = 100;
const MAX_AASX_JSON_VALUES: usize = 300_000;
const MAX_AASX_ROWS: usize = 100_000;
const MAX_AASX_DISPLAY_BYTES: usize = 512;

const AASX_ORIGIN_SUFFIX: &str = "aasx-origin";
const AAS_SPEC_SUFFIX: &str = "aas-spec";
const AAS_SUPPLEMENTARY_SUFFIX: &str = "aas-suppl";

#[derive(Default)]
struct Summary {
    spec_parts: usize,
    json_parts: usize,
    xml_parts: usize,
    shells: usize,
    submodels: usize,
    concepts: usize,
    elements: usize,
    files: usize,
    blobs: usize,
    supplementary: usize,
    thumbnails: usize,
    rows: Vec<Vec<String>>,
}

struct AasxPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for AasxPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "aasx".into();
        if page.title.is_empty() {
            page.title = "AASX asset package".into();
        }
        page.description = "AASX relationships and bounded Asset Administration Shell metadata are rendered; supplementary files and external resources are not opened".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_archive(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if metadata.len() > MAX_AASX_BYTES {
        return false;
    }
    let Ok(bytes) = read_limited_file(path, MAX_AASX_BYTES, "AASX sniff") else {
        return false;
    };
    looks_like_bytes(&bytes)
}

fn looks_like_bytes(bytes: &[u8]) -> bool {
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    if archive.len() > MAX_AASX_ENTRIES {
        return false;
    }
    let Ok(data) = read_named_entry(&mut archive, "_rels/.rels") else {
        return false;
    };
    let Ok(root) = parse_xml(&data) else {
        return false;
    };
    relationships(&root)
        .iter()
        .any(|(_, relation_type, _)| relation_type.ends_with(AASX_ORIGIN_SUFFIX))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_AASX_BYTES),
        "AASX input",
    )?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("invalid AASX archive: {error}")))?;
    if archive.len() > MAX_AASX_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "AASX entries exceed {MAX_AASX_ENTRIES}"
        )));
    }
    let mut expanded_bytes = 0_u64;
    let root_rels = read_named_entry(&mut archive, "_rels/.rels")?;
    reserve_expanded(&mut expanded_bytes, root_rels.len() as u64)?;
    let root = parse_xml(&root_rels)?;
    let root_relations = relationships(&root);
    let origin_target = root_relations
        .iter()
        .find(|(_, relation_type, _)| relation_type.ends_with(AASX_ORIGIN_SUFFIX))
        .map(|(_, _, target)| resolve_target("", target))
        .transpose()?
        .ok_or_else(|| Error::InvalidInput("AASX root has no aasx-origin relationship".into()))?;
    let origin_rels_name = relationship_part_name(&origin_target)?;
    let origin_rels = read_named_entry(&mut archive, &origin_rels_name)?;
    reserve_expanded(&mut expanded_bytes, origin_rels.len() as u64)?;
    let origin_root = parse_xml(&origin_rels)?;
    let origin_relations = relationships(&origin_root);
    let mut summary = Summary {
        thumbnails: root_relations
            .iter()
            .filter(|(_, relation_type, _)| relation_type.ends_with("metadata/thumbnail"))
            .count(),
        ..Summary::default()
    };
    let mut spec_targets = Vec::new();
    for (_, relation_type, target) in &origin_relations {
        if relation_type.ends_with(AAS_SPEC_SUFFIX) {
            spec_targets.push(resolve_target(&origin_target, target)?);
        } else if relation_type.ends_with(AAS_SUPPLEMENTARY_SUFFIX) {
            summary.supplementary = summary.supplementary.saturating_add(1);
        }
    }
    if spec_targets.is_empty() {
        for index in 0..archive.len() {
            let entry = archive.by_index(index)?;
            let name = entry.name().to_ascii_lowercase();
            if name.ends_with(".aas.json") || name.ends_with(".aas.xml") {
                spec_targets.push(entry.name().to_owned());
            }
        }
    }
    let mut seen_targets = HashSet::new();
    for target in spec_targets {
        if !seen_targets.insert(target.clone()) {
            continue;
        }
        let data = read_named_entry(&mut archive, &target)?;
        reserve_expanded(&mut expanded_bytes, data.len() as u64)?;
        summary.spec_parts = summary.spec_parts.saturating_add(1);
        if data
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(b'{')
        {
            summary.json_parts = summary.json_parts.saturating_add(1);
            summarize_json(&data, &mut summary)?;
        } else {
            summary.xml_parts = summary.xml_parts.saturating_add(1);
            summarize_xml(&data, options, &mut summary)?;
        }
    }
    if summary.spec_parts == 0 {
        return Err(Error::InvalidInput(
            "AASX contains no aas-spec XML or JSON part".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Package",
        "AASX",
        &format!(
            "spec_parts={} supplementary={} thumbnails={}",
            summary.spec_parts, summary.supplementary, summary.thumbnails
        ),
    )?;
    push_row(
        &mut summary.rows,
        "AAS",
        &summary.shells.to_string(),
        &format!(
            "submodels={} concepts={}",
            summary.submodels, summary.concepts
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Elements",
        &summary.elements.to_string(),
        &format!("files={} blobs={}", summary.files, summary.blobs),
    )?;
    let metadata = format!(
        "Specification parts: {}\nJSON parts: {}\nXML parts: {}\nAsset Administration Shells: {}\nSubmodels: {}\nConcept descriptions: {}\nSubmodel elements: {}\nFile elements: {}\nBlob elements: {}\nSupplementary files skipped: {}\nThumbnails skipped: {}",
        summary.spec_parts,
        summary.json_parts,
        summary.xml_parts,
        summary.shells,
        summary.submodels,
        summary.concepts,
        summary.elements,
        summary.files,
        summary.blobs,
        summary.supplementary,
        summary.thumbnails
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "AASX asset package".into(),
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
        "AASX specification metadata and relationship counts are shown; identifiers, property values, URLs, supplementary CAD/manual payloads, signatures and encryption material are omitted or redacted".into(),
        "AASX ZIP/XML/JSON traversal is bounded; supplementary files, external resources, network operations, scripts and digital-twin actions never run".into(),
    ];
    let mut page_sink = AasxPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn read_named_entry<R: Read + Seek>(archive: &mut ZipArchive<R>, name: &str) -> Result<Vec<u8>> {
    let entry = archive.by_name(name).map_err(|error| {
        Error::InvalidInput(format!("AASX is missing package part {name}: {error}"))
    })?;
    if entry.size() > MAX_AASX_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "AASX part {name} exceeds {MAX_AASX_ENTRY_BYTES} bytes"
        )));
    }
    let mut data = Vec::new();
    entry
        .take(MAX_AASX_ENTRY_BYTES.saturating_add(1))
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_AASX_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "AASX part {name} expanded beyond {MAX_AASX_ENTRY_BYTES} bytes"
        )));
    }
    Ok(data)
}

fn parse_xml(bytes: &[u8]) -> Result<XmlElement> {
    parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_AASX_XML_EVENTS,
            max_nodes: MAX_AASX_XML_NODES,
            max_depth: MAX_AASX_XML_DEPTH,
            max_text_bytes: 24 * 1024 * 1024,
        },
        "AASX",
    )
}

fn relationships(root: &XmlElement) -> Vec<(String, String, String)> {
    descendants_named(root, "Relationship")
        .into_iter()
        .filter_map(|node| {
            let id = attr_local(node, "Id").unwrap_or_default().to_owned();
            let relation_type = attr_local(node, "Type").unwrap_or_default().to_owned();
            let target = attr_local(node, "Target").unwrap_or_default().to_owned();
            (!relation_type.is_empty() && !target.is_empty()).then_some((id, relation_type, target))
        })
        .collect()
}

fn resolve_target(base: &str, target: &str) -> Result<String> {
    if target.is_empty() || target.contains('\0') || target.contains('\\') {
        return Err(Error::InvalidInput(
            "unsafe AASX relationship target".into(),
        ));
    }
    let mut parts = if target.starts_with('/') {
        Vec::new()
    } else {
        base.rsplit_once('/')
            .map(|(parent, _)| parent.split('/').collect())
            .unwrap_or_default()
    };
    for component in target.trim_start_matches('/').split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(Error::InvalidInput(
                        "AASX relationship escapes package root".into(),
                    ));
                }
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err(Error::InvalidInput("empty AASX relationship target".into()));
    }
    Ok(parts.join("/"))
}

fn relationship_part_name(source: &str) -> Result<String> {
    let (parent, file) = source.rsplit_once('/').unwrap_or(("", source));
    Ok(if parent.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{parent}/_rels/{file}.rels")
    })
}

fn summarize_xml(bytes: &[u8], options: &ConvertOptions, summary: &mut Summary) -> Result<()> {
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_AASX_XML_EVENTS),
            max_nodes: MAX_AASX_XML_NODES,
            max_depth: MAX_AASX_XML_DEPTH,
            max_text_bytes: 24 * 1024 * 1024,
        },
        "AASX specification",
    )?;
    summary.shells = summary
        .shells
        .saturating_add(count_named(&root, "assetAdministrationShell"));
    summary.submodels = summary
        .submodels
        .saturating_add(count_named(&root, "submodel"));
    summary.concepts = summary
        .concepts
        .saturating_add(count_named(&root, "conceptDescription"));
    summary.elements = summary
        .elements
        .saturating_add(count_named(&root, "submodelElement"));
    summary.files = summary.files.saturating_add(count_named(&root, "file"));
    summary.blobs = summary.blobs.saturating_add(count_named(&root, "blob"));
    Ok(())
}

fn summarize_json(bytes: &[u8], summary: &mut Summary) -> Result<()> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| {
        Error::InvalidInput(format!("invalid AASX JSON specification: {error}"))
    })?;
    let mut values = 0usize;
    let mut rows = Vec::new();
    walk_json(&value, 0, &mut values, summary, &mut rows)?;
    for (kind, label) in rows {
        push_row(
            &mut summary.rows,
            &kind,
            &label,
            "identifier/value payload omitted",
        )?;
    }
    Ok(())
}

fn walk_json(
    value: &Value,
    depth: usize,
    values: &mut usize,
    summary: &mut Summary,
    rows: &mut Vec<(String, String)>,
) -> Result<()> {
    if depth > MAX_AASX_JSON_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "AASX JSON nesting exceeds {MAX_AASX_JSON_DEPTH}"
        )));
    }
    *values = values.saturating_add(1);
    if *values > MAX_AASX_JSON_VALUES {
        return Err(Error::LimitExceeded(format!(
            "AASX JSON values exceed {MAX_AASX_JSON_VALUES}"
        )));
    }
    match value {
        Value::Object(map) => {
            if let Some(Value::String(id_short)) = map.get("idShort")
                && rows.len() < MAX_AASX_ROWS
            {
                rows.push(("idShort".into(), truncate(id_short)));
            }
            for (key, child) in map {
                match key.as_str() {
                    "assetAdministrationShells" if depth == 0 => {
                        summary.shells = summary.shells.saturating_add(array_len(child))
                    }
                    "submodels" if depth == 0 => {
                        summary.submodels = summary.submodels.saturating_add(array_len(child))
                    }
                    "conceptDescriptions" if depth == 0 => {
                        summary.concepts = summary.concepts.saturating_add(array_len(child))
                    }
                    "submodelElements" => {
                        summary.elements = summary.elements.saturating_add(array_len(child))
                    }
                    "file" => summary.files = summary.files.saturating_add(1),
                    "blob" => summary.blobs = summary.blobs.saturating_add(1),
                    _ => {}
                }
                walk_json(child, depth.saturating_add(1), values, summary, rows)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                walk_json(item, depth.saturating_add(1), values, summary, rows)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn array_len(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.len(),
        _ => 0,
    }
}

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
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

fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| usize::from(child.name.eq_ignore_ascii_case(name)) + count_named(child, name))
        .sum()
}

fn reserve_expanded(total: &mut u64, amount: u64) -> Result<()> {
    let next = total
        .checked_add(amount)
        .ok_or_else(|| Error::LimitExceeded("AASX expanded bytes overflow".into()))?;
    if next > MAX_AASX_EXPANDED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "AASX expanded parts exceed {MAX_AASX_EXPANDED_BYTES} bytes"
        )));
    }
    *total = next;
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_AASX_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_AASX_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_AASX_ROWS {
        return Err(Error::LimitExceeded(format!(
            "AASX rows exceed {MAX_AASX_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
