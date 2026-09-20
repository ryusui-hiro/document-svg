//! Bounded Adobe XMP (ISO 16684-1) metadata previews.
//!
//! XMP packets use RDF/XML and can be embedded in PDF, image, video and Office
//! files. This adapter exposes common descriptive properties and counts while
//! omitting identifiers, thumbnails, URLs and arbitrary private payloads.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XMP_BYTES: u64 = 32 * 1024 * 1024;
const MAX_XMP_XML_EVENTS: usize = 500_000;
const MAX_XMP_XML_NODES: usize = 300_000;
const MAX_XMP_XML_DEPTH: usize = 96;
const MAX_XMP_TEXT_BYTES: usize = 24 * 1024 * 1024;
const MAX_XMP_ROWS: usize = 100_000;
const MAX_XMP_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root = crate::geospatial::xml_tree::looks_like_root(bytes, b"xmpmeta", None)
        || crate::geospatial::xml_tree::looks_like_root(bytes, b"RDF", None);
    if !root {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("adobe:ns:meta") || text.contains("ns.adobe.com/xap/")
}

struct XmpPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for XmpPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xmp".into();
        if page.title.is_empty() {
            page.title = "XMP metadata".into();
        }
        page.description = "XMP RDF metadata is rendered as a bounded inert summary; thumbnails, identifiers, URLs and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    wrapper: bool,
    properties: usize,
    descriptive: usize,
    titles: usize,
    creators: usize,
    dates: usize,
    private_omitted: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XMP_BYTES),
        "XMP input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XMP_XML_EVENTS),
            max_nodes: MAX_XMP_XML_NODES,
            max_depth: MAX_XMP_XML_DEPTH,
            max_text_bytes: MAX_XMP_TEXT_BYTES,
        },
        "XMP",
    )?;
    let wrapper = root.name.eq_ignore_ascii_case("xmpmeta");
    if !wrapper && !root.name.eq_ignore_ascii_case("RDF") {
        return Err(Error::InvalidInput("XMP root must xmpmeta or RDF".into()));
    }
    if !contains_xmp_namespace(&root) {
        return Err(Error::InvalidInput(
            "XMP namespace is missing or unsupported".into(),
        ));
    }
    let mut summary = Summary {
        wrapper,
        ..Summary::default()
    };
    let property_names = [
        ("title", "Title"),
        ("creator", "Creator"),
        ("subject", "Subject"),
        ("CreateDate", "Create date"),
        ("ModifyDate", "Modify date"),
        ("MetadataDate", "Metadata date"),
        ("CreatorTool", "Creator tool"),
        ("Producer", "Producer"),
        ("Format", "Format"),
        ("Label", "Label"),
        ("Rating", "Rating"),
        ("PageCount", "Page count"),
    ];
    for (name, label) in property_names {
        for node in descendants_named(&root, name) {
            let value = safe_text(&text_content(node));
            if value.is_empty() {
                continue;
            }
            summary.properties = summary.properties.saturating_add(1);
            summary.descriptive = summary.descriptive.saturating_add(1);
            if name.eq_ignore_ascii_case("title") {
                summary.titles = summary.titles.saturating_add(1);
            }
            if name.eq_ignore_ascii_case("creator") {
                summary.creators = summary.creators.saturating_add(1);
            }
            if name.to_ascii_lowercase().contains("date") {
                summary.dates = summary.dates.saturating_add(1);
            }
            push_row(&mut summary.rows, label, &value, "XMP property")?;
        }
    }
    for name in [
        "Identifier",
        "DocumentID",
        "InstanceID",
        "OriginalDocumentID",
        "Thumbnail",
        "Rights",
        "BaseURL",
    ] {
        summary.private_omitted = summary
            .private_omitted
            .saturating_add(count_named(&root, name));
    }
    if summary.properties == 0 {
        return Err(Error::InvalidInput(
            "XMP packet contains no supported descriptive properties".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Packet",
        if summary.wrapper { "xmpmeta" } else { "RDF" },
        &format!(
            "properties={} omitted={}",
            summary.properties, summary.private_omitted
        ),
    )?;
    let metadata = format!(
        "Packet wrapper: {}\nProperties shown: {}\nTitles: {}\nCreators: {}\nDates: {}\nPrivate/identifier properties omitted: {}",
        if summary.wrapper { "xmpmeta" } else { "RDF" },
        summary.properties,
        summary.titles,
        summary.creators,
        summary.dates,
        summary.private_omitted
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "XMP metadata".into(),
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
        "XMP descriptive properties and packet counts are shown; identifiers, rights/description payloads, thumbnails, URLs, arbitrary private schemas and embedded binary data are omitted or redacted".into(),
        "XMP XML traversal and rows are bounded; DTD/entities, scripts, external schemas/resources, URL dereferencing and metadata reconciliation never run".into(),
    ];
    let mut page_sink = XmpPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn contains_xmp_namespace(element: &XmlElement) -> bool {
    element.namespace.as_deref().is_some_and(|namespace| {
        namespace.contains("adobe:ns:meta")
            || namespace.contains("ns.adobe.com/xap/")
            || namespace.contains("purl.org/dc/elements/1.1")
    }) || element.children.iter().any(contains_xmp_namespace)
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
fn text_content(element: &XmlElement) -> String {
    let mut parts = Vec::new();
    if !element.text.trim().is_empty() {
        parts.push(element.text.trim().to_owned());
    }
    for child in &element.children {
        let value = text_content(child);
        if !value.is_empty() {
            parts.push(value);
        }
    }
    parts.join(" ")
}
fn safe_text(value: &str) -> String {
    if value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value.trim())
    }
}
fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_XMP_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XMP rows exceed {MAX_XMP_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_XMP_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XMP_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_xmp_wrapper() {
        assert!(looks_like_prefix(br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/></x:xmpmeta>"#));
    }
    #[test]
    fn rejects_generic_rdf() {
        assert!(!looks_like_prefix(
            br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>"#
        ));
    }
}
