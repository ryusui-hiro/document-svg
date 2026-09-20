//! Bounded XBRL 2.1 instance-document previews.
//!
//! XBRL instances contain facts plus contexts and units needed to interpret
//! them. This adapter renders a deterministic fact table and period/unit
//! summary without loading taxonomies, linkbases or external schemas.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XBRL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XBRL_XML_EVENTS: usize = 1_000_000;
const MAX_XBRL_XML_NODES: usize = 500_000;
const MAX_XBRL_XML_DEPTH: usize = 128;
const MAX_XBRL_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_XBRL_FACTS: usize = 200_000;
const MAX_XBRL_CONTEXTS: usize = 100_000;
const MAX_XBRL_UNITS: usize = 100_000;
const MAX_XBRL_DISPLAY_BYTES: usize = 512;

const XBRL_INSTANCE_NAMESPACE: &str = "http://www.xbrl.org/2003/instance";

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"xbrl", None)
        && String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains(XBRL_INSTANCE_NAMESPACE)
}

struct XbrlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for XbrlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xbrl".into();
        if page.title.is_empty() {
            page.title = "XBRL 2.1 instance".into();
        }
        page.description =
            "XBRL facts, contexts and units are rendered as a bounded inert table; taxonomies, linkbases and external schemas are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    contexts: BTreeMap<String, String>,
    units: BTreeMap<String, String>,
    facts: usize,
    numeric_facts: usize,
    tuples: usize,
    schema_refs: usize,
    linkbase_refs: usize,
    footnotes: usize,
    dimensions: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XBRL_BYTES),
        "XBRL input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XBRL_XML_EVENTS),
            max_nodes: MAX_XBRL_XML_NODES,
            max_depth: MAX_XBRL_XML_DEPTH,
            max_text_bytes: MAX_XBRL_TEXT_BYTES,
        },
        "XBRL",
    )?;
    if !root.name.eq_ignore_ascii_case("xbrl") {
        return Err(Error::InvalidInput("XBRL instance root must xbrl".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace != XBRL_INSTANCE_NAMESPACE)
    {
        return Err(Error::InvalidInput(
            "XBRL instance namespace is missing or unsupported".into(),
        ));
    }

    let mut summary = Summary {
        schema_refs: count_named(&root, "schemaRef"),
        linkbase_refs: count_named(&root, "linkbaseRef"),
        footnotes: count_named(&root, "footnote"),
        dimensions: count_named(&root, "explicitMember") + count_named(&root, "typedMember"),
        ..Summary::default()
    };
    for context in root.children.iter().filter(|child| child.name == "context") {
        if summary.contexts.len() >= MAX_XBRL_CONTEXTS {
            return Err(Error::LimitExceeded(format!(
                "XBRL contexts exceed {MAX_XBRL_CONTEXTS}"
            )));
        }
        let id = context.attribute("id").unwrap_or("[context]").to_owned();
        let period = context_period(context);
        summary.contexts.insert(id, period);
    }
    for unit in root.children.iter().filter(|child| child.name == "unit") {
        if summary.units.len() >= MAX_XBRL_UNITS {
            return Err(Error::LimitExceeded(format!(
                "XBRL units exceed {MAX_XBRL_UNITS}"
            )));
        }
        let id = unit.attribute("id").unwrap_or("[unit]").to_owned();
        summary.units.insert(id, unit_value(unit));
    }

    for fact in &root.children {
        if is_instance_child(&fact.name) {
            continue;
        }
        let Some(context_ref) = fact.attribute("contextRef") else {
            if fact.children.is_empty() && fact.text.trim().is_empty() {
                continue;
            }
            if !fact.children.is_empty() {
                summary.tuples = summary.tuples.saturating_add(1);
            }
            continue;
        };
        if summary.facts >= MAX_XBRL_FACTS {
            return Err(Error::LimitExceeded(format!(
                "XBRL facts exceed {MAX_XBRL_FACTS}"
            )));
        }
        summary.facts = summary.facts.saturating_add(1);
        let unit_ref = fact.attribute("unitRef").unwrap_or("");
        if !unit_ref.is_empty() {
            summary.numeric_facts = summary.numeric_facts.saturating_add(1);
        }
        let value = if fact.children.is_empty() {
            safe_text(fact.text.trim())
        } else {
            "[tuple/compound fact]".into()
        };
        let period = summary
            .contexts
            .get(context_ref)
            .cloned()
            .unwrap_or_else(|| "[context omitted]".into());
        let unit = if unit_ref.is_empty() {
            "-".into()
        } else {
            summary
                .units
                .get(unit_ref)
                .cloned()
                .unwrap_or_else(|| "[unit omitted]".into())
        };
        let precision = fact
            .attribute("decimals")
            .or_else(|| fact.attribute("precision"))
            .unwrap_or("-");
        push_row(
            &mut summary.rows,
            &truncate(fact.name.as_str()),
            &value,
            &format!(
                "period={} unit={} precision={}",
                truncate(&period),
                truncate(&unit),
                truncate(precision)
            ),
        )?;
    }
    if summary.facts == 0 && summary.contexts.is_empty() {
        return Err(Error::InvalidInput(
            "XBRL instance contains no bounded facts or contexts".into(),
        ));
    }

    let metadata = format!(
        "Contexts: {}\nUnits: {}\nFacts: {} (numeric: {})\nTuples: {}\nTaxonomy references: {}\nLinkbase references: {}\nDimensions: {}\nFootnotes: {}",
        summary.contexts.len(),
        summary.units.len(),
        summary.facts,
        summary.numeric_facts,
        summary.tuples,
        summary.schema_refs,
        summary.linkbase_refs,
        summary.dimensions,
        summary.footnotes,
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "XBRL 2.1 instance".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Fact".into(), "Value".into(), "Context / unit".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "XBRL fact names/values, context periods, units and structural counts are shown; entity identifiers, dimensions, taxonomy labels, URLs and linkbase payloads are omitted or redacted".into(),
        "XBRL source values may be confidential and this preview is not de-identification; XML traversal and facts are bounded, and no taxonomy/schema/linkbase fetch, formula evaluation, validation, script or network operation runs".into(),
    ];
    let mut page_sink = XbrlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn is_instance_child(name: &str) -> bool {
    matches!(
        name,
        "context" | "unit" | "schemaRef" | "linkbaseRef" | "footnoteLink"
    )
}

fn context_period(context: &XmlElement) -> String {
    let Some(period) = context.children_named("period").next() else {
        return "[period omitted]".into();
    };
    if let Some(instant) = period.children_named("instant").next() {
        return safe_text(&text_content(instant));
    }
    let start = period
        .children_named("startDate")
        .next()
        .map(|node| safe_text(&text_content(node)))
        .unwrap_or_else(|| "?".into());
    let end = period
        .children_named("endDate")
        .next()
        .map(|node| safe_text(&text_content(node)))
        .unwrap_or_else(|| "?".into());
    format!("{start}..{end}")
}

fn unit_value(unit: &XmlElement) -> String {
    if let Some(measure) = descendants_named(unit, "measure").first().copied() {
        return safe_text(&text_content(measure));
    }
    if !descendants_named(unit, "unitNumerator").is_empty() {
        return "[divide unit]".into();
    }
    "[unit]".into()
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

fn push_row(rows: &mut Vec<Vec<String>>, fact: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_XBRL_FACTS {
        return Err(Error::LimitExceeded(format!(
            "XBRL rendered rows exceed {MAX_XBRL_FACTS}"
        )));
    }
    rows.push(vec![truncate(fact), truncate(value), truncate(detail)]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_XBRL_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XBRL_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;

    #[test]
    fn recognizes_xbrl_instance_namespace() {
        let xml = br#"<xbrli:xbrl xmlns:xbrli="http://www.xbrl.org/2003/instance"><xbrli:context id="C1"/></xbrli:xbrl>"#;
        assert!(looks_like_prefix(xml));
    }

    #[test]
    fn rejects_generic_xbrl_root() {
        assert!(!looks_like_prefix(br#"<xbrl><context/></xbrl>"#));
    }
}
