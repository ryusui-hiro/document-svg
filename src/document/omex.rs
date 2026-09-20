//! Bounded COMBINE/OMEX scientific-model archive previews.
//!
//! OMEX packages may contain SBML, CellML and SED-ML models plus metadata.
//! This adapter reads only ZIP/manifest structure in memory; archive members,
//! model references and simulation experiments are not executed or extracted.

use std::io::{Cursor, Read, Seek};
use std::path::Path;

use zip::ZipArchive;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_OMEX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_OMEX_ENTRY_BYTES: u64 = 32 * 1024 * 1024;
const MAX_OMEX_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_OMEX_ENTRIES: usize = 100_000;
const MAX_OMEX_XML_EVENTS: usize = 500_000;
const MAX_OMEX_XML_NODES: usize = 300_000;
const MAX_OMEX_XML_DEPTH: usize = 128;
const MAX_OMEX_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_OMEX_ROWS: usize = 200_000;
const MAX_OMEX_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    entries: usize,
    manifest_parts: usize,
    content_entries: usize,
    model_entries: usize,
    sedml_entries: usize,
    sbml_entries: usize,
    cellml_entries: usize,
    metadata_entries: usize,
    external_entries: usize,
    xml_entries: usize,
    json_entries: usize,
    rows: Vec<Vec<String>>,
}

struct OmexPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OmexPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "omex".into();
        if page.title.is_empty() {
            page.title = "COMBINE/OMEX archive".into();
        }
        page.description =
            "COMBINE/OMEX archive structure is rendered as bounded inert metadata; models and simulations are not extracted or executed".into();
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
    if metadata.len() > MAX_OMEX_BYTES {
        return false;
    }
    let Ok(bytes) = read_limited_file(path, MAX_OMEX_BYTES, "OMEX sniff") else {
        return false;
    };
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    if archive.len() > MAX_OMEX_ENTRIES {
        return false;
    }
    (0..archive.len()).any(|index| {
        archive
            .by_index(index)
            .ok()
            .map(|entry| {
                let name = entry.name().to_ascii_lowercase();
                name == "manifest.xml" || name == "meta-inf/manifest.xml"
            })
            .unwrap_or(false)
    })
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_OMEX_BYTES),
        "OMEX input",
    )?;
    let mut archive = ZipArchive::new(Cursor::new(bytes))
        .map_err(|error| Error::InvalidInput(format!("invalid OMEX ZIP archive: {error}")))?;
    if archive.len() > MAX_OMEX_ENTRIES {
        return Err(Error::LimitExceeded(format!(
            "OMEX entries exceed {MAX_OMEX_ENTRIES}"
        )));
    }
    let mut summary = Summary {
        entries: archive.len(),
        ..Summary::default()
    };
    let mut expanded_bytes = 0_u64;
    let mut manifest_name = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        validate_entry_name(&name)?;
        let lower = name.to_ascii_lowercase();
        if lower == "manifest.xml" || lower == "meta-inf/manifest.xml" {
            summary.manifest_parts = summary.manifest_parts.saturating_add(1);
            manifest_name = Some(name.clone());
        }
        if lower.ends_with(".xml") {
            summary.xml_entries = summary.xml_entries.saturating_add(1);
        }
        if lower.ends_with(".json") || lower.ends_with(".jsonld") {
            summary.json_entries = summary.json_entries.saturating_add(1);
        }
    }
    let Some(manifest_name) = manifest_name else {
        return Err(Error::InvalidInput(
            "OMEX archive has no manifest.xml".into(),
        ));
    };
    let manifest = read_entry(&mut archive, &manifest_name, &mut expanded_bytes)?;
    let manifest_root = parse_xml_tree(
        &manifest,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_OMEX_XML_EVENTS),
            max_nodes: MAX_OMEX_XML_NODES,
            max_depth: MAX_OMEX_XML_DEPTH,
            max_text_bytes: MAX_OMEX_TEXT_BYTES,
        },
        "OMEX manifest",
    )?;
    if !manifest_root.name.eq_ignore_ascii_case("omexManifest") {
        return Err(Error::InvalidInput(
            "OMEX manifest root must be <omexManifest>".into(),
        ));
    }
    summarize_manifest(&manifest_root, &mut summary);
    push_row(
        &mut summary.rows,
        "Archive",
        &summary.entries.to_string(),
        &format!(
            "manifest={} xml={} json={}",
            summary.manifest_parts, summary.xml_entries, summary.json_entries
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Content",
        &summary.content_entries.to_string(),
        &format!(
            "models={} SED-ML={} metadata={}",
            summary.model_entries, summary.sedml_entries, summary.metadata_entries
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Model formats",
        &format!(
            "SBML={} CellML={}",
            summary.sbml_entries, summary.cellml_entries
        ),
        &format!(
            "external references={} targets unopened",
            summary.external_entries
        ),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "COMBINE/OMEX archive".into(),
        },
        HtmlBlock::Paragraph {
            text: "OMEX scientific-model package structure is summarized without extracting or executing models and simulation experiments.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "OMEX manifest locations, model IDs, URLs, archive member payloads and metadata values are omitted or redacted; only bounded package counts are shown".into(),
        "OMEX archive extraction, SBML/CellML/SED-ML execution, external references, scripts, network resources and simulation engines never run".into(),
    ];
    let mut page_sink = OmexPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    expanded_bytes: &mut u64,
) -> Result<Vec<u8>> {
    let mut entry = archive.by_name(name)?;
    if entry.size() > MAX_OMEX_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OMEX entry '{name}' exceeds {MAX_OMEX_ENTRY_BYTES} bytes"
        )));
    }
    let mut data = Vec::new();
    entry
        .by_ref()
        .take(MAX_OMEX_ENTRY_BYTES.saturating_add(1))
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_OMEX_ENTRY_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OMEX entry '{name}' expands beyond {MAX_OMEX_ENTRY_BYTES} bytes"
        )));
    }
    *expanded_bytes = expanded_bytes.saturating_add(data.len() as u64);
    if *expanded_bytes > MAX_OMEX_EXPANDED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OMEX expanded metadata exceeds {MAX_OMEX_EXPANDED_BYTES} bytes"
        )));
    }
    Ok(data)
}

fn validate_entry_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if path.is_absolute()
        || name.starts_with('\\')
        || name.split('/').any(|component| component == "..")
    {
        return Err(Error::InvalidInput(format!(
            "OMEX entry path escapes package root: {name}"
        )));
    }
    Ok(())
}

fn summarize_manifest(root: &XmlElement, summary: &mut Summary) {
    for child in &root.children {
        if child.name.eq_ignore_ascii_case("content") {
            summary.content_entries = summary.content_entries.saturating_add(1);
            let format = child
                .attributes
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("format"))
                .map(|(_, value)| value.to_ascii_lowercase())
                .unwrap_or_default();
            let location = child
                .attributes
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("location"))
                .map(|(_, value)| value.to_ascii_lowercase())
                .unwrap_or_default();
            if format.contains("sed-ml") {
                summary.sedml_entries = summary.sedml_entries.saturating_add(1);
            } else if format.contains("sbml") {
                summary.sbml_entries = summary.sbml_entries.saturating_add(1);
                summary.model_entries = summary.model_entries.saturating_add(1);
            } else if format.contains("cellml") {
                summary.cellml_entries = summary.cellml_entries.saturating_add(1);
                summary.model_entries = summary.model_entries.saturating_add(1);
            } else if format.contains("metadata")
                || format.contains("rdf")
                || format.contains("jsonld")
            {
                summary.metadata_entries = summary.metadata_entries.saturating_add(1);
            } else if location.starts_with("http://")
                || location.starts_with("https://")
                || format.contains("example.invalid")
            {
                summary.external_entries = summary.external_entries.saturating_add(1);
            }
        }
        summarize_manifest(child, summary);
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_OMEX_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OMEX rows exceed {MAX_OMEX_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OMEX_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_OMEX_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
