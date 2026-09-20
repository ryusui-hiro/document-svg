//! Bounded OAI-PMH 2.0 response previews.
//!
//! OAI-PMH responses contain repository and harvested-record metadata. The
//! preview reports response/record structure while never performing the HTTP
//! verbs, resolving resumption tokens or exposing identifiers and payloads.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_OAIPMH_BYTES: u64 = 128 * 1024 * 1024;
const MAX_OAIPMH_EVENTS: usize = 1_000_000;
const MAX_OAIPMH_NODES: usize = 500_000;
const MAX_OAIPMH_DEPTH: usize = 128;
const MAX_OAIPMH_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_OAIPMH_ROWS: usize = 200_000;
const MAX_OAIPMH_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    verbs: usize,
    identify: usize,
    list_records: usize,
    list_identifiers: usize,
    get_record: usize,
    metadata_formats: usize,
    list_sets: usize,
    records: usize,
    headers: usize,
    metadata: usize,
    sets: usize,
    resumption_tokens: usize,
    errors: usize,
    rows: Vec<Vec<String>>,
}

struct OaiPmhPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OaiPmhPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "oaipmh".into();
        if page.title.is_empty() {
            page.title = "OAI-PMH response".into();
        }
        page.description =
            "OAI-PMH response structure is rendered as bounded inert metadata; HTTP verbs, identifiers and harvested payloads are not executed or exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(prefix, b"OAI-PMH", None)
        && String::from_utf8_lossy(prefix)
            .to_ascii_lowercase()
            .contains("openarchives.org/oai/2.0")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_OAIPMH_BYTES),
        "OAI-PMH input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_OAIPMH_EVENTS),
            max_nodes: MAX_OAIPMH_NODES,
            max_depth: MAX_OAIPMH_DEPTH,
            max_text_bytes: MAX_OAIPMH_TEXT_BYTES,
        },
        "OAI-PMH",
    )?;
    if !root.name.eq_ignore_ascii_case("OAI-PMH") {
        return Err(Error::InvalidInput("OAI-PMH root must be <OAI-PMH>".into()));
    }
    if root.namespace.as_deref().is_none_or(|namespace| {
        !namespace
            .to_ascii_lowercase()
            .contains("openarchives.org/oai/2.0")
    }) {
        return Err(Error::InvalidInput(
            "OAI-PMH root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        verbs: count_named(&root, "Identify")
            + count_named(&root, "ListRecords")
            + count_named(&root, "ListIdentifiers")
            + count_named(&root, "GetRecord")
            + count_named(&root, "ListMetadataFormats")
            + count_named(&root, "ListSets"),
        identify: count_named(&root, "Identify"),
        list_records: count_named(&root, "ListRecords"),
        list_identifiers: count_named(&root, "ListIdentifiers"),
        get_record: count_named(&root, "GetRecord"),
        metadata_formats: count_named(&root, "ListMetadataFormats")
            + count_named(&root, "metadataFormat"),
        list_sets: count_named(&root, "ListSets") + count_named(&root, "set"),
        records: count_named(&root, "record"),
        headers: count_named(&root, "header"),
        metadata: count_named(&root, "metadata"),
        sets: count_named(&root, "setSpec"),
        resumption_tokens: count_named(&root, "resumptionToken"),
        errors: count_named(&root, "error"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Verbs",
        &summary.verbs.to_string(),
        &format!(
            "identify={} listRecords={} listIdentifiers={}",
            summary.identify, summary.list_records, summary.list_identifiers
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Records",
        &summary.records.to_string(),
        &format!(
            "headers={} metadata={} sets={}",
            summary.headers, summary.metadata, summary.sets
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Requests",
        &summary.get_record.to_string(),
        &format!(
            "metadataFormats={} listSets={}",
            summary.metadata_formats, summary.list_sets
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Paging",
        &summary.resumption_tokens.to_string(),
        &format!("errors={} tokens never followed", summary.errors),
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OAI-PMH response".into(),
        },
        HtmlBlock::Paragraph {
            text: "Open Archives Initiative Protocol for Metadata Harvesting structure is summarized without issuing HTTP verbs or exposing repository records.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "OAI-PMH identifiers, datestamps, set names, repository URLs, metadata payloads, error text and resumption-token values are omitted or redacted".into(),
        "OAI-PMH HTTP verbs, resumption-token paging, schema locations, external metadata formats and network harvesting are never executed or fetched".into(),
    ];
    let mut page_sink = OaiPmhPageSink {
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
    if rows.len() >= MAX_OAIPMH_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OAI-PMH rows exceed {MAX_OAIPMH_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OAIPMH_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_OAIPMH_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
