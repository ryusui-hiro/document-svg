//! Bounded WSDL 1.1/2.0 service-description previews.
//!
//! WSDL describes services, ports, bindings, interfaces/portTypes, operations
//! and messages. This adapter renders names and structural counts only; import
//! locations, schema URLs, endpoint addresses and SOAP/HTTP calls remain inert.

use std::collections::BTreeMap;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_WSDL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WSDL_XML_NODES: usize = 500_000;
const MAX_WSDL_XML_EVENTS: usize = 1_000_000;
const MAX_WSDL_XML_DEPTH: usize = 96;
const MAX_WSDL_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_WSDL_ROWS: usize = 200_000;
const MAX_WSDL_DISPLAY_BYTES: usize = 256;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(
        bytes,
        b"definitions",
        Some(b"http://schemas.xmlsoap.org/wsdl/"),
    ) || crate::geospatial::xml_tree::looks_like_root(
        bytes,
        b"description",
        Some(b"http://www.w3.org/ns/wsdl"),
    )
}

struct WsdlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for WsdlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "wsdl".into();
        if page.title.is_empty() {
            page.title = "WSDL service description".into();
        }
        page.description =
            "WSDL service and operation metadata is rendered inertly; imports, schemas, endpoint addresses and SOAP/HTTP calls are not resolved or executed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Counts {
    definitions: usize,
    services: usize,
    ports: usize,
    bindings: usize,
    interfaces: usize,
    port_types: usize,
    operations: usize,
    messages: usize,
    imports: usize,
    includes: usize,
    schemas: usize,
    rows: Vec<Vec<String>>,
    by_kind: BTreeMap<String, usize>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_WSDL_BYTES),
        "WSDL input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_WSDL_XML_EVENTS),
            max_nodes: MAX_WSDL_XML_NODES,
            max_depth: MAX_WSDL_XML_DEPTH,
            max_text_bytes: MAX_WSDL_TEXT_BYTES,
        },
        "WSDL",
    )?;
    if root.name != "definitions" && root.name != "description" {
        return Err(Error::InvalidInput(
            "WSDL XML root must be definitions (1.1) or description (2.0)".into(),
        ));
    }
    let mut summary = Counts::default();
    visit(&root, &mut summary)?;
    summary.definitions = 1;
    if summary.rows.is_empty() {
        summary.rows.push(vec![
            "WSDL".into(),
            "—".into(),
            "1".into(),
            "definitions/description".into(),
        ]);
    }
    let mut warnings = Vec::new();
    if summary.imports > 0 || summary.includes > 0 {
        warnings.push(format!(
            "{} WSDL import(s) and {} include(s) were counted but not opened",
            summary.imports, summary.includes
        ));
    }
    if summary.schemas > 0 {
        warnings.push(format!(
            "{} embedded XML Schema element(s) were counted; schema types and imports remain inert",
            summary.schemas
        ));
    }
    warnings.push("WSDL endpoint addresses, bindings, schema locations, documentation, policies, credentials and message payloads are omitted; no SOAP/HTTP request or code generation runs".into());
    warnings.push("WSDL XML traversal is bounded by input, node/event/depth/text and rendered-row limits; QName references are shown only as inert attributes".into());
    let metadata = format!(
        "Services: {}\nPorts: {}\nBindings: {}\nInterfaces: {}\nPort types: {}\nOperations: {}\nMessages: {}\nImports/includes: {}/{}\nEmbedded schemas: {}",
        summary.services,
        summary.ports,
        summary.bindings,
        summary.interfaces,
        summary.port_types,
        summary.operations,
        summary.messages,
        summary.imports,
        summary.includes,
        summary.schemas
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "WSDL service description".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "Name".into(), "N".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let mut page_sink = WsdlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn visit(element: &XmlElement, summary: &mut Counts) -> Result<()> {
    let (kind, detail) = match element.name.as_str() {
        "service" => {
            summary.services = summary.services.saturating_add(1);
            (Some("service"), "service")
        }
        "port" => {
            summary.ports = summary.ports.saturating_add(1);
            (Some("port"), "endpoint port")
        }
        "binding" => {
            summary.bindings = summary.bindings.saturating_add(1);
            (Some("binding"), "binding")
        }
        "interface" => {
            summary.interfaces = summary.interfaces.saturating_add(1);
            (Some("interface"), "interface")
        }
        "portType" => {
            summary.port_types = summary.port_types.saturating_add(1);
            (Some("portType"), "abstract port type")
        }
        "operation" => {
            summary.operations = summary.operations.saturating_add(1);
            (Some("operation"), "operation")
        }
        "message" => {
            summary.messages = summary.messages.saturating_add(1);
            (Some("message"), "message")
        }
        "import" => {
            summary.imports = summary.imports.saturating_add(1);
            (Some("import"), "external import")
        }
        "include" => {
            summary.includes = summary.includes.saturating_add(1);
            (Some("include"), "external include")
        }
        "schema" => {
            summary.schemas = summary.schemas.saturating_add(1);
            (Some("schema"), "embedded schema")
        }
        _ => (None, ""),
    };
    if let Some(kind) = kind {
        let name = element
            .attribute("name")
            .or_else(|| element.attribute("id"))
            .unwrap_or("—");
        if summary.rows.len() >= MAX_WSDL_ROWS {
            return Err(Error::LimitExceeded(format!(
                "WSDL rows exceed {MAX_WSDL_ROWS}"
            )));
        }
        summary
            .rows
            .push(vec![kind.into(), truncate(name), "1".into(), detail.into()]);
        *summary.by_kind.entry(kind.into()).or_default() += 1;
    }
    for child in &element.children {
        visit(child, summary)?;
    }
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_WSDL_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_WSDL_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_wsdl_roots() {
        assert!(looks_like_prefix(
            br#"<definitions xmlns="http://schemas.xmlsoap.org/wsdl/"/>"#
        ));
        assert!(looks_like_prefix(
            br#"<description xmlns="http://www.w3.org/ns/wsdl"/>"#
        ));
        assert!(!looks_like_prefix(
            br#"<description><p>generic XML</p></description>"#
        ));
        assert!(!looks_like_prefix(
            br#"<definitions><service/></definitions>"#
        ));
        assert!(!looks_like_prefix(br#"<OpenSCENARIO/>"#));
    }

    #[test]
    fn counts_services_operations_and_imports() {
        let xml = br#"<definitions><import namespace="urn:x" location="private.wsdl"/><service name="Catalog"><port name="Get" binding="tns:b"/></service><portType name="CatalogPort"><operation name="list"><input message="tns:List"/></operation></portType><binding name="CatalogBinding" type="tns:CatalogPort"/><message name="List"/></definitions>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "WSDL",
        )
        .unwrap();
        let mut summary = Counts::default();
        visit(&root, &mut summary).unwrap();
        assert_eq!(summary.services, 1);
        assert_eq!(summary.ports, 1);
        assert_eq!(summary.operations, 1);
        assert_eq!(summary.imports, 1);
    }
}
