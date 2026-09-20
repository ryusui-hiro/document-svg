//! Bounded ONIX for Books product-metadata previews.
//!
//! ONIX messages carry commercial and bibliographic data for publishing
//! supply chains. This adapter renders product structure and code-element
//! counts without exposing ISBNs, prices, titles, descriptions or URLs.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_ONIX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ONIX_EVENTS: usize = 1_000_000;
const MAX_ONIX_NODES: usize = 500_000;
const MAX_ONIX_DEPTH: usize = 128;
const MAX_ONIX_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ONIX_ROWS: usize = 200_000;
const MAX_ONIX_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    release: String,
    products: usize,
    identifiers: usize,
    descriptive: usize,
    collateral: usize,
    publishing_status: usize,
    supply: usize,
    contributors: usize,
    languages: usize,
    subjects: usize,
    measures: usize,
    prices: usize,
    text_content: usize,
    resources: usize,
    rows: Vec<Vec<String>>,
}

struct OnixPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OnixPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "onix".into();
        if page.title.is_empty() {
            page.title = "ONIX for Books message".into();
        }
        page.description =
            "ONIX book-product metadata structure is rendered as bounded inert metadata; private publishing values and links are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"ONIXMessage", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("ns.editeur.org/onix")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ONIX_BYTES),
        "ONIX input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_ONIX_EVENTS),
            max_nodes: MAX_ONIX_NODES,
            max_depth: MAX_ONIX_DEPTH,
            max_text_bytes: MAX_ONIX_TEXT_BYTES,
        },
        "ONIX",
    )?;
    if !root.name.eq_ignore_ascii_case("ONIXMessage") {
        return Err(Error::InvalidInput(
            "ONIX root must be <ONIXMessage>".into(),
        ));
    }
    if root.namespace.as_deref().is_none_or(|namespace| {
        !namespace
            .to_ascii_lowercase()
            .contains("ns.editeur.org/onix")
    }) {
        return Err(Error::InvalidInput(
            "ONIX root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        release: attr_local(&root, "release")
            .map(truncate)
            .unwrap_or_default(),
        products: count_named(&root, "Product"),
        identifiers: count_named(&root, "ProductIdentifier"),
        descriptive: count_named(&root, "DescriptiveDetail"),
        collateral: count_named(&root, "CollateralDetail"),
        publishing_status: count_named(&root, "PublishingStatus"),
        supply: count_named(&root, "SupplyDetail"),
        contributors: count_named(&root, "Contributor"),
        languages: count_named(&root, "Language"),
        subjects: count_named(&root, "Subject"),
        measures: count_named(&root, "Measure"),
        prices: count_named(&root, "Price"),
        text_content: count_named(&root, "TextContent") + count_named(&root, "TitleText"),
        resources: count_named(&root, "SupportingResource") + count_named(&root, "ResourceVersion"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Message",
        &root.name,
        &format!("release={}", display_or_dash(&summary.release)),
    )?;
    push_row(
        &mut summary.rows,
        "Products",
        &summary.products.to_string(),
        &format!(
            "identifiers={} publishingStatus={}",
            summary.identifiers, summary.publishing_status
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Description",
        &summary.descriptive.to_string(),
        &format!(
            "collateral={} textContent={} contributors={}",
            summary.collateral, summary.text_content, summary.contributors
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Supply",
        &summary.supply.to_string(),
        &format!(
            "prices={} measures={} languages={}",
            summary.prices, summary.measures, summary.languages
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Resources",
        &summary.resources.to_string(),
        &format!("subjects={} external payloads omitted", summary.subjects),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "ONIX for Books message".into(),
        },
        HtmlBlock::Paragraph {
            text: "ONIX publishing metadata structure is summarized without displaying book titles, identifiers, prices, descriptions or links.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "ONIX ISBN/product identifiers, titles, contributors, prices, descriptions, codelist values, URLs and commercial payloads are omitted or redacted; only bounded structure is shown".into(),
        "ONIX DTD/XSD/schemaLocation resources, images, links, codelists and publishing operations are never fetched or executed".into(),
    ];
    let mut page_sink = OnixPageSink {
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

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_ONIX_ROWS {
        return Err(Error::LimitExceeded(format!(
            "ONIX rows exceed {MAX_ONIX_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_ONIX_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_ONIX_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
