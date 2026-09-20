//! Bounded, inert previews for OMG XMI model-interchange documents.

use std::collections::HashSet;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const XMI_NAMESPACES: &[&str] = &[
    "http://schema.omg.org/spec/XMI/2.1",
    "http://www.omg.org/spec/XMI/20110701",
    "http://www.omg.org/spec/XMI/20131001",
    "http://www.omg.org/spec/XMI/20161101",
    "http://www.omg.org/XMI",
];

const MAX_XMI_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XMI_EVENTS: usize = 1_000_000;
const MAX_XMI_NODES: usize = 500_000;
const MAX_XMI_DEPTH: usize = 96;
const MAX_XMI_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_XMI_ELEMENTS: usize = 200_000;
const MAX_XMI_ATTRIBUTES: usize = 128;
const MAX_XMI_VALUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_XMI_RENDERED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
struct ModelRow {
    depth: usize,
    id: String,
    kind: String,
    name: String,
    properties: String,
}

struct XmiPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for XmiPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xmi".into();
        if page.title.is_empty() {
            page.title = "XMI model".into();
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
        options.max_input_bytes.min(MAX_XMI_BYTES),
        "XMI input",
    )?;
    let root = parse_xmi(&bytes)?;
    let (blocks, warnings) = render_xmi(&root)?;
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "XMI conversion requires at least one page; max_pages is zero".into(),
        ));
    }
    let mut page_sink = XmiPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let mut reader = quick_xml::NsReader::from_reader(std::io::Cursor::new(bytes));
    let mut buffer = Vec::new();
    loop {
        match reader.read_resolved_event_into(&mut buffer) {
            Ok((namespace, quick_xml::events::Event::Start(element)))
            | Ok((namespace, quick_xml::events::Event::Empty(element))) => {
                return crate::ooxml::local_name(element.name().as_ref()) == b"XMI"
                    && matches!(
                        namespace,
                        quick_xml::name::ResolveResult::Bound(value)
                            if XMI_NAMESPACES.iter().any(|expected| value.as_ref() == expected.as_bytes())
                    );
            }
            Ok((_, quick_xml::events::Event::Eof)) | Err(_) => return false,
            _ => buffer.clear(),
        }
        buffer.clear();
    }
}

fn parse_xmi(bytes: &[u8]) -> Result<XmlElement> {
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_XMI_EVENTS,
            max_nodes: MAX_XMI_NODES,
            max_depth: MAX_XMI_DEPTH,
            max_text_bytes: MAX_XMI_TEXT_BYTES,
        },
        "XMI",
    )?;
    if root.name != "XMI"
        || !root
            .namespace
            .as_deref()
            .is_some_and(|namespace| XMI_NAMESPACES.contains(&namespace))
    {
        return Err(Error::Unsupported(
            "XMI input must have an OMG XMI root and recognized namespace".into(),
        ));
    }
    Ok(root)
}

fn render_xmi(root: &XmlElement) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut rows = Vec::new();
    let mut warnings = Vec::new();
    let mut rendered_bytes = 0usize;
    collect_rows(root, 0, &mut rows, &mut warnings, &mut rendered_bytes)?;
    if rows.is_empty() {
        return Err(Error::Unsupported(
            "XMI document contains no model elements with xmi:id, xmi:type, or name".into(),
        ));
    }
    let title = attr_any(root, &["xmi:version", "version"])
        .map(|version| format!("XMI model (version {version})"))
        .unwrap_or_else(|| "XMI model".into());
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: title,
    }];
    blocks.push(HtmlBlock::Table(TableData {
        headers: vec![
            "Depth".into(),
            "Identifier".into(),
            "Type".into(),
            "Name".into(),
        ],
        rows: rows
            .iter()
            .map(|row| {
                vec![
                    row.depth.to_string(),
                    row.id.clone(),
                    row.kind.clone(),
                    row.name.clone(),
                ]
            })
            .collect(),
        alignments: vec![TableAlign::Left; 4],
        raw_source: String::new(),
    }));
    blocks.push(HtmlBlock::Heading {
        level: 2,
        text: "Model element details".into(),
    });
    for row in rows {
        let mut detail = format!(
            "{} — {}",
            if row.id.is_empty() {
                "(no id)"
            } else {
                &row.id
            },
            row.name
        );
        if !row.kind.is_empty() {
            detail.push_str(&format!(" [{kind}]", kind = row.kind));
        }
        if !row.properties.is_empty() {
            detail.push_str(&format!(": {}", row.properties));
        }
        push_text(&mut blocks, &mut rendered_bytes, detail)?;
    }
    Ok((blocks, warnings))
}

fn collect_rows(
    element: &XmlElement,
    depth: usize,
    rows: &mut Vec<ModelRow>,
    warnings: &mut Vec<String>,
    rendered_bytes: &mut usize,
) -> Result<()> {
    if rows.len() >= MAX_XMI_ELEMENTS {
        return Err(Error::LimitExceeded(format!(
            "XMI model exceeds {MAX_XMI_ELEMENTS} rendered elements"
        )));
    }
    let id = attr_any(element, &["xmi:id", "id"])
        .unwrap_or_default()
        .to_owned();
    let kind = attr_any(element, &["xmi:type", "type"])
        .unwrap_or_default()
        .to_owned();
    let name = attr_any(element, &["name", "label"])
        .unwrap_or_default()
        .to_owned();
    let mut properties = Vec::new();
    let mut seen = HashSet::new();
    let mut attributes = element.attributes.iter().collect::<Vec<_>>();
    if attributes.len() > MAX_XMI_ATTRIBUTES {
        return Err(Error::LimitExceeded(format!(
            "XMI element exceeds {MAX_XMI_ATTRIBUTES} attributes"
        )));
    }
    attributes.sort_by(|left, right| left.0.cmp(right.0));
    for (key, value) in attributes {
        let is_model_id = key == "id" && element.attributes.contains_key("xmi:id");
        let is_model_type = key == "type" && !element.attributes.contains_key("xmi:type");
        if is_model_id
            || is_model_type
            || matches!(
                key.as_str(),
                "id" | "name" | "label" | "xmlns" | "xmi:id" | "xmi:type" | "xmi:version"
            )
            || key.starts_with("xmlns:")
        {
            continue;
        }
        if !seen.insert(key.clone()) {
            continue;
        }
        if value.len() > MAX_XMI_VALUE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XMI attribute {key:?} exceeds {MAX_XMI_VALUE_BYTES} bytes"
            )));
        }
        properties.push(format!("{key}={value}"));
    }
    let meaningful = !id.is_empty() || !kind.is_empty() || !name.is_empty();
    if meaningful {
        let property_text = properties.join("; ");
        *rendered_bytes = rendered_bytes
            .saturating_add(id.len())
            .saturating_add(kind.len())
            .saturating_add(name.len())
            .saturating_add(property_text.len());
        if *rendered_bytes > MAX_XMI_RENDERED_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XMI rendered text exceeds {MAX_XMI_RENDERED_BYTES} bytes"
            )));
        }
        rows.push(ModelRow {
            depth,
            id,
            kind,
            name,
            properties: property_text,
        });
    }
    if element.attribute("href").is_some() {
        warnings
            .push("XMI href references are displayed as attributes and never dereferenced".into());
    }
    for child in &element.children {
        collect_rows(child, depth + 1, rows, warnings, rendered_bytes)?;
    }
    Ok(())
}

fn push_text(blocks: &mut Vec<HtmlBlock>, rendered_bytes: &mut usize, text: String) -> Result<()> {
    *rendered_bytes = rendered_bytes.saturating_add(text.len());
    if *rendered_bytes > MAX_XMI_RENDERED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XMI rendered text exceeds {MAX_XMI_RENDERED_BYTES} bytes"
        )));
    }
    blocks.push(HtmlBlock::Paragraph { text });
    Ok(())
}

fn attr_any<'a>(element: &'a XmlElement, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| element.attributes.get(*name).map(String::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xmi:XMI xmi:version="2.1" xmlns:xmi="http://schema.omg.org/spec/XMI/2.1" xmlns:uml="http://www.eclipse.org/uml2/5.0.0/UML">
  <uml:Model xmi:id="model1" name="Vehicle">
    <packagedElement xmi:type="uml:Class" xmi:id="class1" name="BrakeController" visibility="public">
      <ownedAttribute xmi:type="uml:Property" xmi:id="attr1" name="pressure" type="double" />
    </packagedElement>
  </uml:Model>
</xmi:XMI>"#;

    #[test]
    fn renders_xmi_model_elements_and_inert_properties() {
        let root = parse_xmi(SAMPLE.as_bytes()).unwrap();
        let (blocks, warnings) = render_xmi(&root).unwrap();
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Heading { text, .. } | HtmlBlock::Paragraph { text } => {
                    Some(text.as_str())
                }
                HtmlBlock::Table(table) => table
                    .rows
                    .first()
                    .and_then(|row| row.get(2).map(String::as_str)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(text.iter().any(|line| line.contains("BrakeController")));
        assert!(
            text.iter()
                .any(|line| line.contains("pressure [uml:Property]: type=double"))
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn rejects_wrong_namespace_and_doctype() {
        assert!(!looks_like_prefix(b"<XMI/>"));
        assert!(parse_xmi(b"<XMI xmlns:xmi=\"urn:wrong\"/>").is_err());
        let source = format!(
            "<!DOCTYPE XMI SYSTEM \"https://example.invalid/xmi.dtd\"><XMI xmlns=\"{}\"/>",
            XMI_NAMESPACES[0]
        );
        assert!(parse_xmi(source.as_bytes()).is_err());
    }
}
