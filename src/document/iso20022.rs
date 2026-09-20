//! Bounded ISO 20022 financial-message previews.
//!
//! ISO 20022 messages carry payment, securities and reporting data. This
//! adapter recognizes the ISO 20022 namespace and renders message-family and
//! structural counts without exposing account numbers, amounts or parties.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_ISO20022_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ISO20022_EVENTS: usize = 1_000_000;
const MAX_ISO20022_NODES: usize = 500_000;
const MAX_ISO20022_DEPTH: usize = 128;
const MAX_ISO20022_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ISO20022_ROWS: usize = 200_000;
const MAX_ISO20022_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    root: String,
    documents: usize,
    headers: usize,
    messages: usize,
    payments: usize,
    transactions: usize,
    parties: usize,
    accounts: usize,
    amounts: usize,
    dates: usize,
    remittance: usize,
    rows: Vec<Vec<String>>,
}

struct Iso20022PageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for Iso20022PageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "iso20022".into();
        if page.title.is_empty() {
            page.title = "ISO 20022 message".into();
        }
        page.description =
            "ISO 20022 message structure is rendered as bounded inert metadata; financial values and parties are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("iso:std:iso:20022:tech:xsd:")
        && (text.contains("<document") || text.contains("<apphdr") || text.contains("<busmsg"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_ISO20022_BYTES),
        "ISO 20022 input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_ISO20022_EVENTS),
            max_nodes: MAX_ISO20022_NODES,
            max_depth: MAX_ISO20022_DEPTH,
            max_text_bytes: MAX_ISO20022_TEXT_BYTES,
        },
        "ISO 20022",
    )?;
    if !root.namespace.as_deref().is_some_and(|namespace| {
        namespace
            .to_ascii_lowercase()
            .contains("iso:std:iso:20022:tech:xsd:")
    }) {
        return Err(Error::InvalidInput(
            "ISO 20022 root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        root: root.name.clone(),
        documents: count_named(&root, "Document"),
        headers: count_named(&root, "AppHdr") + count_named(&root, "AppHdrV01"),
        messages: count_named(&root, "FIToFICstmrCdtTrf")
            + count_named(&root, "CstmrCdtTrfInitn")
            + count_named(&root, "BkToCstmrStmt")
            + count_named(&root, "PmtStsRpt"),
        payments: count_named(&root, "PmtInf") + count_named(&root, "PaymentInformation"),
        transactions: count_named(&root, "CdtTrfTxInf")
            + count_named(&root, "TxInfAndSts")
            + count_named(&root, "TxInf"),
        parties: count_named(&root, "Dbtr")
            + count_named(&root, "Cdtr")
            + count_named(&root, "OrgnlPty"),
        accounts: count_named(&root, "DbtrAcct")
            + count_named(&root, "CdtrAcct")
            + count_named(&root, "Acct"),
        amounts: count_named(&root, "Amt")
            + count_named(&root, "InstdAmt")
            + count_named(&root, "TtlIntrBkSttlmAmt"),
        dates: count_named(&root, "ReqdExctnDt")
            + count_named(&root, "IntrBkSttlmDt")
            + count_named(&root, "CreDtTm"),
        remittance: count_named(&root, "RmtInf")
            + count_named(&root, "Ustrd")
            + count_named(&root, "Strd"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Message",
        &summary.root,
        &format!(
            "documents={} headers={} messages={}",
            summary.documents, summary.headers, summary.messages
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Payments",
        &summary.payments.to_string(),
        &format!(
            "transactions={} accounts={}",
            summary.transactions, summary.accounts
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Parties",
        &summary.parties.to_string(),
        &format!("amountNodes={} dates={}", summary.amounts, summary.dates),
    )?;
    push_row(
        &mut summary.rows,
        "Remittance",
        &summary.remittance.to_string(),
        "account, amount and reference values omitted",
    )?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "ISO 20022 message".into(),
        },
        HtmlBlock::Paragraph {
            text: "ISO 20022 financial-message structure is summarized without displaying account, party, amount or reference values.".into(),
        },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "ISO 20022 account numbers, names, addresses, amounts, currencies, dates, identifiers, remittance text, URLs and private financial values are omitted or redacted".into(),
        "ISO 20022 schema locations, code lists, external references, payment operations and network delivery never run".into(),
    ];
    let mut page_sink = Iso20022PageSink {
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
    if rows.len() >= MAX_ISO20022_ROWS {
        return Err(Error::LimitExceeded(format!(
            "ISO 20022 rows exceed {MAX_ISO20022_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_ISO20022_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_ISO20022_DISPLAY_BYTES;
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
