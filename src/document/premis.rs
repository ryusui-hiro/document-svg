//! Bounded PREMIS preservation-metadata previews.
//!
//! PREMIS models Objects, Events, Agents and Rights. This adapter renders
//! entity types, safe dates/names and relationship counts while omitting
//! checksums, URIs, detailed payloads and external resources.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_PREMIS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PREMIS_XML_EVENTS: usize = 1_000_000;
const MAX_PREMIS_XML_NODES: usize = 500_000;
const MAX_PREMIS_XML_DEPTH: usize = 96;
const MAX_PREMIS_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_PREMIS_ROWS: usize = 200_000;
const MAX_PREMIS_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"premis", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<object")
        || text.contains("<event")
        || text.contains("<agent")
        || text.contains("<rights")
}

struct PremisPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for PremisPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "premis".into();
        if page.title.is_empty() {
            page.title = "PREMIS preservation metadata".into();
        }
        page.description =
            "PREMIS entity and relationship metadata is rendered inertly; checksums, URIs, detailed payloads and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    version: String,
    objects: usize,
    events: usize,
    agents: usize,
    rights: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_PREMIS_BYTES),
        "PREMIS input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_PREMIS_XML_EVENTS),
            max_nodes: MAX_PREMIS_XML_NODES,
            max_depth: MAX_PREMIS_XML_DEPTH,
            max_text_bytes: MAX_PREMIS_TEXT_BYTES,
        },
        "PREMIS",
    )?;
    if !root.name.eq_ignore_ascii_case("premis") {
        return Err(Error::InvalidInput("PREMIS XML root must premis".into()));
    }
    let mut summary = Summary {
        version: root
            .attribute("version")
            .or_else(|| root.attribute("VERSION"))
            .unwrap_or("—")
            .to_owned(),
        ..Summary::default()
    };
    for object in descendants_named(&root, "object") {
        summary.objects = summary.objects.saturating_add(1);
        let kind = first_text(object, "objectCategory");
        let identifier = first_text(object, "objectIdentifierValue");
        push_row(
            &mut summary,
            "object",
            &kind,
            "identifier omitted",
            &identifier,
        )?;
    }
    for event in descendants_named(&root, "event") {
        summary.events = summary.events.saturating_add(1);
        let event_type = first_text(event, "eventType");
        let date = first_text(event, "eventDateTime");
        let links = descendants_named(event, "linkingObjectIdentifier").len()
            + descendants_named(event, "linkingAgentIdentifier").len();
        push_row(
            &mut summary,
            "event",
            &event_type,
            &format!("date {}, links {links}", display_or_dash(&date)),
            "detail omitted",
        )?;
    }
    for agent in descendants_named(&root, "agent") {
        summary.agents = summary.agents.saturating_add(1);
        let agent_type = first_text(agent, "agentType");
        let name = first_text(agent, "agentName");
        push_row(&mut summary, "agent", &agent_type, &name, "name shown")?;
    }
    for rights in descendants_named(&root, "rightsStatement") {
        summary.rights = summary.rights.saturating_add(1);
        let basis = first_text(rights, "rightsBasis");
        let acts = descendants_named(rights, "act").len();
        push_row(
            &mut summary,
            "rights",
            &basis,
            &format!("acts {acts}"),
            "statement omitted",
        )?;
    }
    if summary.objects + summary.events + summary.agents + summary.rights == 0 {
        return Err(Error::InvalidInput(
            "PREMIS document contains no object, event, agent or rightsStatement entities".into(),
        ));
    }
    let metadata = format!(
        "Version: {}\nObjects: {}\nEvents: {}\nAgents: {}\nRights statements: {}",
        summary.version, summary.objects, summary.events, summary.agents, summary.rights
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "PREMIS preservation metadata".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Entity".into(),
                "Type".into(),
                "Detail".into(),
                "Value".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "PREMIS object/event/agent/rights types and relationship counts are shown; identifiers, checksums, URIs, rights text and detailed payloads are omitted or redacted".into(),
        "PREMIS XML traversal is bounded; external entities, linked resources, applications and preservation actions are never executed".into(),
    ];
    let mut page_sink = PremisPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
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
    entity: &str,
    type_name: &str,
    detail: &str,
    value: &str,
) -> Result<()> {
    if summary.rows.len() >= MAX_PREMIS_ROWS {
        return Err(Error::LimitExceeded(format!(
            "PREMIS rendered rows exceed {MAX_PREMIS_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(entity),
        truncate(type_name),
        truncate(detail),
        truncate(value),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_PREMIS_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_PREMIS_DISPLAY_BYTES;
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
    fn recognizes_premis_root() {
        assert!(looks_like_prefix(
            br#"<premis xmlns="http://www.loc.gov/premis/v3"><object><objectCategory>file</objectCategory></object></premis>"#
        ));
        assert!(!looks_like_prefix(br#"<premis><extension/></premis>"#));
    }

    #[test]
    fn redacts_uri_values() {
        assert_eq!(
            safe_text("https://private.example.invalid/hash"),
            "[URL omitted]"
        );
        assert_eq!(safe_text("file"), "file");
    }
}
