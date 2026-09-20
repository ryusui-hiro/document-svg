//! Bounded OpenSearch Description Document previews.
//!
//! OpenSearch description files advertise search URL templates and supported
//! encodings. This adapter reports descriptor structure only; search URLs,
//! suggestions and network operations are never contacted.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_OPENSEARCH_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OPENSEARCH_EVENTS: usize = 500_000;
const MAX_OPENSEARCH_NODES: usize = 300_000;
const MAX_OPENSEARCH_DEPTH: usize = 96;
const MAX_OPENSEARCH_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_OPENSEARCH_ROWS: usize = 100_000;
const MAX_OPENSEARCH_DISPLAY_BYTES: usize = 512;
const OPENSEARCH_NAMESPACE: &str = "http://a9.com/-/spec/opensearch/1.1/";

#[derive(Default)]
struct Summary {
    urls: usize,
    queries: usize,
    languages: usize,
    input_encodings: usize,
    output_encodings: usize,
    images: usize,
    short_names: usize,
    descriptions: usize,
    syndication_rights: usize,
    developer: usize,
    contact: usize,
    rows: Vec<Vec<String>>,
}

struct OpenSearchPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for OpenSearchPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "opensearch".into();
        if page.title.is_empty() {
            page.title = "OpenSearch description".into();
        }
        page.description = "OpenSearch Description Document structure is rendered as bounded inert metadata; search templates and URLs are not contacted".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"OpenSearchDescription", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("a9.com/-/spec/opensearch/1.1")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_OPENSEARCH_BYTES),
        "OpenSearch input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_OPENSEARCH_EVENTS),
            max_nodes: MAX_OPENSEARCH_NODES,
            max_depth: MAX_OPENSEARCH_DEPTH,
            max_text_bytes: MAX_OPENSEARCH_TEXT_BYTES,
        },
        "OpenSearch",
    )?;
    if !root.name.eq_ignore_ascii_case("OpenSearchDescription") {
        return Err(Error::InvalidInput(
            "OpenSearch root must be <OpenSearchDescription>".into(),
        ));
    }
    if root.namespace.as_deref() != Some(OPENSEARCH_NAMESPACE) {
        return Err(Error::InvalidInput(
            "OpenSearch root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        urls: count_named(&root, "Url"),
        queries: count_named(&root, "Query"),
        languages: count_named(&root, "Language"),
        input_encodings: count_named(&root, "InputEncoding"),
        output_encodings: count_named(&root, "OutputEncoding"),
        images: count_named(&root, "Image"),
        short_names: count_named(&root, "ShortName"),
        descriptions: count_named(&root, "Description"),
        syndication_rights: count_named(&root, "SyndicationRight"),
        developer: count_named(&root, "Developer"),
        contact: count_named(&root, "Contact"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Search URLs",
        &summary.urls.to_string(),
        &format!("queries={} images={}", summary.queries, summary.images),
    )?;
    push_row(
        &mut summary.rows,
        "Locales",
        &summary.languages.to_string(),
        &format!(
            "inputEncodings={} outputEncodings={}",
            summary.input_encodings, summary.output_encodings
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Descriptor",
        &summary.short_names.to_string(),
        &format!(
            "descriptions={} developer={} contact={}",
            summary.descriptions, summary.developer, summary.contact
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Rights",
        &summary.syndication_rights.to_string(),
        "URL templates and values omitted",
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "OpenSearch description".into() }, HtmlBlock::Paragraph { text: "OpenSearch Description Document structure is summarized without contacting search services or URL templates.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["OpenSearch URL templates, query parameters, image URLs, descriptions, contact values and private metadata are omitted or redacted".into(), "OpenSearch suggestions, search requests, remote resources and URL templates never run".into()];
    let mut page_sink = OpenSearchPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn count_named(element: &XmlElement, name: &str) -> usize {
    element
        .children
        .iter()
        .map(|child| usize::from(child.name.eq_ignore_ascii_case(name)) + count_named(child, name))
        .sum()
}
fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_OPENSEARCH_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OpenSearch rows exceed {MAX_OPENSEARCH_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_OPENSEARCH_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_OPENSEARCH_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
