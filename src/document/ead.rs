//! Bounded EAD2/EAD3 archival-finding-aid previews.
//!
//! EAD describes archival collections as nested components with descriptive
//! identification data. This adapter renders safe titles, dates and levels;
//! URLs, identifiers, digital objects and external resources remain inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_EAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EAD_XML_EVENTS: usize = 1_000_000;
const MAX_EAD_XML_NODES: usize = 500_000;
const MAX_EAD_XML_DEPTH: usize = 128;
const MAX_EAD_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_EAD_ROWS: usize = 200_000;
const MAX_EAD_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"ead", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<archdesc") || text.contains("<eadheader") || text.contains("<c01")
}

struct EadPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for EadPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "ead".into();
        if page.title.is_empty() {
            page.title = "EAD archival finding aid".into();
        }
        page.description =
            "EAD finding-aid structure is rendered inertly; digital-object URLs, identifiers and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    title: String,
    repository: String,
    components: usize,
    unit_dates: usize,
    notes: usize,
    digital_objects: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_EAD_BYTES),
        "EAD input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_EAD_XML_EVENTS),
            max_nodes: MAX_EAD_XML_NODES,
            max_depth: MAX_EAD_XML_DEPTH,
            max_text_bytes: MAX_EAD_TEXT_BYTES,
        },
        "EAD",
    )?;
    if !root.name.eq_ignore_ascii_case("ead") {
        return Err(Error::InvalidInput("EAD XML root must ead".into()));
    }
    let mut summary = Summary::default();
    if let Some(archdesc) = descendants_named(&root, "archdesc").first().copied() {
        if let Some(did) = archdesc.children_named("did").next() {
            summary.title = first_text(did, "unittitle");
            summary.repository = first_text(did, "repository");
            summary.unit_dates = count_named(did, "unitdate");
        }
        collect_component_children(archdesc, 0, &mut summary)?;
    }
    summary.notes = count_named(&root, "note") + count_named(&root, "scopecontent");
    summary.digital_objects = count_named(&root, "dao") + count_named(&root, "daogrp");
    if summary.components == 0 && summary.title.is_empty() {
        return Err(Error::InvalidInput(
            "EAD document contains no archival description or components".into(),
        ));
    }
    let metadata = format!(
        "Title: {}\nRepository: {}\nComponents: {}\nUnit dates: {}\nNotes/scope: {}\nDigital objects: {}",
        display_or_dash(&summary.title),
        display_or_dash(&summary.repository),
        summary.components,
        summary.unit_dates,
        summary.notes,
        summary.digital_objects,
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "EAD archival finding aid".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Kind".into(),
                "Level".into(),
                "Title".into(),
                "Detail".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "EAD finding-aid titles, dates, levels and component counts are shown; identifiers, URLs, digital-object paths and descriptive payloads are omitted or redacted".into(),
        "EAD XML traversal and rendered rows are bounded; external entities, XInclude, images, scripts and linked archival resources are never opened".into(),
    ];
    let mut page_sink = EadPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_component_children(
    element: &XmlElement,
    depth: usize,
    summary: &mut Summary,
) -> Result<()> {
    for child in &element.children {
        if is_component(&child.name) {
            summary.components = summary.components.saturating_add(1);
            let level = child.attribute("level").unwrap_or(child.name.as_str());
            let did = child.children_named("did").next();
            let title = did
                .map(|did| first_text(did, "unittitle"))
                .unwrap_or_default();
            let date = did
                .map(|did| first_text(did, "unitdate"))
                .unwrap_or_default();
            push_row(
                summary,
                "component",
                depth.saturating_add(1),
                &title,
                format!("{} {}", level, display_or_dash(&date)),
            )?;
            collect_component_children(child, depth.saturating_add(1), summary)?;
        } else {
            collect_component_children(child, depth, summary)?;
        }
    }
    Ok(())
}

fn is_component(name: &str) -> bool {
    name == "c"
        || (name.len() == 3
            && name.starts_with('c')
            && name[1..].chars().all(|ch| ch.is_ascii_digit()))
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

fn first_text(parent: &XmlElement, name: &str) -> String {
    parent
        .children_named(name)
        .next()
        .map(|child| safe_text(child.text.trim()))
        .unwrap_or_default()
}

fn safe_text(value: &str) -> String {
    if value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value)
    }
}

fn push_row(
    summary: &mut Summary,
    kind: &str,
    level: usize,
    title: &str,
    detail: String,
) -> Result<()> {
    if summary.rows.len() >= MAX_EAD_ROWS {
        return Err(Error::LimitExceeded(format!(
            "EAD rendered rows exceed {MAX_EAD_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(kind),
        level.to_string(),
        truncate(title),
        truncate(&detail),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_EAD_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_EAD_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_ead_roots() {
        assert!(looks_like_prefix(br#"<ead xmlns="urn:isbn:1-931666-22-9"><archdesc><did><unittitle>Collection</unittitle></did></archdesc></ead>"#));
        assert!(!looks_like_prefix(br#"<ead><control/></ead>"#));
    }

    #[test]
    fn counts_components_and_redacts_urls() {
        let xml = br#"<ead><archdesc><did><unittitle>Collection</unittitle><repository>Archive</repository></did><dsc><c01 level="series"><did><unittitle>Series One</unittitle><unitdate>1900-1901</unitdate></did><dao href="https://private.example.invalid/a"/></c01></dsc></archdesc></ead>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "EAD",
        )
        .unwrap();
        let mut summary = Summary::default();
        let archdesc = root.children_named("archdesc").next().unwrap();
        summary.title = first_text(archdesc.children_named("did").next().unwrap(), "unittitle");
        collect_component_children(archdesc, 0, &mut summary).unwrap();
        assert_eq!(summary.components, 1);
        assert_eq!(summary.title, "Collection");
    }
}
