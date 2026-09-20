//! Bounded MathML 3 presentation previews.
//!
//! This adapter converts common MathML presentation trees into deterministic
//! plain-text formula rows. It intentionally ignores annotations, embedded XML,
//! image glyphs and all active or external content.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_MATHML_BYTES: u64 = 32 * 1024 * 1024;
const MAX_MATHML_XML_EVENTS: usize = 500_000;
const MAX_MATHML_XML_NODES: usize = 300_000;
const MAX_MATHML_XML_DEPTH: usize = 96;
const MAX_MATHML_TEXT_BYTES: usize = 24 * 1024 * 1024;
const MAX_MATHML_ROWS: usize = 20_000;
const MAX_MATHML_DISPLAY_BYTES: usize = 2_048;

const MATHML_NAMESPACE: &str = "http://www.w3.org/1998/Math/MathML";

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"math", None)
        && String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains("www.w3.org/1998/math/mathml")
}

struct MathmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for MathmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "mathml".into();
        if page.title.is_empty() {
            page.title = "MathML formula".into();
        }
        page.description = "MathML presentation markup is rendered as a bounded inert formula summary; annotations and external resources are not evaluated".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    formula: String,
    tokens: usize,
    structures: BTreeMap<String, usize>,
    annotations: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_MATHML_BYTES),
        "MathML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_MATHML_XML_EVENTS),
            max_nodes: MAX_MATHML_XML_NODES,
            max_depth: MAX_MATHML_XML_DEPTH,
            max_text_bytes: MAX_MATHML_TEXT_BYTES,
        },
        "MathML",
    )?;
    if !root.name.eq_ignore_ascii_case("math") {
        return Err(Error::InvalidInput("MathML XML root must math".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace != MATHML_NAMESPACE)
    {
        return Err(Error::InvalidInput(
            "MathML namespace is missing or unsupported".into(),
        ));
    }
    let mut summary = Summary::default();
    walk_stats(&root, &mut summary);
    summary.formula = render_expr(&root);
    if summary.formula.is_empty() || summary.tokens == 0 {
        return Err(Error::InvalidInput(
            "MathML contains no presentation tokens".into(),
        ));
    }
    push_row(
        &mut summary.rows,
        "Formula",
        &summary.formula,
        "presentation tree",
    )?;
    for (kind, count) in &summary.structures {
        push_row(&mut summary.rows, kind, &count.to_string(), "element count")?;
    }
    let metadata = format!(
        "Tokens: {}\nAnnotations skipped: {}\nStructures: {}\nFormula: {}",
        summary.tokens,
        summary.annotations,
        summary.structures.values().sum::<usize>(),
        display_or_dash(&summary.formula)
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "MathML formula".into(),
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
        "MathML presentation tokens and common fraction/script/root/fence/table structures are shown; annotation, annotation-xml, mglyph/image, URL, semantic metadata and arbitrary extension payloads are omitted".into(),
        "MathML XML traversal and rows are bounded; DTD/entities, scripts, URL dereferencing, external styles/resources, content evaluation and symbolic algebra never run".into(),
    ];
    let mut page_sink = MathmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn walk_stats(element: &XmlElement, summary: &mut Summary) {
    if matches!(
        element.name.as_str(),
        "mi" | "mn" | "mo" | "mtext" | "ms" | "mspace"
    ) {
        summary.tokens = summary.tokens.saturating_add(1);
    }
    if matches!(element.name.as_str(), "annotation" | "annotation-xml") {
        summary.annotations = summary.annotations.saturating_add(1);
    }
    if is_structure(&element.name) {
        *summary.structures.entry(element.name.clone()).or_default() += 1;
    }
    for child in &element.children {
        walk_stats(child, summary);
    }
}

fn is_structure(name: &str) -> bool {
    matches!(
        name,
        "mfrac"
            | "msqrt"
            | "mroot"
            | "msub"
            | "msup"
            | "msubsup"
            | "munder"
            | "mover"
            | "munderover"
            | "mfenced"
            | "mtable"
            | "mtr"
            | "mtd"
            | "mrow"
    )
}

fn render_expr(element: &XmlElement) -> String {
    match element.name.as_str() {
        "annotation" | "annotation-xml" | "mglyph" => String::new(),
        "mi" | "mn" | "mo" | "mtext" | "ms" => safe_text(&text_content(element)),
        "mspace" => " ".into(),
        "mfrac" => binary_expr(element, "/"),
        "msup" => script_expr(element, "^"),
        "msub" => script_expr(element, "_"),
        "msubsup" => {
            let parts = child_exprs(element);
            if parts.len() >= 3 {
                format!(
                    "{}_{}{}",
                    parts[0],
                    subscript(&parts[1]),
                    superscript(&parts[2])
                )
            } else {
                join_exprs(element)
            }
        }
        "msqrt" => format!("sqrt({})", join_exprs(element)),
        "mroot" => {
            let parts = child_exprs(element);
            if parts.len() >= 2 {
                format!("root({}, {})", parts[0], parts[1])
            } else {
                join_exprs(element)
            }
        }
        "mfenced" => {
            let open = element.attribute("open").unwrap_or("(");
            let close = element.attribute("close").unwrap_or(")");
            let sep = element.attribute("separators").unwrap_or(",");
            format!("{open}{}{close}", join_exprs_with_separator(element, sep))
        }
        "mtable" => element
            .children
            .iter()
            .filter(|child| child.name == "mtr" || child.name == "mlabeledtr")
            .map(render_expr)
            .collect::<Vec<_>>()
            .join("; "),
        "mtr" => element
            .children
            .iter()
            .filter(|child| child.name == "mtd" || child.name == "mlabeledtr")
            .map(render_expr)
            .collect::<Vec<_>>()
            .join(" | "),
        "mtd" => join_exprs(element),
        "semantics" => element
            .children
            .iter()
            .find(|child| child.name != "annotation" && child.name != "annotation-xml")
            .map(render_expr)
            .unwrap_or_default(),
        "mrow" | "math" | "mstyle" | "merror" | "mpadded" | "mphantom" | "menclose" | "munder"
        | "mover" | "munderover" => join_exprs(element),
        _ => join_exprs(element),
    }
}

fn child_exprs(element: &XmlElement) -> Vec<String> {
    element
        .children
        .iter()
        .filter_map(|child| {
            let value = render_expr(child);
            (!value.is_empty()).then_some(value)
        })
        .collect()
}
fn join_exprs(element: &XmlElement) -> String {
    join_exprs_with_separator(element, " ")
}
fn join_exprs_with_separator(element: &XmlElement, separator: &str) -> String {
    child_exprs(element).join(separator)
}
fn binary_expr(element: &XmlElement, operator: &str) -> String {
    let parts = child_exprs(element);
    if parts.len() >= 2 {
        format!("({}){}({})", parts[0], operator, parts[1])
    } else {
        join_exprs(element)
    }
}
fn script_expr(element: &XmlElement, operator: &str) -> String {
    let parts = child_exprs(element);
    if parts.len() >= 2 {
        format!("{}{}({})", parts[0], operator, parts[1])
    } else {
        join_exprs(element)
    }
}
fn subscript(value: &str) -> String {
    format!("({value})")
}
fn superscript(value: &str) -> String {
    format!("^({value})")
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
    if rows.len() >= MAX_MATHML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "MathML rows exceed {MAX_MATHML_ROWS}"
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
    if value.len() <= MAX_MATHML_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_MATHML_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_mathml_namespace() {
        assert!(looks_like_prefix(
            br#"<math xmlns="http://www.w3.org/1998/Math/MathML"><mi>x</mi></math>"#
        ));
    }
    #[test]
    fn rejects_generic_math() {
        assert!(!looks_like_prefix(br#"<math><mi>x</mi></math>"#));
    }
}
