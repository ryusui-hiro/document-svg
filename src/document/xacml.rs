//! Bounded OASIS XACML 3.0 policy previews.
//!
//! XACML policies are executable authorization rules. This adapter reports
//! policy structure only; matching, condition evaluation, obligations,
//! references and external policy retrieval are never performed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XACML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XACML_EVENTS: usize = 1_000_000;
const MAX_XACML_NODES: usize = 500_000;
const MAX_XACML_DEPTH: usize = 128;
const MAX_XACML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_XACML_ROWS: usize = 200_000;
const MAX_XACML_DISPLAY_BYTES: usize = 512;

#[derive(Default)]
struct Summary {
    policy_sets: usize,
    policies: usize,
    rules: usize,
    targets: usize,
    conditions: usize,
    matches: usize,
    attributes: usize,
    values: usize,
    obligations: usize,
    advice: usize,
    references: usize,
    functions: usize,
    rows: Vec<Vec<String>>,
}

struct XacmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for XacmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xacml".into();
        if page.title.is_empty() {
            page.title = "XACML policy".into();
        }
        page.description = "XACML policy structure is rendered as bounded inert metadata; authorization decisions are not evaluated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    (text.contains("<policy") || text.contains("<policyset"))
        && text.contains("xacml:3.0:core:schema")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XACML_BYTES),
        "XACML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XACML_EVENTS),
            max_nodes: MAX_XACML_NODES,
            max_depth: MAX_XACML_DEPTH,
            max_text_bytes: MAX_XACML_TEXT_BYTES,
        },
        "XACML",
    )?;
    if !root.name.eq_ignore_ascii_case("Policy") && !root.name.eq_ignore_ascii_case("PolicySet") {
        return Err(Error::InvalidInput(
            "XACML root must be Policy or PolicySet".into(),
        ));
    }
    if root.namespace.as_deref().is_none_or(|namespace| {
        !namespace
            .to_ascii_lowercase()
            .contains("xacml:3.0:core:schema")
    }) {
        return Err(Error::InvalidInput(
            "XACML root uses an unsupported namespace".into(),
        ));
    }
    let mut summary = Summary {
        policy_sets: count_named(&root, "PolicySet")
            + usize::from(root.name.eq_ignore_ascii_case("PolicySet")),
        policies: count_named(&root, "Policy")
            + usize::from(root.name.eq_ignore_ascii_case("Policy")),
        rules: count_named(&root, "Rule"),
        targets: count_named(&root, "Target"),
        conditions: count_named(&root, "Condition"),
        matches: count_named(&root, "Match"),
        attributes: count_named(&root, "AttributeDesignator")
            + count_named(&root, "AttributeSelector"),
        values: count_named(&root, "AttributeValue"),
        obligations: count_named(&root, "ObligationExpressions") + count_named(&root, "Obligation"),
        advice: count_named(&root, "AdviceExpressions") + count_named(&root, "Advice"),
        references: count_named(&root, "PolicyIdReference")
            + count_named(&root, "PolicySetIdReference"),
        functions: count_named(&root, "Function"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Policies",
        &format!("policies={}", summary.policies),
        &format!("policySets={} rules={}", summary.policy_sets, summary.rules),
    )?;
    push_row(
        &mut summary.rows,
        "Targets",
        &format!("targets={}", summary.targets),
        &format!(
            "conditions={} matches={}",
            summary.conditions, summary.matches
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Attributes",
        &summary.attributes.to_string(),
        &format!("values={} functions={}", summary.values, summary.functions),
    )?;
    push_row(
        &mut summary.rows,
        "Effects",
        &summary.obligations.to_string(),
        &format!(
            "advice={} references={}",
            summary.advice, summary.references
        ),
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "XACML policy".into() }, HtmlBlock::Paragraph { text: "XACML access-control policy structure is summarized without evaluating authorization requests or obligations.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["XACML Policy/Rule IDs, target conditions, attribute values, function URIs, obligations, advice and policy references are omitted or redacted".into(), "XACML policy combining, XPath/functions, external references, PDP/PEP calls and authorization evaluation never run".into()];
    let mut page_sink = XacmlPageSink {
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
    if rows.len() >= MAX_XACML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XACML rows exceed {MAX_XACML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_XACML_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XACML_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
