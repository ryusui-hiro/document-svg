//! Bounded MODS (Metadata Object Description Schema) bibliographic previews.
//!
//! MODS carries titles, names, origin, subjects, identifiers and locations.
//! This adapter renders common descriptive metadata and counts while keeping
//! authority/value URIs and external resources inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_MODS_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MODS_XML_EVENTS: usize = 1_000_000;
const MAX_MODS_XML_NODES: usize = 500_000;
const MAX_MODS_XML_DEPTH: usize = 96;
const MAX_MODS_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_MODS_ROWS: usize = 200_000;
const MAX_MODS_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"mods", None)
        || crate::geospatial::xml_tree::looks_like_root(bytes, b"modsCollection", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<titleinfo") || text.contains("<datafield") || text.contains("<name")
}

struct ModsPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for ModsPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "mods".into();
        if page.title.is_empty() {
            page.title = "MODS bibliographic record".into();
        }
        page.description =
            "MODS descriptive metadata is rendered inertly; authority URIs, location URLs and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    records: usize,
    titles: usize,
    names: usize,
    subjects: usize,
    identifiers: usize,
    locations: usize,
    genres: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_MODS_BYTES),
        "MODS input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_MODS_XML_EVENTS),
            max_nodes: MAX_MODS_XML_NODES,
            max_depth: MAX_MODS_XML_DEPTH,
            max_text_bytes: MAX_MODS_TEXT_BYTES,
        },
        "MODS",
    )?;
    let records: Vec<&XmlElement> = if root.name.eq_ignore_ascii_case("mods") {
        vec![&root]
    } else if root.name.eq_ignore_ascii_case("modsCollection") {
        root.children_named("mods").collect()
    } else {
        return Err(Error::InvalidInput(
            "MODS XML root must be mods or modsCollection".into(),
        ));
    };
    let mut summary = Summary::default();
    for (index, record) in records.iter().enumerate() {
        collect_record(record, index + 1, &mut summary)?;
    }
    if summary.records == 0 {
        return Err(Error::InvalidInput(
            "MODS collection contains no records".into(),
        ));
    }
    let metadata = format!(
        "Records: {}\nTitle elements: {}\nName elements: {}\nSubjects: {}\nIdentifiers: {}\nLocations: {}\nGenres: {}",
        summary.records,
        summary.titles,
        summary.names,
        summary.subjects,
        summary.identifiers,
        summary.locations,
        summary.genres,
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "MODS bibliographic record".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["No.".into(), "Kind".into(), "Type".into(), "Value".into()],
            rows: summary.rows,
            alignments: vec![
                TableAlign::Right,
                TableAlign::Left,
                TableAlign::Left,
                TableAlign::Left,
            ],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "MODS titles, names, subjects, origin and structural metadata are shown; authority/value URIs, location URLs, notes and arbitrary extension values are omitted or redacted".into(),
        "MODS XML traversal and rendered rows are bounded; no catalog, schema, image, script or external resource is resolved".into(),
    ];
    let mut page_sink = ModsPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_record(record: &XmlElement, number: usize, summary: &mut Summary) -> Result<()> {
    summary.records = summary.records.saturating_add(1);
    for title_info in record.children_named("titleInfo") {
        summary.titles = summary.titles.saturating_add(1);
        let title = first_text(title_info, "title");
        let subtitle = first_text(title_info, "subTitle");
        let value = if subtitle.is_empty() {
            title
        } else {
            format!("{title}: {subtitle}")
        };
        push_element(summary, number, title_info, "title", value)?;
    }
    for name in record.children_named("name") {
        summary.names = summary.names.saturating_add(1);
        push_element(summary, number, name, "name", first_text(name, "namePart"))?;
    }
    for origin in record.children_named("originInfo") {
        for publisher in origin.children_named("publisher") {
            push_element(
                summary,
                number,
                publisher,
                "publisher",
                truncate(publisher.text.trim()),
            )?;
        }
        for date in origin.children_named("dateIssued") {
            push_element(
                summary,
                number,
                date,
                "dateIssued",
                truncate(date.text.trim()),
            )?;
        }
    }
    for subject in record.children_named("subject") {
        summary.subjects = summary.subjects.saturating_add(1);
        let value = first_text(subject, "topic");
        push_element(summary, number, subject, "subject", value)?;
    }
    for identifier in record.children_named("identifier") {
        summary.identifiers = summary.identifiers.saturating_add(1);
        let value = if identifier.text.contains("://") {
            "[URL omitted]".into()
        } else {
            truncate(identifier.text.trim())
        };
        push_element(summary, number, identifier, "identifier", value)?;
    }
    for location in record.children_named("location") {
        summary.locations = summary.locations.saturating_add(1);
        push_element(
            summary,
            number,
            location,
            "location",
            "external location omitted".into(),
        )?;
    }
    for genre in record.children_named("genre") {
        summary.genres = summary.genres.saturating_add(1);
        push_element(summary, number, genre, "genre", truncate(genre.text.trim()))?;
    }
    Ok(())
}

fn push_element(
    summary: &mut Summary,
    record: usize,
    element: &XmlElement,
    kind: &str,
    value: String,
) -> Result<()> {
    push_row(
        summary,
        record,
        kind,
        element.attribute("type").unwrap_or("—"),
        value,
    )
}

fn first_text(parent: &XmlElement, name: &str) -> String {
    parent
        .children_named(name)
        .next()
        .map(|child| truncate(child.text.trim()))
        .unwrap_or_default()
}

fn push_row(
    summary: &mut Summary,
    record: usize,
    kind: &str,
    type_name: &str,
    value: String,
) -> Result<()> {
    if summary.rows.len() >= MAX_MODS_ROWS {
        return Err(Error::LimitExceeded(format!(
            "MODS rendered rows exceed {MAX_MODS_ROWS}"
        )));
    }
    summary.rows.push(vec![
        record.to_string(),
        truncate(kind),
        truncate(type_name),
        truncate(&value),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_MODS_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_MODS_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_mods_roots() {
        assert!(looks_like_prefix(
            br#"<mods xmlns="http://www.loc.gov/mods/v3"><titleInfo><title>Book</title></titleInfo></mods>"#
        ));
        assert!(looks_like_prefix(
            br#"<modsCollection><mods><name><namePart>A</namePart></name></mods></modsCollection>"#
        ));
        assert!(!looks_like_prefix(br#"<mods><note>x</note></mods>"#));
    }

    #[test]
    fn redacts_identifier_urls() {
        let xml = br#"<mods><identifier type="uri">https://private.example.invalid/item</identifier></mods>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 100,
                max_nodes: 100,
                max_depth: 16,
                max_text_bytes: 1000,
            },
            "MODS",
        )
        .unwrap();
        let mut summary = Summary::default();
        collect_record(&root, 1, &mut summary).unwrap();
        assert_eq!(summary.rows[0][3], "[URL omitted]");
    }
}
