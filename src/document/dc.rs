//! Bounded Dublin Core XML metadata previews.
//!
//! Simple Dublin Core metadata is common in OAI-PMH, repositories and
//! digital-library exports. This adapter renders descriptive elements and
//! counts only; identifiers, relations, rights text and external URLs remain
//! inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_DC_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DC_XML_EVENTS: usize = 500_000;
const MAX_DC_XML_NODES: usize = 300_000;
const MAX_DC_XML_DEPTH: usize = 80;
const MAX_DC_TEXT_BYTES: usize = 24 * 1024 * 1024;
const MAX_DC_ROWS: usize = 100_000;
const MAX_DC_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"dc", None)
        || crate::geospatial::xml_tree::looks_like_root(bytes, b"metadata", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    let known_namespace = text.contains("purl.org/dc/") || text.contains("openarchives.org/oai");
    known_namespace && (text.contains(":title") || text.contains("<title"))
}

struct DcPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for DcPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "dublin-core".into();
        if page.title.is_empty() {
            page.title = "Dublin Core metadata".into();
        }
        page.description =
            "Dublin Core descriptive metadata is rendered inertly; identifiers, rights text and external URLs are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    records: usize,
    fields: usize,
    title_count: usize,
    creator_count: usize,
    subject_count: usize,
    url_count: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DC_BYTES),
        "Dublin Core input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_DC_XML_EVENTS),
            max_nodes: MAX_DC_XML_NODES,
            max_depth: MAX_DC_XML_DEPTH,
            max_text_bytes: MAX_DC_TEXT_BYTES,
        },
        "Dublin Core",
    )?;
    if !root.name.eq_ignore_ascii_case("dc") && !root.name.eq_ignore_ascii_case("metadata") {
        return Err(Error::InvalidInput(
            "Dublin Core XML root must dc or metadata".into(),
        ));
    }
    let records: Vec<&XmlElement> = if root.name.eq_ignore_ascii_case("dc") {
        vec![&root]
    } else {
        descendants_named(&root, "dc")
    };
    let records = if records.is_empty() {
        vec![&root]
    } else {
        records
    };
    let mut summary = Summary {
        records: records.len(),
        ..Summary::default()
    };
    for record in records {
        collect_record(record, &mut summary)?;
    }
    if summary.fields == 0 {
        return Err(Error::InvalidInput(
            "Dublin Core document contains no metadata elements".into(),
        ));
    }
    let metadata = format!(
        "Records: {}\nFields: {}\nTitles: {}\nCreators: {}\nSubjects: {}\nURL/identifier fields: {}",
        summary.records,
        summary.fields,
        summary.title_count,
        summary.creator_count,
        summary.subject_count,
        summary.url_count
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Dublin Core metadata".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Record".into(), "Element".into(), "Value".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Right, TableAlign::Left, TableAlign::Left],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "Dublin Core title/creator/subject and descriptive fields are shown; identifiers, relation URLs, rights text, descriptions and extension values are omitted or redacted".into(),
        "Dublin Core XML traversal and rendered rows are bounded; no OAI-PMH/catalog/network operation runs".into(),
    ];
    let mut page_sink = DcPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_record(record: &XmlElement, summary: &mut Summary) -> Result<()> {
    for child in &record.children {
        let element = child.name.as_str();
        if !is_dc_element(element) {
            continue;
        }
        summary.fields = summary.fields.saturating_add(1);
        match element {
            "title" => summary.title_count = summary.title_count.saturating_add(1),
            "creator" => summary.creator_count = summary.creator_count.saturating_add(1),
            "subject" => summary.subject_count = summary.subject_count.saturating_add(1),
            "identifier" | "relation" => summary.url_count = summary.url_count.saturating_add(1),
            _ => {}
        }
        let value = if matches!(element, "identifier" | "relation" | "rights")
            || child.text.contains("://")
        {
            "[value omitted]".into()
        } else if element == "description" {
            "[description omitted]".into()
        } else {
            truncate(child.text.trim())
        };
        push_row(summary, summary.records, element, value)?;
    }
    Ok(())
}

fn is_dc_element(name: &str) -> bool {
    matches!(
        name,
        "title"
            | "creator"
            | "subject"
            | "publisher"
            | "contributor"
            | "date"
            | "type"
            | "format"
            | "language"
            | "coverage"
            | "source"
            | "identifier"
            | "relation"
            | "rights"
            | "description"
    )
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

fn push_row(summary: &mut Summary, record: usize, element: &str, value: String) -> Result<()> {
    if summary.rows.len() >= MAX_DC_ROWS {
        return Err(Error::LimitExceeded(format!(
            "Dublin Core rows exceed {MAX_DC_ROWS}"
        )));
    }
    summary.rows.push(vec![
        record.to_string(),
        truncate(element),
        truncate(&value),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_DC_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_DC_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
