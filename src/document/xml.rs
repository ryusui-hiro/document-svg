//! Bounded, inert previews for generic XML documents.

use std::collections::{HashMap, hash_map::Entry};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};

const MAX_XML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_XML_LINES: usize = 100_000;
const MAX_XML_LINE_BYTES: usize = 1024 * 1024;
const MAX_XML_EVENTS: usize = 400_000;
const MAX_XML_NODES: usize = 200_000;
const MAX_XML_DEPTH: usize = 80;
const MAX_XML_PATH_BYTES: usize = 4 * 1024;
const MAX_XML_OUTPUT_BLOCKS: usize = 200_000;
const MAX_XML_RENDERED_TEXT_BYTES: usize = 32 * 1024 * 1024;

struct XmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for XmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "xml".into();
        if page.title.is_empty() {
            page.title = "XML document".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct RenderState {
    blocks: Vec<HtmlBlock>,
    text_bytes: usize,
    truncated_paths: usize,
}

#[derive(Default)]
struct NamespaceMap {
    prefixes: std::collections::HashMap<String, String>,
    entries: Vec<(String, String)>,
}

impl NamespaceMap {
    fn collect(root: &XmlElement) -> Self {
        let mut namespaces = Self::default();
        namespaces.visit(root);
        namespaces
    }

    fn visit(&mut self, element: &XmlElement) {
        if let Some(namespace) = &element.namespace
            && !self.prefixes.contains_key(namespace)
        {
            let prefix = format!("ns{}", self.entries.len() + 1);
            self.prefixes.insert(namespace.clone(), prefix.clone());
            self.entries.push((prefix, namespace.clone()));
        }
        for child in &element.children {
            self.visit(child);
        }
    }

    fn prefix(&self, namespace: &str) -> &str {
        self.prefixes
            .get(namespace)
            .map(String::as_str)
            .unwrap_or("ns?")
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XML_BYTES),
        "XML input",
    )?;
    check_line_limits(&bytes)?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: MAX_XML_EVENTS,
            max_nodes: MAX_XML_NODES,
            max_depth: MAX_XML_DEPTH,
            max_text_bytes: MAX_XML_RENDERED_TEXT_BYTES,
        },
        "XML",
    )?;
    let (blocks, warnings) = render_xml_blocks(&root)?;
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "XML conversion requires at least one page; max_pages is zero".into(),
        ));
    }
    let mut page_sink = XmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

pub(crate) fn looks_like_xml_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.starts_with("<?xml") || text.starts_with("<!--") || text.starts_with("<!") {
        return true;
    }
    let Some(name) = text
        .strip_prefix('<')
        .and_then(|value| value.chars().next())
    else {
        return false;
    };
    name.is_ascii_alphabetic() || name == '_' || name == ':'
}

fn check_line_limits(bytes: &[u8]) -> Result<()> {
    let mut lines = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        lines = lines.saturating_add(1);
        if lines > MAX_XML_LINES {
            return Err(Error::LimitExceeded(format!(
                "XML input exceeds {MAX_XML_LINES} lines"
            )));
        }
        if line.len() > MAX_XML_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "XML line exceeds {MAX_XML_LINE_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

fn render_xml_blocks(root: &XmlElement) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut state = RenderState::default();
    let namespaces = NamespaceMap::collect(root);
    state.blocks.push(HtmlBlock::Heading {
        level: 1,
        text: "XML document".into(),
    });
    for (prefix, namespace) in &namespaces.entries {
        let encoded_namespace = serde_json::to_string(namespace)?;
        add_line(
            &mut state,
            format!("namespace {prefix} = {encoded_namespace}"),
        )?;
    }
    visit_element(root, "", None, None, &namespaces, 0, &mut state)?;
    let warnings = if state.truncated_paths == 0 {
        Vec::new()
    } else {
        vec![format!(
            "{} XML element path(s) were truncated to {MAX_XML_PATH_BYTES} bytes for display",
            state.truncated_paths
        )]
    };
    Ok((state.blocks, warnings))
}

fn visit_element(
    element: &XmlElement,
    parent_path: &str,
    parent_namespace: Option<&str>,
    sibling_index: Option<usize>,
    namespaces: &NamespaceMap,
    depth: usize,
    state: &mut RenderState,
) -> Result<()> {
    if depth > MAX_XML_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "XML nesting exceeds {MAX_XML_DEPTH} levels"
        )));
    }
    let component = path_component(element, parent_namespace, namespaces);
    let mut path = if parent_path.is_empty() {
        format!("/{component}")
    } else {
        format!("{parent_path}/{component}")
    };
    if let Some(index) = sibling_index {
        path.push_str(&format!("[{index}]"));
    }
    if path.len() > MAX_XML_PATH_BYTES {
        let mut end = MAX_XML_PATH_BYTES;
        while !path.is_char_boundary(end) {
            end -= 1;
        }
        path.truncate(end);
        state.truncated_paths = state.truncated_paths.saturating_add(1);
    }
    add_line(state, format!("{path} (element)"))?;

    let mut attributes = element.attributes.iter().collect::<Vec<_>>();
    attributes.sort_by(|left, right| left.0.cmp(right.0));
    for (name, value) in attributes {
        if name == "xmlns" || name.starts_with("xmlns:") {
            continue;
        }
        let encoded_value = serde_json::to_string(value)?;
        add_line(state, format!("{path}/@{name} = {encoded_value}"))?;
    }
    if !element.text.trim().is_empty() {
        let encoded_text = serde_json::to_string(&element.text)?;
        add_line(state, format!("{path}/text = {encoded_text}"))?;
    }
    let mut sibling_counts = HashMap::<String, usize>::new();
    for child in &element.children {
        let child_component = path_component(child, element.namespace.as_deref(), namespaces);
        let occurrence = match sibling_counts.entry(child_component) {
            Entry::Occupied(mut entry) => {
                let next = entry.get().saturating_add(1);
                *entry.get_mut() = next;
                next
            }
            Entry::Vacant(entry) => {
                entry.insert(1);
                1
            }
        };
        visit_element(
            child,
            &path,
            element.namespace.as_deref(),
            Some(occurrence),
            namespaces,
            depth + 1,
            state,
        )?;
    }
    Ok(())
}

fn path_component(
    element: &XmlElement,
    parent_namespace: Option<&str>,
    namespaces: &NamespaceMap,
) -> String {
    match &element.namespace {
        Some(namespace) if Some(namespace.as_str()) != parent_namespace => {
            format!("{}:{}", namespaces.prefix(namespace), element.name)
        }
        _ => element.name.clone(),
    }
}

fn add_line(state: &mut RenderState, line: String) -> Result<()> {
    if state.blocks.len() >= MAX_XML_OUTPUT_BLOCKS {
        return Err(Error::LimitExceeded(format!(
            "XML preview exceeds {MAX_XML_OUTPUT_BLOCKS} rendered rows"
        )));
    }
    let new_size = state.text_bytes.saturating_add(line.len());
    if new_size > MAX_XML_RENDERED_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XML preview text exceeds {MAX_XML_RENDERED_TEXT_BYTES} bytes"
        )));
    }
    state.text_bytes = new_size;
    state.blocks.push(HtmlBlock::Paragraph { text: line });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
        let root = parse_xml_tree(
            source.as_bytes(),
            &XmlLimits {
                max_events: MAX_XML_EVENTS,
                max_nodes: MAX_XML_NODES,
                max_depth: MAX_XML_DEPTH,
                max_text_bytes: MAX_XML_RENDERED_TEXT_BYTES,
            },
            "XML",
        )?;
        render_xml_blocks(&root)
    }

    #[test]
    fn renders_nested_xml_text_attributes_and_namespaces_as_inert_rows() {
        let (blocks, warnings) = parse(
            r#"<configuration xmlns="urn:example:config" version="1">
              <service id="catalog">A&amp;B<![CDATA[ <raw> ]]></service>
              <service id="worker"><enabled>true</enabled></service>
            </configuration>"#,
        )
        .unwrap();
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Heading { text, .. } | HtmlBlock::Paragraph { text } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(text.contains(&"namespace ns1 = \"urn:example:config\""));
        assert!(text.contains(&"/ns1:configuration (element)"));
        assert!(text.contains(&"/ns1:configuration/@version = \"1\""));
        assert!(text.contains(&"/ns1:configuration/service[1]/@id = \"catalog\""));
        assert!(text.iter().any(|line| line.contains("A&B <raw>")));
        assert!(text.iter().any(|line| line.contains("worker")));
        assert!(warnings.is_empty());
    }

    #[test]
    fn rejects_doctypes_and_malformed_xml() {
        assert!(
            parse("<!DOCTYPE config SYSTEM \"https://example.invalid/config.dtd\"><config/>")
                .is_err()
        );
        assert!(parse("<config><value></config>").is_err());
    }

    #[test]
    fn enforces_xml_line_and_depth_limits_and_warns_on_long_paths() {
        let long_line = format!("<config>{}</config>", "x".repeat(MAX_XML_LINE_BYTES));
        assert!(matches!(
            check_line_limits(long_line.as_bytes()),
            Err(Error::LimitExceeded(_))
        ));

        let mut nested = "<n>".repeat(MAX_XML_DEPTH + 1);
        nested.push_str(&"</n>".repeat(MAX_XML_DEPTH + 1));
        assert!(matches!(parse(&nested), Err(Error::LimitExceeded(_))));

        let long_name = "x".repeat(MAX_XML_PATH_BYTES + 32);
        let (_, warnings) = parse(&format!("<{long_name}/>")).unwrap();
        assert!(warnings.iter().any(|warning| warning.contains("truncated")));
    }

    #[test]
    fn detects_xml_prefixes_without_claiming_plain_text() {
        assert!(looks_like_xml_prefix(
            b"\xef\xbb\xbf <?xml version=\"1.0\"?><config/>"
        ));
        assert!(looks_like_xml_prefix(b" <!-- note --> <config/>"));
        assert!(looks_like_xml_prefix(b"<config/>"));
        assert!(!looks_like_xml_prefix(b"not XML <config/>"));
    }
}
