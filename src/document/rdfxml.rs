//! Bounded RDF/XML graph previews.
//!
//! RDF/XML maps XML descriptions to RDF triples. This adapter emits safe
//! predicate/value rows and structural counts without resolving IRIs, schemas,
//! vocabularies or linked resources.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const MAX_RDFXML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RDFXML_XML_EVENTS: usize = 1_000_000;
const MAX_RDFXML_XML_NODES: usize = 500_000;
const MAX_RDFXML_XML_DEPTH: usize = 96;
const MAX_RDFXML_TEXT_BYTES: usize = 48 * 1024 * 1024;
const MAX_RDFXML_ROWS: usize = 200_000;
const MAX_RDFXML_DISPLAY_BYTES: usize = 512;
const RDF_NAMESPACE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"RDF", None)
        && String::from_utf8_lossy(bytes)
            .to_ascii_lowercase()
            .contains("1999/02/22-rdf-syntax-ns")
}

struct RdfXmlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}
impl PageConsumer for RdfXmlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "rdf-xml".into();
        if page.title.is_empty() {
            page.title = "RDF/XML graph".into();
        }
        page.description = "RDF/XML predicates and safe literals are rendered as a bounded inert summary; IRIs and linked resources are not resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    subjects: usize,
    predicates: usize,
    blank_nodes: usize,
    iri_values: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_RDFXML_BYTES),
        "RDF/XML input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_RDFXML_XML_EVENTS),
            max_nodes: MAX_RDFXML_XML_NODES,
            max_depth: MAX_RDFXML_XML_DEPTH,
            max_text_bytes: MAX_RDFXML_TEXT_BYTES,
        },
        "RDF/XML",
    )?;
    if !root.name.eq_ignore_ascii_case("RDF") {
        return Err(Error::InvalidInput("RDF/XML root must RDF".into()));
    }
    if root
        .namespace
        .as_deref()
        .is_none_or(|namespace| namespace != RDF_NAMESPACE)
    {
        return Err(Error::InvalidInput(
            "RDF/XML namespace is missing or unsupported".into(),
        ));
    }
    let descriptions = root
        .children
        .iter()
        .filter(|child| {
            child.name.eq_ignore_ascii_case("Description")
                || child
                    .namespace
                    .as_deref()
                    .is_some_and(|ns| ns != RDF_NAMESPACE)
        })
        .collect::<Vec<_>>();
    let mut summary = Summary {
        subjects: descriptions.len(),
        ..Summary::default()
    };
    for description in descriptions {
        if attr_local(description, "nodeID").is_some() {
            summary.blank_nodes = summary.blank_nodes.saturating_add(1);
        }
        for predicate in &description.children {
            if predicate.name.eq_ignore_ascii_case("type") {
                continue;
            }
            if summary.predicates >= MAX_RDFXML_ROWS {
                return Err(Error::LimitExceeded(format!(
                    "RDF/XML predicates exceed {MAX_RDFXML_ROWS}"
                )));
            }
            summary.predicates = summary.predicates.saturating_add(1);
            let resource = attr_local(predicate, "resource");
            let value = if resource.is_some() {
                summary.iri_values = summary.iri_values.saturating_add(1);
                "[IRI omitted]".into()
            } else if predicate.children.is_empty() {
                safe_text(&predicate.text)
            } else {
                "[nested resource omitted]".into()
            };
            push_row(
                &mut summary.rows,
                &predicate.name,
                &value,
                "predicate object",
            )?;
        }
    }
    if summary.subjects == 0 || summary.predicates == 0 {
        return Err(Error::InvalidInput(
            "RDF/XML contains no bounded descriptions or predicates".into(),
        ));
    }
    let metadata = format!(
        "Subjects: {}\nPredicates: {}\nBlank nodes: {}\nIRI values omitted: {}",
        summary.subjects, summary.predicates, summary.blank_nodes, summary.iri_values
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "RDF/XML graph".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Predicate".into(), "Value".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 3],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "RDF/XML predicate names and safe literals are shown; subject IRIs, resource URLs, blank-node payloads, typed XML literals, vocabularies and external resources are omitted or redacted".into(),
        "RDF/XML traversal and rows are bounded; DTD/entities, URI dereferencing, schema/vocabulary fetches, inference, SPARQL, scripts and semantic reasoning never run".into(),
    ];
    let mut page_sink = RdfXmlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn attr_local<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.iter().find_map(|(key, value)| {
        key.rsplit(':')
            .next()
            .filter(|local| local.eq_ignore_ascii_case(name))
            .map(|_| value.as_str())
    })
}
fn safe_text(value: &str) -> String {
    if value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value.trim())
    }
}
fn push_row(rows: &mut Vec<Vec<String>>, predicate: &str, value: &str, detail: &str) -> Result<()> {
    if rows.len() >= MAX_RDFXML_ROWS {
        return Err(Error::LimitExceeded(format!(
            "RDF/XML rows exceed {MAX_RDFXML_ROWS}"
        )));
    }
    rows.push(vec![truncate(predicate), truncate(value), truncate(detail)]);
    Ok(())
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_RDFXML_DISPLAY_BYTES {
        value.to_owned()
    } else {
        let mut end = MAX_RDFXML_DISPLAY_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::looks_like_prefix;
    #[test]
    fn recognizes_rdf_namespace() {
        assert!(looks_like_prefix(
            br#"<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>"#
        ));
    }
    #[test]
    fn rejects_generic_rdf() {
        assert!(!looks_like_prefix(br#"<RDF><Description/></RDF>"#));
    }
}
