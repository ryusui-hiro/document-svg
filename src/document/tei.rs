//! Bounded TEI P5 scholarly-text previews.
//!
//! TEI documents can contain editions, apparatus, bibliographies, links and
//! arbitrary extension vocabularies. This adapter renders the common text
//! structure and header metadata only; external targets, facsimiles, scripts,
//! images and entity resources remain inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_TEI_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TEI_XML_EVENTS: usize = 1_000_000;
const MAX_TEI_XML_NODES: usize = 500_000;
const MAX_TEI_XML_DEPTH: usize = 128;
const MAX_TEI_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_TEI_ROWS: usize = 200_000;
const MAX_TEI_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(
        bytes,
        b"TEI",
        Some(b"http://www.tei-c.org/ns/1.0"),
    ) || crate::geospatial::xml_tree::looks_like_root(
        bytes,
        b"tei",
        Some(b"http://www.tei-c.org/ns/1.0"),
    )
}

struct TeiPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for TeiPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "tei".into();
        if page.title.is_empty() {
            page.title = "TEI scholarly text".into();
        }
        page.description =
            "TEI text and structural metadata is rendered inertly; external targets, facsimiles, images and scripts are not resolved or executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    title: String,
    authors: Vec<String>,
    date: String,
    divisions: usize,
    headings: usize,
    paragraphs: usize,
    notes: usize,
    lists: usize,
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
        options.max_input_bytes.min(MAX_TEI_BYTES),
        "TEI input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_TEI_XML_EVENTS),
            max_nodes: MAX_TEI_XML_NODES,
            max_depth: MAX_TEI_XML_DEPTH,
            max_text_bytes: MAX_TEI_TEXT_BYTES,
        },
        "TEI",
    )?;
    if !root.name.eq_ignore_ascii_case("tei") {
        return Err(Error::InvalidInput("TEI XML root must be TEI".into()));
    }
    let mut summary = Summary::default();
    if let Some(header) = root.children_named("teiHeader").next() {
        collect_header(header, &mut summary);
    }
    if let Some(text) = root.children_named("text").next() {
        walk_text(text, 0, &mut summary)?;
    } else {
        return Err(Error::InvalidInput(
            "TEI document requires a text element".into(),
        ));
    }
    let mut warnings = vec![
        "TEI titles, structural text, notes and counts are shown; targets, URLs, facsimiles, identifiers, arbitrary attributes and extension values are omitted".into(),
        "TEI XML traversal is bounded; DTD/entities, XInclude, external images, scripts and linked resources are never resolved or executed".into(),
    ];
    if summary.paragraphs == 0 && summary.headings == 0 {
        warnings.push("TEI text contains no renderable head or paragraph elements".into());
    }
    let authors = if summary.authors.is_empty() {
        "—".into()
    } else {
        summary.authors.join(", ")
    };
    let metadata = format!(
        "Title: {}\nAuthors: {}\nDate: {}\nDivisions: {}\nHeadings: {}\nParagraphs: {}\nNotes: {}\nLists: {}\nFigures: {}\nTables: {}",
        display_or_dash(&summary.title),
        truncate(&authors),
        display_or_dash(&summary.date),
        summary.divisions,
        summary.headings,
        summary.paragraphs,
        summary.notes,
        summary.lists,
        summary.figures,
        summary.tables,
    );
    let rows = if summary.rows.is_empty() {
        vec![vec!["—".into(), "—".into(), "—".into(), "0".into()]]
    } else {
        summary.rows
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "TEI scholarly text".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Depth".into(), "Text".into(), "N".into()],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = TeiPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(std::mem::take(&mut warnings))
}

fn collect_header(header: &XmlElement, summary: &mut Summary) {
    if let Some(file_desc) = header.children_named("fileDesc").next() {
        if let Some(title_stmt) = file_desc.children_named("titleStmt").next() {
            summary.title = first_text(title_stmt, "title");
            for author in title_stmt.children_named("author") {
                let name = first_text(author, "persName");
                let name = if name.is_empty() {
                    truncate(author.text.trim())
                } else {
                    name
                };
                if !name.is_empty() && summary.authors.len() < 1_000 {
                    summary.authors.push(name);
                }
            }
        }
        if let Some(publication) = file_desc.children_named("publicationStmt").next() {
            summary.date = first_text(publication, "date");
        }
    }
}

fn walk_text(element: &XmlElement, depth: usize, summary: &mut Summary) -> Result<()> {
    for child in &element.children {
        let next_depth = depth.saturating_add(1);
        match child.name.as_str() {
            "front" | "body" | "back" | "group" | "text" => {
                walk_text(child, depth, summary)?;
            }
            "div" | "div1" | "div2" | "div3" | "div4" | "div5" | "div6" | "div7" => {
                summary.divisions = summary.divisions.saturating_add(1);
                push_row(
                    summary,
                    "div",
                    next_depth,
                    display_or_dash(child.attribute("type").unwrap_or("")).to_owned(),
                )?;
                walk_text(child, next_depth, summary)?;
            }
            "head" => {
                summary.headings = summary.headings.saturating_add(1);
                push_row(summary, "head", next_depth, text_content(child))?;
            }
            "p" | "ab" | "sp" | "quote" | "cit" => {
                summary.paragraphs = summary.paragraphs.saturating_add(1);
                push_row(
                    summary,
                    child.name.as_str(),
                    next_depth,
                    text_content(child),
                )?;
                walk_text(child, next_depth, summary)?;
            }
            "item" | "entry" => {
                push_row(
                    summary,
                    child.name.as_str(),
                    next_depth,
                    text_content(child),
                )?;
                walk_text(child, next_depth, summary)?;
            }
            "note" | "noteGrp" => {
                summary.notes = summary.notes.saturating_add(1);
                push_row(summary, "note", next_depth, text_content(child))?;
                walk_text(child, next_depth, summary)?;
            }
            "list" | "listBibl" | "listPerson" | "listPlace" => {
                summary.lists = summary.lists.saturating_add(1);
                push_row(
                    summary,
                    "list",
                    next_depth,
                    display_or_dash(child.attribute("type").unwrap_or("")).to_owned(),
                )?;
                walk_text(child, next_depth, summary)?;
            }
            "figure" | "graphic" | "formula" => {
                summary.figures = summary.figures.saturating_add(1);
                push_row(
                    summary,
                    child.name.as_str(),
                    next_depth,
                    "external media omitted".into(),
                )?;
            }
            "table" => {
                summary.tables = summary.tables.saturating_add(1);
                let cells = count_descendants(child, &["cell"]);
                push_row(summary, "table", next_depth, format!("{cells} cells"))?;
            }
            _ => {
                walk_text(child, depth, summary)?;
            }
        }
    }
    Ok(())
}

fn count_descendants(element: &XmlElement, names: &[&str]) -> usize {
    let mut count = usize::from(names.iter().any(|name| *name == element.name));
    for child in &element.children {
        count = count.saturating_add(count_descendants(child, names));
    }
    count
}

fn push_row(summary: &mut Summary, kind: &str, depth: usize, text: String) -> Result<()> {
    if summary.rows.len() >= MAX_TEI_ROWS {
        return Err(Error::LimitExceeded(format!(
            "TEI rendered rows exceed {MAX_TEI_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(kind),
        depth.to_string(),
        truncate(&text),
        "1".into(),
    ]);
    Ok(())
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
        if child_text.is_empty() {
            continue;
        }
        while text.chars().last().is_some_and(char::is_whitespace) {
            text.pop();
        }
        if let Some(last) = text.pop() {
            if ".,;:!?".contains(last) {
                while text.chars().last().is_some_and(char::is_whitespace) {
                    text.pop();
                }
                text.push(' ');
                text.push_str(&child_text);
                text.push(last);
            } else {
                text.push(last);
                if !text.is_empty() {
                    text.push(' ');
                }
                text.push_str(&child_text);
            }
        } else {
            text.push_str(&child_text);
        }
    }
    truncate(&text)
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_TEI_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_TEI_DISPLAY_BYTES;
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
    fn recognizes_namespaced_tei_root() {
        assert!(looks_like_prefix(
            br#"<TEI xmlns="http://www.tei-c.org/ns/1.0"><text><body/></text></TEI>"#
        ));
        assert!(!looks_like_prefix(br#"<TEI><text/></TEI>"#));
    }

    #[test]
    fn walks_text_structure_and_counts_nodes() {
        let xml = br#"<TEI><teiHeader><fileDesc><titleStmt><title>Work</title><author><persName>Ada</persName></author></titleStmt></fileDesc></teiHeader><text><body><div type="chapter"><head>Intro</head><p>Hello <hi>world</hi>.</p><note>Note</note></div></body></text></TEI>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "TEI",
        )
        .unwrap();
        let mut summary = Summary::default();
        collect_header(
            root.children_named("teiHeader").next().unwrap(),
            &mut summary,
        );
        walk_text(root.children_named("text").next().unwrap(), 0, &mut summary).unwrap();
        assert_eq!(summary.title, "Work");
        assert_eq!(summary.authors, vec!["Ada"]);
        assert_eq!(summary.divisions, 1);
        assert_eq!(summary.headings, 1);
        assert_eq!(summary.paragraphs, 1);
        assert_eq!(summary.notes, 1);
    }
}
