//! Bounded MARCXML (MARC 21 in XML) bibliographic-record previews.
//!
//! MARCXML records are represented as leader/controlfield/datafield/subfield
//! elements. This adapter renders field structure and safe text values while
//! omitting external URL values and never resolving catalog links.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_MARCXML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MARCXML_EVENTS: usize = 1_000_000;
const MAX_MARCXML_NODES: usize = 500_000;
const MAX_MARCXML_DEPTH: usize = 96;
const MAX_MARCXML_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_MARCXML_ROWS: usize = 200_000;
const MAX_MARCXML_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"collection", None)
        || crate::geospatial::xml_tree::looks_like_root(bytes, b"record", None);
    root && (text.contains("<datafield")
        || text.contains("<controlfield")
        || text.contains("<leader"))
}

struct MarcPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for MarcPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "marcxml".into();
        if page.title.is_empty() {
            page.title = "MARCXML record".into();
        }
        page.description =
            "MARCXML field and bibliographic metadata is rendered inertly; catalog URLs, payloads and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    records: usize,
    leaders: usize,
    controlfields: usize,
    datafields: usize,
    subfields: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_MARCXML_BYTES),
        "MARCXML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_MARCXML_EVENTS),
            max_nodes: MAX_MARCXML_NODES,
            max_depth: MAX_MARCXML_DEPTH,
            max_text_bytes: MAX_MARCXML_TEXT_BYTES,
        },
        "MARCXML",
    )?;
    if !root.name.eq_ignore_ascii_case("collection") && !root.name.eq_ignore_ascii_case("record") {
        return Err(Error::InvalidInput(
            "MARCXML root must be collection or record".into(),
        ));
    }
    let mut summary = Summary::default();
    let records = if root.name.eq_ignore_ascii_case("record") {
        vec![&root]
    } else {
        root.children_named("record").collect()
    };
    for (index, record) in records.iter().enumerate() {
        collect_record(record, index + 1, &mut summary)?;
    }
    if summary.records == 0 {
        return Err(Error::InvalidInput(
            "MARCXML collection contains no records".into(),
        ));
    }
    let metadata = format!(
        "Records: {}\nLeaders: {}\nControl fields: {}\nData fields: {}\nSubfields: {}",
        summary.records,
        summary.leaders,
        summary.controlfields,
        summary.datafields,
        summary.subfields
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "MARCXML record preview".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Record".into(), "Tag".into(), "Code".into(), "Value".into()],
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
        "MARCXML tags, indicators and safe text values are shown; identifiers, URL values, catalog links and extension payloads are omitted or redacted".into(),
        "MARCXML XML traversal and rendered rows are bounded; no catalog, schema, image, script or external entity is resolved".into(),
    ];
    let mut page_sink = MarcPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_record(record: &XmlElement, number: usize, summary: &mut Summary) -> Result<()> {
    summary.records = summary.records.saturating_add(1);
    if let Some(leader) = record.children_named("leader").next() {
        summary.leaders = summary.leaders.saturating_add(1);
        push_row(summary, number, "leader", "—", truncate(leader.text.trim()))?;
    }
    for field in &record.children {
        match field.name.as_str() {
            "controlfield" => {
                summary.controlfields = summary.controlfields.saturating_add(1);
                let tag = field.attribute("tag").unwrap_or("—");
                push_row(
                    summary,
                    number,
                    tag,
                    "—",
                    safe_value(field.text.trim(), tag),
                )?;
            }
            "datafield" => {
                summary.datafields = summary.datafields.saturating_add(1);
                let tag = field.attribute("tag").unwrap_or("—");
                let indicators = format!(
                    "{}{}",
                    field.attribute("ind1").unwrap_or("—"),
                    field.attribute("ind2").unwrap_or("—")
                );
                for subfield in field.children_named("subfield") {
                    summary.subfields = summary.subfields.saturating_add(1);
                    let code = subfield.attribute("code").unwrap_or("—");
                    push_row(
                        summary,
                        number,
                        tag,
                        code,
                        safe_value(subfield.text.trim(), tag),
                    )?;
                }
                if field.children_named("subfield").next().is_none() {
                    push_row(
                        summary,
                        number,
                        tag,
                        indicators.as_str(),
                        "no subfields".into(),
                    )?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn push_row(
    summary: &mut Summary,
    record: usize,
    tag: &str,
    code: &str,
    value: String,
) -> Result<()> {
    if summary.rows.len() >= MAX_MARCXML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "MARCXML rendered rows exceed {MAX_MARCXML_ROWS}"
        )));
    }
    summary.rows.push(vec![
        record.to_string(),
        truncate(tag),
        truncate(code),
        truncate(&value),
    ]);
    Ok(())
}

fn safe_value(value: &str, tag: &str) -> String {
    if tag == "856" || value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value)
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_MARCXML_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_MARCXML_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_marcxml_collection_and_record() {
        assert!(looks_like_prefix(
            br#"<collection xmlns="http://www.loc.gov/MARC21/slim"><record><leader>abc</leader><controlfield tag="001">1</controlfield></record></collection>"#
        ));
        assert!(!looks_like_prefix(
            br#"<collection><record><foo/></record></collection>"#
        ));
    }

    #[test]
    fn redacts_catalog_urls() {
        assert_eq!(
            safe_value("https://private.example.invalid/item", "856"),
            "[URL omitted]"
        );
        assert_eq!(safe_value("A title", "245"), "A title");
    }
}
