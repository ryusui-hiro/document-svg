//! Bounded ISO 19444/Adobe XFDF XML form-data previews.
//!
//! XFDF is the XML representation of PDF forms data and annotations. This
//! adapter renders field names, value kinds and safe values while keeping PDF
//! references, actions, rich payloads and external URLs inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_XFDF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XFDF_XML_EVENTS: usize = 1_000_000;
const MAX_XFDF_XML_NODES: usize = 500_000;
const MAX_XFDF_XML_DEPTH: usize = 96;
const MAX_XFDF_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_XFDF_FIELDS: usize = 100_000;
const MAX_XFDF_ROWS: usize = 100_000;
const MAX_XFDF_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"xfdf", None)
        && String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains("adobe.com/xfdf")
}

struct XfdfPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for XfdfPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xfdf".into();
        if page.title.is_empty() {
            page.title = "XFDF form data".into();
        }
        page.description =
            "XFDF field and annotation metadata is rendered as a bounded inert summary; PDF targets, actions and external resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    fields: usize,
    sensitive_fields: usize,
    values: usize,
    rich_text_fields: usize,
    annotations: usize,
    target_files: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XFDF_BYTES),
        "XFDF input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_XFDF_XML_EVENTS),
            max_nodes: MAX_XFDF_XML_NODES,
            max_depth: MAX_XFDF_XML_DEPTH,
            max_text_bytes: MAX_XFDF_TEXT_BYTES,
        },
        "XFDF",
    )?;
    if !root.name.eq_ignore_ascii_case("xfdf") {
        return Err(Error::InvalidInput("XFDF XML root must xfdf".into()));
    }
    if !root
        .namespace
        .as_deref()
        .is_some_and(|namespace| namespace.contains("adobe.com/xfdf"))
    {
        return Err(Error::InvalidInput(
            "XFDF namespace is missing or unsupported".into(),
        ));
    }
    let mut summary = Summary {
        annotations: count_named(&root, "annot")
            + count_named(&root, "freetext")
            + count_named(&root, "highlight")
            + count_named(&root, "underline")
            + count_named(&root, "strikeout"),
        target_files: count_named(&root, "f") + count_named(&root, "ids"),
        ..Summary::default()
    };
    if let Some(fields) = descendants_named(&root, "fields").first().copied() {
        for field in fields
            .children
            .iter()
            .filter(|child| child.name.eq_ignore_ascii_case("field"))
        {
            walk_field(field, "", 1, &mut summary)?;
        }
    }
    if summary.fields == 0 && summary.annotations == 0 {
        return Err(Error::InvalidInput(
            "XFDF document contains no fields or annotations".into(),
        ));
    }
    let metadata = format!(
        "Fields: {}\nSensitive fields: {}\nValues: {}\nRich-text fields: {}\nAnnotations: {}\nPDF target/ID elements: {}",
        summary.fields,
        summary.sensitive_fields,
        summary.values,
        summary.rich_text_fields,
        summary.annotations,
        summary.target_files
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "XFDF form data".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec![
                "Field".into(),
                "Kind".into(),
                "Value".into(),
                "Detail".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "XFDF field names, value kinds, safe values and annotation counts are shown; PDF file targets, IDs, actions, rich-text payloads, appearance data and external URLs are omitted or redacted".into(),
        "XFDF XML traversal and rendered rows are bounded; DTD/entities, submit/reset actions, JavaScript, URI dereferencing, annotation imports and PDF rendering never run".into(),
    ];
    let mut page_sink = XfdfPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn walk_field(field: &XmlElement, parent: &str, depth: usize, summary: &mut Summary) -> Result<()> {
    if depth > MAX_XFDF_XML_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "XFDF field depth exceeds {MAX_XFDF_XML_DEPTH}"
        )));
    }
    if summary.fields >= MAX_XFDF_FIELDS {
        return Err(Error::LimitExceeded(format!(
            "XFDF fields exceed {MAX_XFDF_FIELDS}"
        )));
    }
    summary.fields = summary.fields.saturating_add(1);
    let name = field.attribute("name").unwrap_or("[unnamed]");
    let path = if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}.{name}")
    };
    for value in field
        .children
        .iter()
        .filter(|child| child.name.eq_ignore_ascii_case("value"))
    {
        summary.values = summary.values.saturating_add(1);
        let value_text = if is_sensitive_field(&path) {
            summary.sensitive_fields = summary.sensitive_fields.saturating_add(1);
            "[sensitive value omitted]".into()
        } else {
            safe_text(&text_content(value))
        };
        push_row(
            &mut summary.rows,
            &path,
            "value",
            &value_text,
            &format!("depth={depth}"),
        )?;
    }
    for value in field
        .children
        .iter()
        .filter(|child| child.name.eq_ignore_ascii_case("value-richtext"))
    {
        summary.values = summary.values.saturating_add(1);
        summary.rich_text_fields = summary.rich_text_fields.saturating_add(1);
        push_row(
            &mut summary.rows,
            &path,
            "richtext",
            "[rich text omitted]",
            &format!("depth={depth}"),
        )?;
        let _ = value;
    }
    if !field.children.iter().any(|child| {
        child.name.eq_ignore_ascii_case("value")
            || child.name.eq_ignore_ascii_case("value-richtext")
    }) {
        push_row(
            &mut summary.rows,
            &path,
            "field",
            "-",
            &format!("depth={depth}"),
        )?;
    }
    for child in field
        .children
        .iter()
        .filter(|child| child.name.eq_ignore_ascii_case("field"))
    {
        walk_field(child, &path, depth.saturating_add(1), summary)?;
    }
    Ok(())
}

fn is_sensitive_field(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        "password",
        "passcode",
        "secret",
        "token",
        "ssn",
        "socialsecurity",
        "bankaccount",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
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

fn push_row(
    rows: &mut Vec<Vec<String>>,
    field: &str,
    kind: &str,
    value: &str,
    detail: &str,
) -> Result<()> {
    if rows.len() >= MAX_XFDF_ROWS {
        return Err(Error::LimitExceeded(format!(
            "XFDF rendered rows exceed {MAX_XFDF_ROWS}"
        )));
    }
    rows.push(vec![
        truncate(field),
        truncate(kind),
        truncate(value),
        truncate(detail),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_XFDF_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_XFDF_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_xfdf_namespace() {
        assert!(looks_like_prefix(
            br#"<xfdf xmlns="http://ns.adobe.com/xfdf/"><fields/></xfdf>"#
        ));
    }
    #[test]
    fn rejects_generic_fields() {
        assert!(!looks_like_prefix(br#"<xfdf><fields/></xfdf>"#));
    }
}
