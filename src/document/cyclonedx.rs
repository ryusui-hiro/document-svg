//! Bounded CycloneDX JSON/XML bill-of-material previews.
//!
//! CycloneDX BOMs may contain package URLs, hashes, licenses, properties,
//! vulnerability details and other supply-chain metadata. This adapter renders
//! only component type/name/version/scope and aggregate counts; sensitive or
//! externally resolvable payloads remain inert and no registry or URL is used.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_CDX_JSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CDX_XML_BYTES: u64 = 32 * 1024 * 1024;
const MAX_CDX_DEPTH: usize = 100;
const MAX_CDX_VALUES: usize = 300_000;
const MAX_CDX_COMPONENTS: usize = 200_000;
const MAX_CDX_EVENTS: usize = 500_000;
const MAX_CDX_NODES: usize = 300_000;
const MAX_CDX_XML_DEPTH: usize = 80;
const MAX_CDX_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_CDX_STRING_BYTES: usize = 512 * 1024;

pub(crate) fn looks_like_json_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"bomFormat\"")
        && text.to_ascii_lowercase().contains("cyclonedx")
        && text.contains("\"specVersion\"")
}

pub(crate) fn looks_like_xml_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    text.contains("<bom")
        && text.contains("cyclonedx.org/schema/bom/")
        && (text.contains("<components")
            || text.contains("<metadata")
            || text.contains("<vulnerabilities"))
}

struct CycloneDxPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CycloneDxPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "cyclonedx".into();
        if page.title.is_empty() {
            page.title = "CycloneDX BOM".into();
        }
        page.description =
            "CycloneDX components and aggregate counts are rendered as inert metadata; supply-chain payloads and external resources are omitted".into();
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
        options.max_input_bytes.min(MAX_CDX_JSON_BYTES),
        "CycloneDX input",
    )?;
    let is_json = bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
        .is_some_and(|byte| byte == b'{');
    let (table, metadata, warnings) = if is_json {
        parse_json(&bytes)?
    } else {
        parse_xml(&bytes)?
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "CycloneDX BOM".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = CycloneDxPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_json(bytes: &[u8]) -> Result<(TableData, String, Vec<String>)> {
    if bytes.len() as u64 > MAX_CDX_JSON_BYTES {
        return Err(Error::LimitExceeded(format!(
            "CycloneDX JSON exceeds {MAX_CDX_JSON_BYTES} bytes"
        )));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("CycloneDX JSON must be UTF-8: {error}")))?;
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid CycloneDX JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("CycloneDX JSON root must be an object".into()))?;
    if root.get("bomFormat").and_then(Value::as_str) != Some("CycloneDX") {
        return Err(Error::InvalidInput(
            "CycloneDX JSON bomFormat must be CycloneDX".into(),
        ));
    }
    let spec_version = root
        .get("specVersion")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut rows = Vec::new();
    if let Some(components) = root.get("components").and_then(Value::as_array) {
        collect_json_components(components, &mut rows)?;
    }
    if let Some(component) = root
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("component"))
    {
        collect_json_component(component, &mut rows)?;
    }
    if rows.is_empty() {
        rows.push(vec![
            "(no components)".into(),
            "—".into(),
            "—".into(),
            "—".into(),
        ]);
    }
    let vulnerabilities = root
        .get("vulnerabilities")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let dependencies = root
        .get("dependencies")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let services = root
        .get("services")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let metadata = format!(
        "Spec version: {spec_version}\nComponents: {}\nVulnerabilities: {vulnerabilities}\nDependencies: {dependencies}\nServices: {services}",
        rows.len().saturating_sub(usize::from(
            rows.len() == 1 && rows[0][0] == "(no components)"
        ))
    );
    Ok((table(rows), metadata, warnings()))
}

fn collect_json_components(components: &[Value], rows: &mut Vec<Vec<String>>) -> Result<()> {
    for component in components {
        collect_json_component(component, rows)?;
    }
    Ok(())
}

fn collect_json_component(component: &Value, rows: &mut Vec<Vec<String>>) -> Result<()> {
    if rows.len() >= MAX_CDX_COMPONENTS {
        return Err(Error::LimitExceeded(format!(
            "CycloneDX components exceed {MAX_CDX_COMPONENTS}"
        )));
    }
    let Some(object) = component.as_object() else {
        return Ok(());
    };
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("(unnamed component)");
    let version = object.get("version").and_then(Value::as_str).unwrap_or("—");
    let scope = object.get("scope").and_then(Value::as_str).unwrap_or("—");
    rows.push(vec![
        truncate(kind),
        truncate(name),
        truncate(version),
        truncate(scope),
    ]);
    if let Some(children) = object.get("components").and_then(Value::as_array) {
        collect_json_components(children, rows)?;
    }
    Ok(())
}

fn parse_xml(bytes: &[u8]) -> Result<(TableData, String, Vec<String>)> {
    if bytes.len() as u64 > MAX_CDX_XML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "CycloneDX XML exceeds {MAX_CDX_XML_BYTES} bytes"
        )));
    }
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_CDX_EVENTS,
            max_nodes: MAX_CDX_NODES,
            max_depth: MAX_CDX_XML_DEPTH,
            max_text_bytes: MAX_CDX_TEXT_BYTES,
        },
        "CycloneDX",
    )?;
    if root.name != "bom"
        || !root
            .namespace
            .as_deref()
            .is_some_and(|namespace| namespace.starts_with("http://cyclonedx.org/schema/bom/"))
    {
        return Err(Error::InvalidInput(
            "CycloneDX XML root must use the CycloneDX BOM namespace".into(),
        ));
    }
    let mut components = Vec::new();
    collect_xml_components(&root, &mut components)?;
    if components.is_empty() {
        components.push(vec![
            "(no components)".into(),
            "—".into(),
            "—".into(),
            "—".into(),
        ]);
    }
    let vulnerabilities = count_xml_named(&root, "vulnerability");
    let dependencies = count_xml_named(&root, "dependency");
    let services = count_xml_named(&root, "service");
    let spec_version = root
        .namespace
        .as_deref()
        .and_then(|namespace| namespace.rsplit('/').next())
        .unwrap_or("unknown");
    let metadata = format!(
        "Spec version: {spec_version}\nComponents: {}\nVulnerabilities: {vulnerabilities}\nDependencies: {dependencies}\nServices: {services}",
        components.len().saturating_sub(usize::from(
            components.len() == 1 && components[0][0] == "(no components)"
        ))
    );
    Ok((table(components), metadata, warnings()))
}

fn collect_xml_components(element: &XmlElement, rows: &mut Vec<Vec<String>>) -> Result<()> {
    if element.name == "component" {
        if rows.len() >= MAX_CDX_COMPONENTS {
            return Err(Error::LimitExceeded(format!(
                "CycloneDX components exceed {MAX_CDX_COMPONENTS}"
            )));
        }
        let kind = element.attribute("type").unwrap_or("unknown");
        let name = element
            .children_named("name")
            .next()
            .map(|value| value.text.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("(unnamed component)");
        let version = element
            .children_named("version")
            .next()
            .map(|value| value.text.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("—");
        let scope = element.attribute("scope").unwrap_or("—");
        rows.push(vec![
            truncate(kind),
            truncate(name),
            truncate(version),
            truncate(scope),
        ]);
    }
    for child in &element.children {
        collect_xml_components(child, rows)?;
    }
    Ok(())
}

fn count_xml_named(element: &XmlElement, name: &str) -> usize {
    let own = usize::from(element.name == name);
    own.saturating_add(
        element
            .children
            .iter()
            .map(|child| count_xml_named(child, name))
            .sum(),
    )
}

fn table(rows: Vec<Vec<String>>) -> TableData {
    TableData {
        headers: vec![
            "Type".into(),
            "Name".into(),
            "Version".into(),
            "Scope".into(),
        ],
        rows,
        alignments: vec![TableAlign::Left; 4],
        raw_source: String::new(),
    }
}

fn warnings() -> Vec<String> {
    vec![
        "CycloneDX serial numbers, bom-ref values, hashes, licenses, package URLs, properties, vulnerability details, source URLs, services and external references are omitted; no registry, URL, provider or code operation is performed".into(),
        "Components are summarized by type/name/version/scope and nested relationships are not resolved".into(),
    ]
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_CDX_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_CDX_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_CDX_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "CycloneDX JSON nesting exceeds {MAX_CDX_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_CDX_VALUES {
        return Err(Error::LimitExceeded(format!(
            "CycloneDX JSON contains more than {MAX_CDX_VALUES} values"
        )));
    }
    match value {
        Value::Array(values) => {
            for item in values {
                count_values(item, depth + 1, count)?;
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                count_values(item, depth + 1, count)?;
            }
        }
        Value::String(value) if value.len() > MAX_CDX_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "CycloneDX JSON string exceeds {MAX_CDX_STRING_BYTES} bytes"
            )));
        }
        _ => {}
    }
    Ok(())
}

fn preflight_depth(text: &str) -> Result<()> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_CDX_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "CycloneDX JSON nesting exceeds {MAX_CDX_DEPTH} levels"
                    )));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_cyclonedx_json_and_xml() {
        assert!(looks_like_json_prefix(
            br#"{"bomFormat":"CycloneDX","specVersion":"1.6","components":[]}"#
        ));
        assert!(looks_like_xml_prefix(
            br#"<bom xmlns="http://cyclonedx.org/schema/bom/1.6"><components/></bom>"#
        ));
    }

    #[test]
    fn summarizes_components_without_sensitive_payloads() {
        let (table, metadata, warnings) = parse_json(
            br#"{"bomFormat":"CycloneDX","specVersion":"1.6","serialNumber":"urn:uuid:private","components":[{"type":"library","name":"serde","version":"1.0","purl":"pkg:cargo/serde@1.0","hashes":[{"alg":"SHA-256","content":"very-secret"}]}],"vulnerabilities":[{"id":"CVE-private","description":"private details"}]}"#,
        )
        .unwrap();
        assert!(metadata.contains("Components: 1"));
        assert_eq!(table.rows[0][1], "serde");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("private"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("omitted")));
    }
}
