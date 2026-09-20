//! Bounded OASIS Universal Business Language (UBL) document previews.
//!
//! UBL invoices, orders and related procurement/transport documents are XML
//! business documents. This adapter exposes document type and structural counts
//! while keeping party payloads, identifiers, amounts, attachments and links
//! inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_UBL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_UBL_XML_EVENTS: usize = 1_000_000;
const MAX_UBL_XML_NODES: usize = 500_000;
const MAX_UBL_XML_DEPTH: usize = 128;
const MAX_UBL_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_UBL_ROWS: usize = 100_000;
const MAX_UBL_DISPLAY_BYTES: usize = 512;

const UBL_DOCUMENT_NAMES: &[&str] = &[
    "Invoice",
    "Order",
    "CreditNote",
    "DebitNote",
    "ReceiptAdvice",
    "DespatchAdvice",
    "Catalogue",
    "ApplicationResponse",
    "SelfBilledInvoice",
    "OrderResponse",
    "OrderChange",
    "OrderCancellation",
    "Quotation",
    "Tender",
    "Statement",
    "RemittanceAdvice",
    "Waybill",
    "TransportationStatus",
    "FreightInvoice",
];

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let root_name = UBL_DOCUMENT_NAMES
        .iter()
        .find(|name| crate::geospatial::xml_tree::looks_like_root(bytes, name.as_bytes(), None));
    if root_name.is_none() {
        return false;
    }
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("urn:oasis:names:specification:ubl:schema:xsd")
        && (text.contains("<cbc:id")
            || text.contains("<cbc:issuedate")
            || text.contains("<cac:")
            || text.contains("<ublversionid"))
}

struct UblPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for UblPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "ubl".into();
        if page.title.is_empty() {
            page.title = "UBL business document".into();
        }
        page.description =
            "UBL business-document structure is rendered as a bounded inert summary; party data, amounts, attachments and links are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    document_name: String,
    version: String,
    issue_date: String,
    due_date: String,
    currency: String,
    identifiers: usize,
    lines: usize,
    supplier_parties: usize,
    customer_parties: usize,
    tax_totals: usize,
    monetary_totals: usize,
    payment_means: usize,
    payment_terms: usize,
    allowance_charges: usize,
    attachments: usize,
    document_references: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_UBL_BYTES),
        "UBL input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_UBL_XML_EVENTS),
            max_nodes: MAX_UBL_XML_NODES,
            max_depth: MAX_UBL_XML_DEPTH,
            max_text_bytes: MAX_UBL_TEXT_BYTES,
        },
        "UBL",
    )?;
    if !UBL_DOCUMENT_NAMES
        .iter()
        .any(|name| root.name.eq_ignore_ascii_case(name))
    {
        return Err(Error::InvalidInput(
            "UBL XML root is not a supported business document".into(),
        ));
    }
    let namespace = root.namespace.as_deref().unwrap_or_default();
    if !namespace.contains("oasis:names:specification:ubl:schema:xsd") {
        return Err(Error::InvalidInput(
            "UBL XML root namespace is missing or unsupported".into(),
        ));
    }

    let lines = [
        "InvoiceLine",
        "OrderLine",
        "CreditNoteLine",
        "DebitNoteLine",
        "DespatchLine",
        "ReceiptLine",
        "CatalogueLine",
        "OrderResponseLine",
        "QuotationLine",
        "TenderLine",
    ]
    .iter()
    .map(|name| count_named(&root, name))
    .sum();
    let mut summary = Summary {
        document_name: root.name.clone(),
        version: first_text_descendant(&root, "UBLVersionID"),
        issue_date: first_text_descendant(&root, "IssueDate"),
        due_date: first_text_descendant(&root, "DueDate"),
        currency: first_text_descendant(&root, "DocumentCurrencyCode"),
        identifiers: count_named(&root, "ID"),
        lines,
        supplier_parties: count_named(&root, "AccountingSupplierParty"),
        customer_parties: count_named(&root, "AccountingCustomerParty")
            + count_named(&root, "BuyerCustomerParty"),
        tax_totals: count_named(&root, "TaxTotal"),
        monetary_totals: count_named(&root, "LegalMonetaryTotal"),
        payment_means: count_named(&root, "PaymentMeans"),
        payment_terms: count_named(&root, "PaymentTerms"),
        allowance_charges: count_named(&root, "AllowanceCharge"),
        attachments: count_named(&root, "Attachment"),
        document_references: count_named(&root, "AdditionalDocumentReference")
            + count_named(&root, "DocumentReference"),
        ..Summary::default()
    };
    if summary.lines == 0 && summary.identifiers == 0 {
        return Err(Error::InvalidInput(
            "UBL document contains no bounded business structure".into(),
        ));
    }

    push_row(
        &mut summary.rows,
        "Document",
        &summary.document_name,
        "UBL root",
    )?;
    push_row(
        &mut summary.rows,
        "UBL version",
        &summary.version,
        "schema version",
    )?;
    push_row(
        &mut summary.rows,
        "Issue date",
        &summary.issue_date,
        "document header",
    )?;
    push_row(
        &mut summary.rows,
        "Due date",
        &summary.due_date,
        "document header",
    )?;
    push_row(
        &mut summary.rows,
        "Currency",
        &summary.currency,
        "document header",
    )?;
    push_row(
        &mut summary.rows,
        "Identifiers",
        &summary.identifiers.to_string(),
        "values omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Line items",
        &summary.lines.to_string(),
        "line payload omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Parties",
        &format!(
            "supplier={} customer={}",
            summary.supplier_parties, summary.customer_parties
        ),
        "party names/addresses omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Totals",
        &format!(
            "tax={} monetary={}",
            summary.tax_totals, summary.monetary_totals
        ),
        "amount values omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Payment structure",
        &format!(
            "means={} terms={}",
            summary.payment_means, summary.payment_terms
        ),
        "credentials and account data omitted",
    )?;
    push_row(
        &mut summary.rows,
        "Adjustments",
        &summary.allowance_charges.to_string(),
        "allowance/charge values omitted",
    )?;
    push_row(
        &mut summary.rows,
        "References/attachments",
        &format!(
            "references={} attachments={}",
            summary.document_references, summary.attachments
        ),
        "locations and payloads omitted",
    )?;

    let metadata = format!(
        "Document: {}\nUBL version: {}\nIssue date: {}\nDue date: {}\nLine items: {}\nParties: {}\nTotals: {}",
        display_or_dash(&summary.document_name),
        display_or_dash(&summary.version),
        display_or_dash(&summary.issue_date),
        display_or_dash(&summary.due_date),
        summary.lines,
        summary.supplier_parties + summary.customer_parties,
        summary.tax_totals + summary.monetary_totals,
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: format!("UBL {}", summary.document_name),
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
        "UBL document type, schema version, dates/currency and structural counts are shown; party names/addresses, identifiers, item descriptions, amounts, tax/account values, references, attachments and URLs are omitted or redacted".into(),
        "UBL XML traversal and rendered rows are bounded; DTD/entities, XInclude, external schemas, scripts, attachment payloads, URL dereferencing and payment/procurement operations never run".into(),
    ];
    let mut page_sink = UblPageSink {
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

fn first_text_descendant(element: &XmlElement, name: &str) -> String {
    descendants_named(element, name)
        .into_iter()
        .map(text_content)
        .map(|value| safe_text(&value))
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

fn safe_text(value: &str) -> String {
    if value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value.trim())
    }
}

fn push_row(rows: &mut Vec<Vec<String>>, kind: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_UBL_ROWS {
        return Err(Error::LimitExceeded(format!(
            "UBL rendered rows exceed {MAX_UBL_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}

fn display_or_dash(value: &str) -> String {
    if value.is_empty() {
        "-".into()
    } else {
        truncate(value)
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_UBL_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_UBL_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;

    #[test]
    fn recognizes_ubl_invoice_namespace() {
        let xml = br#"<Invoice xmlns="urn:oasis:names:specification:ubl:schema:xsd:Invoice-2" xmlns:cbc="urn:oasis:names:specification:ubl:schema:xsd:CommonBasicComponents-2"><cbc:ID>1</cbc:ID></Invoice>"#;
        assert!(looks_like_prefix(xml));
    }

    #[test]
    fn rejects_generic_invoice_root() {
        assert!(!looks_like_prefix(br#"<Invoice><ID>1</ID></Invoice>"#));
    }
}
