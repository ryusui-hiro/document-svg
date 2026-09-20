//! Bounded SAML 2.0 metadata previews.
//!
//! SAML metadata describes identity-provider and service-provider roles. This
//! adapter reports descriptor and endpoint structure while keeping entity IDs,
//! certificates, URLs and authentication material inert.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_SAML_BYTES: u64 = 128 * 1024 * 1024;
const MAX_SAML_EVENTS: usize = 1_000_000;
const MAX_SAML_NODES: usize = 500_000;
const MAX_SAML_DEPTH: usize = 128;
const MAX_SAML_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_SAML_ROWS: usize = 200_000;
const MAX_SAML_DISPLAY_BYTES: usize = 512;
const SAML_NAMESPACE: &str = "urn:oasis:names:tc:SAML:2.0:metadata";

#[derive(Default)]
struct Summary {
    entities: usize,
    idp_descriptors: usize,
    sp_descriptors: usize,
    role_descriptors: usize,
    endpoints: usize,
    keys: usize,
    certificates: usize,
    attributes: usize,
    organizations: usize,
    contacts: usize,
    signatures: usize,
    rows: Vec<Vec<String>>,
}

struct SamlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for SamlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "saml".into();
        if page.title.is_empty() {
            page.title = "SAML metadata".into();
        }
        page.description = "SAML identity-provider/service-provider metadata is rendered as bounded inert structure; endpoints and certificates are not exposed".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix).to_ascii_lowercase();
    (text.contains("entitydescriptor") || text.contains("entitiesdescriptor"))
        && text.contains("saml:2.0:metadata")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_SAML_BYTES),
        "SAML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_SAML_EVENTS),
            max_nodes: MAX_SAML_NODES,
            max_depth: MAX_SAML_DEPTH,
            max_text_bytes: MAX_SAML_TEXT_BYTES,
        },
        "SAML",
    )?;
    if !root.name.eq_ignore_ascii_case("EntityDescriptor")
        && !root.name.eq_ignore_ascii_case("EntitiesDescriptor")
    {
        return Err(Error::InvalidInput(
            "SAML root must be EntityDescriptor or EntitiesDescriptor".into(),
        ));
    }
    if root.namespace.as_deref() != Some(SAML_NAMESPACE) {
        return Err(Error::InvalidInput(
            "SAML root uses an unsupported metadata namespace".into(),
        ));
    }
    let mut summary = Summary {
        entities: count_named(&root, "EntityDescriptor")
            + usize::from(root.name.eq_ignore_ascii_case("EntityDescriptor")),
        idp_descriptors: count_named(&root, "IDPSSODescriptor"),
        sp_descriptors: count_named(&root, "SPSSODescriptor"),
        role_descriptors: count_named(&root, "RoleDescriptor"),
        endpoints: count_named(&root, "SingleSignOnService")
            + count_named(&root, "SingleLogoutService")
            + count_named(&root, "AssertionConsumerService")
            + count_named(&root, "ArtifactResolutionService"),
        keys: count_named(&root, "KeyDescriptor") + count_named(&root, "KeyInfo"),
        certificates: count_named(&root, "X509Certificate"),
        attributes: count_named(&root, "Attribute") + count_named(&root, "RequestedAttribute"),
        organizations: count_named(&root, "Organization"),
        contacts: count_named(&root, "ContactPerson"),
        signatures: count_named(&root, "Signature"),
        ..Summary::default()
    };
    push_row(
        &mut summary.rows,
        "Entities",
        &format!("entities={}", summary.entities),
        &format!(
            "IdP={} SP={} roles={}",
            summary.idp_descriptors, summary.sp_descriptors, summary.role_descriptors
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Endpoints",
        &summary.endpoints.to_string(),
        &format!(
            "keys={} certificates={}",
            summary.keys, summary.certificates
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Attributes",
        &summary.attributes.to_string(),
        &format!(
            "organizations={} contacts={}",
            summary.organizations, summary.contacts
        ),
    )?;
    push_row(
        &mut summary.rows,
        "Integrity",
        &summary.signatures.to_string(),
        "signatures and entity IDs omitted",
    )?;
    let blocks = vec![HtmlBlock::Heading { level: 1, text: "SAML metadata".into() }, HtmlBlock::Paragraph { text: "SAML identity metadata structure is summarized without exposing endpoints, certificates or authentication material.".into() }, HtmlBlock::Table(TableData { headers: vec!["Kind".into(), "Value".into(), "Detail".into()], rows: summary.rows, alignments: vec![TableAlign::Left; 3], raw_source: String::new() })];
    let warnings = vec!["SAML entity IDs, endpoint URLs, certificates, bindings, organization/contact values, attributes and signatures are omitted or redacted".into(), "SAML metadata imports, schema locations, signature verification, IdP/SP discovery, authentication and network requests never run".into()];
    let mut page_sink = SamlPageSink {
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
    if rows.len() >= MAX_SAML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "SAML rows exceed {MAX_SAML_ROWS}"
        )));
    }
    rows.push(vec![truncate(kind), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_SAML_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_SAML_DISPLAY_BYTES;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}
