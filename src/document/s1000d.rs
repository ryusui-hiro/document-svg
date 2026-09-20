//! Bounded S1000D technical-publication data-module previews.
//!
//! S1000D data modules combine a DMC identity with structured maintenance or
//! descriptive content. This adapter renders safe code metadata and common
//! text structure while keeping DM/ICN references and external resources inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_S1000D_BYTES: u64 = 64 * 1024 * 1024;
const MAX_S1000D_XML_EVENTS: usize = 1_000_000;
const MAX_S1000D_XML_NODES: usize = 500_000;
const MAX_S1000D_XML_DEPTH: usize = 128;
const MAX_S1000D_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_S1000D_ROWS: usize = 200_000;
const MAX_S1000D_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"dmodule", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<dmc") || text.contains("<dmaddress") || text.contains("<content")
}

struct S1000dPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for S1000dPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "s1000d".into();
        if page.title.is_empty() {
            page.title = "S1000D data module".into();
        }
        page.description =
            "S1000D data-module metadata and text are rendered inertly; DM/ICN references and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    dmc: String,
    title: String,
    issue: String,
    security: String,
    paragraphs: usize,
    steps: usize,
    notes: usize,
    figures: usize,
    tables: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_S1000D_BYTES),
        "S1000D input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_S1000D_XML_EVENTS),
            max_nodes: MAX_S1000D_XML_NODES,
            max_depth: MAX_S1000D_XML_DEPTH,
            max_text_bytes: MAX_S1000D_TEXT_BYTES,
        },
        "S1000D",
    )?;
    if !root.name.eq_ignore_ascii_case("dmodule") {
        return Err(Error::InvalidInput("S1000D XML root must dmodule".into()));
    }
    let mut summary = Summary::default();
    if let Some(dm_ident) = descendants_named(&root, "dmIdent").first().copied() {
        if let Some(dm_code) = dm_ident.children_named("dmCode").next() {
            summary.dmc = format_dmc(dm_code);
        }
        summary.issue = first_text(dm_ident, "issueInfo");
    }
    if let Some(dm_title) = descendants_named(&root, "dmTitle").first().copied() {
        summary.title = text_content(dm_title);
    }
    summary.security = descendants_named(&root, "security")
        .first()
        .map(|node| {
            node.attribute("securityClassification")
                .or_else(|| node.attribute("classification"))
                .unwrap_or("present")
                .to_owned()
        })
        .unwrap_or_default();
    if let Some(content) = descendants_named(&root, "content").first().copied() {
        walk_content(content, 0, &mut summary)?;
    }
    summary.figures = count_named(&root, "figure")
        + count_named(&root, "graphic")
        + count_named(&root, "hotspot");
    summary.tables = count_named(&root, "table");
    if summary.rows.is_empty() && summary.title.is_empty() {
        return Err(Error::InvalidInput(
            "S1000D data module contains no renderable content".into(),
        ));
    }
    let metadata = format!(
        "DMC: {}\nTitle: {}\nIssue: {}\nSecurity classification: {}\nParagraphs: {}\nSteps: {}\nNotes: {}\nFigures/graphics: {}\nTables: {}",
        display_or_dash(&summary.dmc),
        display_or_dash(&summary.title),
        display_or_dash(&summary.issue),
        display_or_dash(&summary.security),
        summary.paragraphs,
        summary.steps,
        summary.notes,
        summary.figures,
        summary.tables,
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "S1000D data module".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Kind".into(),
                "Depth".into(),
                "Text".into(),
                "Detail".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "S1000D DMC, issue/security metadata and common technical text are shown; DM/ICN references, URLs, identifiers, graphics and arbitrary extension values are omitted".into(),
        "S1000D XML traversal and rendered rows are bounded; XInclude, DTD/entities, scripts, external resources and publication assembly are never executed".into(),
    ];
    let mut page_sink = S1000dPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn format_dmc(dm_code: &XmlElement) -> String {
    const ATTRS: &[&str] = &[
        "modelIdentCode",
        "systemDiffCode",
        "systemCode",
        "subSystemCode",
        "subSubSystemCode",
        "assyCode",
        "disassyCode",
        "disassyCodeVariant",
        "infoCode",
        "infoCodeVariant",
        "itemLocationCode",
    ];
    let parts: Vec<String> = ATTRS
        .iter()
        .filter_map(|name| {
            dm_code
                .attribute(name)
                .map(|value| format!("{name}={value}"))
        })
        .collect();
    if parts.is_empty() {
        "present".into()
    } else {
        truncate(&parts.join(" "))
    }
}

fn walk_content(element: &XmlElement, depth: usize, summary: &mut Summary) -> Result<()> {
    for child in &element.children {
        let next = depth.saturating_add(1);
        match child.name.as_str() {
            "levelledPara" | "levelledParaSegment" | "topic" | "section" => {
                push_row(
                    summary,
                    child.name.as_str(),
                    next,
                    text_content(child),
                    "technical text".into(),
                )?;
                walk_content(child, next, summary)?;
            }
            "title" | "para" | "simplePara" | "description" | "warning" | "caution" | "note" => {
                if matches!(child.name.as_str(), "para" | "simplePara" | "description") {
                    summary.paragraphs = summary.paragraphs.saturating_add(1);
                }
                if child.name == "note" || child.name == "warning" || child.name == "caution" {
                    summary.notes = summary.notes.saturating_add(1);
                }
                push_row(
                    summary,
                    child.name.as_str(),
                    next,
                    text_content(child),
                    "text".into(),
                )?;
                walk_content(child, next, summary)?;
            }
            "step" | "proceduralStep" => {
                summary.steps = summary.steps.saturating_add(1);
                push_row(
                    summary,
                    "step",
                    next,
                    text_content(child),
                    "procedure".into(),
                )?;
                walk_content(child, next, summary)?;
            }
            "listItem" | "randomList" | "sequentialList" | "table" => {
                push_row(
                    summary,
                    child.name.as_str(),
                    next,
                    text_content(child),
                    "structure".into(),
                )?;
                walk_content(child, next, summary)?;
            }
            _ => walk_content(child, depth, summary)?,
        }
    }
    Ok(())
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
        .map(text_content)
        .unwrap_or_default()
}

fn text_content(element: &XmlElement) -> String {
    let mut text = element.text.trim().to_owned();
    for child in &element.children {
        let child_text = text_content(child);
        if !child_text.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&child_text);
        }
    }
    truncate(&text)
}

fn push_row(
    summary: &mut Summary,
    kind: &str,
    depth: usize,
    text: String,
    detail: String,
) -> Result<()> {
    if summary.rows.len() >= MAX_S1000D_ROWS {
        return Err(Error::LimitExceeded(format!(
            "S1000D rendered rows exceed {MAX_S1000D_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(kind),
        depth.to_string(),
        truncate(&text),
        truncate(&detail),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_S1000D_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_S1000D_DISPLAY_BYTES;
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
    fn recognizes_s1000d_data_module() {
        assert!(looks_like_prefix(br#"<dmodule xmlns="http://www.s1000d.org/S1000D_4-2"><identAndStatusSection><dmAddress><dmIdent><dmCode modelIdentCode="EXAMPLE"/></dmIdent></dmAddress></identAndStatusSection><content/></dmodule>"#));
        assert!(!looks_like_prefix(br#"<dmodule><metadata/></dmodule>"#));
    }

    #[test]
    fn formats_dmc_without_urls() {
        let xml = br#"<dmCode modelIdentCode="BIKE" systemCode="01" infoCode="040" itemLocationCode="A"/>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 100,
                max_nodes: 100,
                max_depth: 8,
                max_text_bytes: 1000,
            },
            "S1000D",
        )
        .unwrap();
        assert!(format_dmc(&root).contains("modelIdentCode=BIKE"));
    }
}
