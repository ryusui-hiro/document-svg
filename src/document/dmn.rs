//! Bounded, inert previews for DMN decision requirements and decision tables.

use std::collections::HashMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_DMN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DMN_EVENTS: usize = 500_000;
const MAX_DMN_NODES: usize = 200_000;
const MAX_DMN_DEPTH: usize = 80;
const MAX_DMN_TEXT_BYTES: usize = 16 * 1024 * 1024;
const MAX_DMN_TABLES: usize = 10_000;
const MAX_DMN_RULES: usize = 200_000;
const MAX_DMN_COLUMNS: usize = 128;
const MAX_DMN_CELLS: usize = 1_000_000;
const MAX_DMN_VALUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_DMN_RENDERED_BYTES: usize = 32 * 1024 * 1024;

const DMN_MODEL_15: &str = "https://www.omg.org/spec/DMN/20230324/MODEL/";
const DMN_MODEL_14: &str = "https://www.omg.org/spec/DMN/20211108/MODEL/";
const DMN_MODEL_13: &str = "https://www.omg.org/spec/DMN/20191111/MODEL/";
const DMN_MODEL_12: &str = "https://www.omg.org/spec/DMN/20180521/MODEL/";
const DMN_MODEL_11: &str = "http://www.omg.org/spec/DMN/20151101/dmn.xsd";

struct DmnPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for DmnPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "dmn".into();
        if page.title.is_empty() {
            page.title = "DMN decision model".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_DMN_BYTES),
        "DMN input",
    )?;
    let root = parse_dmn(&bytes)?;
    let (blocks, warnings) = render_dmn(&root)?;
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "DMN conversion requires at least one page; max_pages is zero".into(),
        ));
    }
    let mut page_sink = DmnPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"definitions", None)
        && parse_dmn_prefix_root(bytes)
}

fn parse_dmn(bytes: &[u8]) -> Result<XmlElement> {
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_DMN_EVENTS,
            max_nodes: MAX_DMN_NODES,
            max_depth: MAX_DMN_DEPTH,
            max_text_bytes: MAX_DMN_TEXT_BYTES,
        },
        "DMN",
    )?;
    if root.name != "definitions" || !is_dmn_model_namespace(root.namespace.as_deref()) {
        return Err(Error::Unsupported(
            "DMN input must have a recognized DMN definitions namespace".into(),
        ));
    }
    Ok(root)
}

fn parse_dmn_prefix_root(bytes: &[u8]) -> bool {
    let mut reader = quick_xml::NsReader::from_reader(std::io::Cursor::new(bytes));
    let mut buffer = Vec::new();
    loop {
        match reader.read_resolved_event_into(&mut buffer) {
            Ok((namespace, quick_xml::events::Event::Start(element)))
            | Ok((namespace, quick_xml::events::Event::Empty(element))) => {
                return element.name().as_ref() == b"definitions"
                    && match namespace {
                        quick_xml::name::ResolveResult::Bound(namespace) => {
                            is_dmn_model_namespace(std::str::from_utf8(namespace.as_ref()).ok())
                        }
                        _ => false,
                    };
            }
            Ok((_, quick_xml::events::Event::Eof)) | Err(_) => return false,
            _ => buffer.clear(),
        }
        buffer.clear();
    }
}

fn is_dmn_model_namespace(namespace: Option<&str>) -> bool {
    matches!(
        namespace,
        Some(DMN_MODEL_11 | DMN_MODEL_12 | DMN_MODEL_13 | DMN_MODEL_14 | DMN_MODEL_15)
    )
}

fn render_dmn(root: &XmlElement) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut elements = Vec::new();
    collect_elements(root, &mut elements);
    let mut names = HashMap::<String, (String, String)>::new();
    for element in &elements {
        if element.namespace.as_deref() == root.namespace.as_deref()
            && let Some(id) = element.attribute("id")
            && names
                .insert(
                    id.to_owned(),
                    (
                        element.name.clone(),
                        element.attribute("name").unwrap_or_default().to_owned(),
                    ),
                )
                .is_some()
        {
            return Err(Error::InvalidInput(format!(
                "DMN document contains duplicate id {id:?}"
            )));
        }
    }

    let decisions = elements
        .into_iter()
        .filter(|element| {
            element.name == "decision" && element.namespace.as_deref() == root.namespace.as_deref()
        })
        .collect::<Vec<_>>();

    let mut tables_seen = 0usize;
    let mut rule_count = 0usize;
    let mut cell_count = 0usize;
    let mut rendered_bytes = 0usize;
    let mut warnings = Vec::new();
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: "DMN decision model".into(),
    }];

    for decision in decisions {
        let decision_name = decision
            .attribute("name")
            .or_else(|| decision.attribute("id"))
            .unwrap_or("Unnamed decision");
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: decision_name.to_owned(),
        });
        push_text(
            &mut blocks,
            &mut rendered_bytes,
            format!("Decision: {decision_name}"),
        )?;
        append_information_requirements(
            decision,
            &names,
            &mut blocks,
            &mut warnings,
            &mut rendered_bytes,
        )?;

        let Some(table) = decision.children.iter().find(|child| {
            child.name == "decisionTable" && child.namespace.as_deref() == root.namespace.as_deref()
        }) else {
            if let Some(expression) = decision.children.iter().find(|child| {
                child.name == "literalExpression"
                    && child.namespace.as_deref() == root.namespace.as_deref()
            }) {
                let literal = child_text(expression, "text").unwrap_or_default();
                push_text(
                    &mut blocks,
                    &mut rendered_bytes,
                    format!("Literal expression (displayed, not evaluated): {literal}"),
                )?;
            } else {
                warnings.push(format!(
                    "DMN decision {decision_name:?} has no supported decision table/literal expression"
                ));
            }
            continue;
        };
        tables_seen = tables_seen.saturating_add(1);
        if tables_seen > MAX_DMN_TABLES {
            return Err(Error::LimitExceeded(format!(
                "DMN input exceeds {MAX_DMN_TABLES} decision tables"
            )));
        }
        let hit_policy = table.attribute("hitPolicy").unwrap_or("UNIQUE");
        let aggregation = table.attribute("aggregation").unwrap_or_default();
        let mut headers = Vec::<String>::new();
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for clause in &table.children {
            if clause.namespace.as_deref() != root.namespace.as_deref() {
                continue;
            }
            match clause.name.as_str() {
                "input" => {
                    let label = clause
                        .attribute("label")
                        .filter(|label| !label.is_empty())
                        .map(str::to_owned)
                        .or_else(|| {
                            clause
                                .children
                                .iter()
                                .find(|child| {
                                    child.name == "inputExpression"
                                        && child.namespace.as_deref() == root.namespace.as_deref()
                                })
                                .and_then(|expression| child_text(expression, "text"))
                        })
                        .unwrap_or_else(|| format!("Input {}", inputs.len() + 1));
                    inputs.push(label.clone());
                    headers.push(label);
                }
                "output" => {
                    let label = clause
                        .attribute("label")
                        .filter(|label| !label.is_empty())
                        .or_else(|| clause.attribute("name").filter(|name| !name.is_empty()))
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("Output {}", outputs.len() + 1));
                    outputs.push(label.clone());
                    headers.push(label);
                }
                _ => {}
            }
        }
        if headers.is_empty() || headers.len() > MAX_DMN_COLUMNS {
            return Err(Error::LimitExceeded(format!(
                "DMN decision table has {} columns; maximum is {MAX_DMN_COLUMNS}",
                headers.len()
            )));
        }

        let rules = table
            .children
            .iter()
            .filter(|child| {
                child.name == "rule" && child.namespace.as_deref() == root.namespace.as_deref()
            })
            .collect::<Vec<_>>();
        if table.children.iter().any(|child| {
            child.name == "annotation" && child.namespace.as_deref() == root.namespace.as_deref()
        }) || rules.iter().any(|rule| {
            rule.children.iter().any(|child| {
                child.name == "annotationEntry"
                    && child.namespace.as_deref() == root.namespace.as_deref()
            })
        }) {
            warnings.push(format!(
                "DMN rule annotations for decision {decision_name:?} are omitted"
            ));
        }
        rule_count = rule_count.saturating_add(rules.len());
        if rule_count > MAX_DMN_RULES {
            return Err(Error::LimitExceeded(format!(
                "DMN input exceeds {MAX_DMN_RULES} decision rules"
            )));
        }
        cell_count = cell_count.saturating_add(headers.len().saturating_mul(rules.len()));
        if cell_count > MAX_DMN_CELLS {
            return Err(Error::LimitExceeded(format!(
                "DMN tables exceed {MAX_DMN_CELLS} rendered cells"
            )));
        }

        let mut rows = Vec::with_capacity(rules.len());
        for rule in rules {
            let mut row = Vec::with_capacity(headers.len());
            for entry_name in ["inputEntry", "outputEntry"] {
                for entry in rule.children.iter().filter(|child| {
                    child.name == entry_name
                        && child.namespace.as_deref() == root.namespace.as_deref()
                }) {
                    let value = child_text(entry, "text").unwrap_or_default();
                    if value.len() > MAX_DMN_VALUE_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "DMN entry exceeds {MAX_DMN_VALUE_BYTES} bytes"
                        )));
                    }
                    row.push(value);
                }
            }
            if row.len() != headers.len() {
                warnings.push(format!(
                    "DMN decision table {decision_name:?} has a rule with {} entries for {} columns; missing cells are blank and extra entries are omitted",
                    row.len(),
                    headers.len()
                ));
                row.truncate(headers.len());
                row.resize(headers.len(), String::new());
            }
            rows.push(row);
        }

        let policy_label = if aggregation.is_empty() {
            hit_policy.to_owned()
        } else {
            format!("{hit_policy} / {aggregation}")
        };
        push_text(
            &mut blocks,
            &mut rendered_bytes,
            format!(
                "Decision table — hit policy: {policy_label}. Expressions are shown as text; FEEL is not evaluated."
            ),
        )?;
        let table_data = TableData {
            alignments: vec![TableAlign::Left; headers.len()],
            headers,
            rows,
            raw_source: String::new(),
        };
        blocks.push(HtmlBlock::Table(table_data));
    }

    if tables_seen == 0 && blocks.len() == 1 {
        return Err(Error::Unsupported(
            "DMN document contains no supported decision or literal-expression content".into(),
        ));
    }
    warnings.sort();
    warnings.dedup();
    Ok((blocks, warnings))
}

fn append_information_requirements(
    decision: &XmlElement,
    names: &HashMap<String, (String, String)>,
    blocks: &mut Vec<HtmlBlock>,
    warnings: &mut Vec<String>,
    rendered_bytes: &mut usize,
) -> Result<()> {
    for requirement in decision.children.iter().filter(|child| {
        child.name == "informationRequirement"
            && child.namespace.as_deref() == decision.namespace.as_deref()
    }) {
        for reference in requirement.children.iter().filter(|child| {
            matches!(child.name.as_str(), "requiredInput" | "requiredDecision")
                && child.namespace.as_deref() == decision.namespace.as_deref()
        }) {
            let Some(href) = reference.attribute("href") else {
                warnings.push("DMN information requirement without href was omitted".into());
                continue;
            };
            if let Some(id) = href.strip_prefix('#') {
                if let Some((kind, name)) = names.get(id) {
                    let label = if name.is_empty() { id } else { name.as_str() };
                    push_text(blocks, rendered_bytes, format!("Requires {kind}: {label}"))?;
                } else {
                    warnings.push(format!("DMN local reference #{id} has no matching element"));
                }
            } else {
                warnings.push("DMN external references are not fetched and were omitted".into());
            }
        }
        if requirement.children.iter().any(|child| {
            child.name == "requiredKnowledge"
                && child.namespace.as_deref() == decision.namespace.as_deref()
        }) {
            warnings.push("DMN requiredKnowledge references are not rendered".into());
        }
    }
    Ok(())
}

fn child_text(parent: &XmlElement, name: &str) -> Option<String> {
    let child = parent.children.iter().find(|child| {
        child.name == name && child.namespace.as_deref() == parent.namespace.as_deref()
    })?;
    let text = child.text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn collect_elements<'a>(element: &'a XmlElement, output: &mut Vec<&'a XmlElement>) {
    output.push(element);
    for child in &element.children {
        collect_elements(child, output);
    }
}

fn push_text(blocks: &mut Vec<HtmlBlock>, rendered_bytes: &mut usize, text: String) -> Result<()> {
    *rendered_bytes = rendered_bytes.saturating_add(text.len());
    if *rendered_bytes > MAX_DMN_RENDERED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "DMN rendered text exceeds {MAX_DMN_RENDERED_BYTES} bytes"
        )));
    }
    blocks.push(HtmlBlock::Paragraph { text });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_known_dmn_model_namespace_versions() {
        for namespace in [
            DMN_MODEL_11,
            DMN_MODEL_12,
            DMN_MODEL_13,
            DMN_MODEL_14,
            DMN_MODEL_15,
        ] {
            let source = format!("<definitions xmlns=\"{namespace}\"/>");
            assert!(looks_like_prefix(source.as_bytes()));
        }
        assert!(!looks_like_prefix(b"<definitions/>"));
    }

    #[test]
    fn renders_decision_table_entries_without_evaluating_feel() {
        let source = format!(
            "<definitions xmlns=\"{DMN_MODEL_15}\"><inputData id=\"Age\" name=\"Age\"/><decision id=\"Risk\" name=\"Risk band\"><informationRequirement><requiredInput href=\"#Age\"/></informationRequirement><decisionTable hitPolicy=\"UNIQUE\"><input label=\"Age\"><inputExpression><text>Age</text></inputExpression></input><output name=\"risk\"/><rule><inputEntry><text>&lt; 18</text></inputEntry><outputEntry><text>\"minor\"</text></outputEntry></rule><rule><inputEntry><text>&gt;= 18</text></inputEntry><outputEntry><text>\"adult\"</text></outputEntry></rule></decisionTable></decision></definitions>"
        );
        let root = parse_dmn(source.as_bytes()).unwrap();
        let (blocks, warnings) = render_dmn(&root).unwrap();
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Heading { text, .. } | HtmlBlock::Paragraph { text } => {
                    Some(text.as_str())
                }
                HtmlBlock::Table(table) => Some(table.headers.first()?.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(text.contains(&"Risk band"));
        assert!(text.contains(&"Requires inputData: Age"));
        assert!(text.contains(&"Decision table — hit policy: UNIQUE. Expressions are shown as text; FEEL is not evaluated."));
        let table = blocks
            .iter()
            .find_map(|block| match block {
                HtmlBlock::Table(table) => Some(table),
                _ => None,
            })
            .unwrap();
        assert_eq!(table.headers, ["Age", "risk"]);
        assert_eq!(table.rows[0], ["< 18", "\"minor\""]);
        assert_eq!(table.rows[1], [">= 18", "\"adult\""]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn rejects_doctypes_and_non_dmn_namespaces() {
        let source = format!(
            "<!DOCTYPE definitions SYSTEM \"https://example.invalid/dmn.dtd\"><definitions xmlns=\"{DMN_MODEL_15}\"/>"
        );
        assert!(parse_dmn(source.as_bytes()).is_err());
        assert!(matches!(
            parse_dmn(b"<definitions/>"),
            Err(Error::Unsupported(_))
        ));
    }
}
