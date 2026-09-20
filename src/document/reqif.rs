//! Bounded, inert previews for OMG Requirements Interchange Format (ReqIF).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};
use crate::table::{TableAlign, TableData};

const REQIF_NS: &str = "http://www.omg.org/spec/ReqIF/20110401/reqif.xsd";
const REQIF_ROOT: &str = "REQ-IF";

const MAX_REQIF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_REQIF_EVENTS: usize = 1_000_000;
const MAX_REQIF_NODES: usize = 500_000;
const MAX_REQIF_DEPTH: usize = 96;
const MAX_REQIF_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_REQIF_OBJECTS: usize = 200_000;
const MAX_REQIF_RELATIONS: usize = 200_000;
const MAX_REQIF_ATTRIBUTES: usize = 128;
const MAX_REQIF_VALUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_REQIF_RENDERED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
struct Requirement {
    id: String,
    name: String,
    description: String,
    type_name: String,
    attributes: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
struct Relation {
    id: String,
    source: String,
    target: String,
    type_name: String,
}

struct ReqifPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for ReqifPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "reqif".into();
        if page.title.is_empty() {
            page.title = "ReqIF requirements".into();
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
        options.max_input_bytes.min(MAX_REQIF_BYTES),
        "ReqIF input",
    )?;
    let root = parse_reqif(&bytes)?;
    let (blocks, warnings) = render_reqif(&root)?;
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "ReqIF conversion requires at least one page; max_pages is zero".into(),
        ));
    }
    let mut page_sink = ReqifPageSink {
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
                return element.name().as_ref() == REQIF_ROOT.as_bytes()
                    && matches!(
                        namespace,
                        quick_xml::name::ResolveResult::Bound(value)
                            if value.as_ref() == REQIF_NS.as_bytes()
                    );
            }
            Ok((_, quick_xml::events::Event::Eof)) | Err(_) => return false,
            _ => buffer.clear(),
        }
        buffer.clear();
    }
}

fn parse_reqif(bytes: &[u8]) -> Result<XmlElement> {
    let root = parse_xml_tree(
        bytes,
        &XmlLimits {
            max_events: MAX_REQIF_EVENTS,
            max_nodes: MAX_REQIF_NODES,
            max_depth: MAX_REQIF_DEPTH,
            max_text_bytes: MAX_REQIF_TEXT_BYTES,
        },
        "ReqIF",
    )?;
    if root.name != REQIF_ROOT || root.namespace.as_deref() != Some(REQIF_NS) {
        return Err(Error::Unsupported(
            "ReqIF input must have the OMG ReqIF 1.0.1/1.2 namespace".into(),
        ));
    }
    Ok(root)
}

fn render_reqif(root: &XmlElement) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    let mut elements = Vec::new();
    collect_elements(root, &mut elements);
    let model_ns = root.namespace.as_deref();

    let title = find_descendant(root, "TITLE")
        .map(element_value)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "ReqIF requirements".into());
    let version = find_descendant(root, "REQ-IF-VERSION")
        .map(element_value)
        .filter(|value| !value.is_empty());
    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: title.clone(),
    }];
    let mut rendered_bytes = title.len();
    if let Some(version) = version {
        push_text(
            &mut blocks,
            &mut rendered_bytes,
            format!("ReqIF exchange version: {version}"),
        )?;
    }

    let mut type_names = HashMap::<String, String>::new();
    let mut attribute_names = HashMap::<String, String>::new();
    let mut requirements = HashMap::<String, Requirement>::new();
    let mut relations = Vec::new();
    let mut warnings = Vec::new();
    for element in &elements {
        if element.namespace.as_deref() != model_ns {
            continue;
        }
        if let Some(id) = attr(element, "IDENTIFIER")
            && let Some(name) = attr_any(element, &["LONG-NAME", "NAME"])
        {
            if matches!(
                element.name.as_str(),
                "SPEC-OBJECT-TYPE" | "SPECIFICATION-TYPE" | "SPEC-RELATION-TYPE"
            ) {
                type_names.insert(id.to_owned(), name.to_owned());
            } else if element.name.starts_with("ATTRIBUTE-DEFINITION-") {
                attribute_names.insert(id.to_owned(), name.to_owned());
            }
        }
    }
    for element in &elements {
        if element.namespace.as_deref() != model_ns || element.name != "SPEC-OBJECT" {
            continue;
        }
        let Some(id) = attr(element, "IDENTIFIER") else {
            warnings.push("ReqIF SPEC-OBJECT without IDENTIFIER was omitted".into());
            continue;
        };
        if requirements.len() >= MAX_REQIF_OBJECTS {
            return Err(Error::LimitExceeded(format!(
                "ReqIF input exceeds {MAX_REQIF_OBJECTS} specification objects"
            )));
        }
        let type_id = find_descendant(element, "SPEC-OBJECT-TYPE-REF")
            .map(element_value)
            .unwrap_or_default();
        let type_name = type_names
            .get(&type_id)
            .cloned()
            .unwrap_or_else(|| type_id.clone());
        let mut attributes = Vec::new();
        if let Some(values) = find_child(element, "VALUES") {
            for value in &values.children {
                if !value.name.starts_with("ATTRIBUTE-VALUE-") {
                    continue;
                }
                let definition_id = find_descendant(value, "ATTRIBUTE-DEFINITION-STRING-REF")
                    .or_else(|| find_descendant(value, "ATTRIBUTE-DEFINITION-ENUMERATION-REF"))
                    .or_else(|| find_descendant(value, "ATTRIBUTE-DEFINITION-INTEGER-REF"))
                    .or_else(|| find_descendant(value, "ATTRIBUTE-DEFINITION-REAL-REF"))
                    .or_else(|| find_descendant(value, "ATTRIBUTE-DEFINITION-DATE-REF"))
                    .map(element_value)
                    .unwrap_or_default();
                let name = attribute_names
                    .get(&definition_id)
                    .cloned()
                    .unwrap_or_else(|| definition_id.clone());
                let value_text = attribute_value(value);
                if value_text.len() > MAX_REQIF_VALUE_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "ReqIF attribute value exceeds {MAX_REQIF_VALUE_BYTES} bytes"
                    )));
                }
                if value.name == "ATTRIBUTE-VALUE-XHTML" {
                    warnings
                        .push("ReqIF XHTML attribute values are flattened to inert text".into());
                }
                if attributes.len() < MAX_REQIF_ATTRIBUTES {
                    attributes.push((name, value_text));
                } else {
                    warnings.push(format!(
                        "ReqIF object {id:?} has more than {MAX_REQIF_ATTRIBUTES} attributes; extras were omitted"
                    ));
                    break;
                }
            }
        }
        requirements.insert(
            id.to_owned(),
            Requirement {
                id: id.to_owned(),
                name: attr_any(element, &["LONG-NAME", "NAME"])
                    .unwrap_or(id)
                    .to_owned(),
                description: attr(element, "DESC").unwrap_or_default().to_owned(),
                type_name,
                attributes,
            },
        );
    }
    for element in &elements {
        if element.namespace.as_deref() != model_ns || element.name != "SPEC-RELATION" {
            continue;
        }
        if relations.len() >= MAX_REQIF_RELATIONS {
            return Err(Error::LimitExceeded(format!(
                "ReqIF input exceeds {MAX_REQIF_RELATIONS} specification relations"
            )));
        }
        let id = attr(element, "IDENTIFIER").unwrap_or("relation").to_owned();
        let source = find_descendant(element, "SPEC-OBJECT-REF")
            .map(element_value)
            .unwrap_or_default();
        let target = element
            .children
            .iter()
            .filter(|child| child.name == "TARGET")
            .find_map(|target| find_descendant(target, "SPEC-OBJECT-REF"))
            .map(element_value)
            .unwrap_or_default();
        let type_id = find_descendant(element, "SPEC-RELATION-TYPE-REF")
            .map(element_value)
            .unwrap_or_default();
        let type_name = type_names.get(&type_id).cloned().unwrap_or(type_id);
        relations.push(Relation {
            id,
            source,
            target,
            type_name,
        });
    }

    let mut ordered_ids = Vec::new();
    let mut seen = HashSet::new();
    if let Some(specifications) = find_descendant(root, "SPECIFICATIONS") {
        for specification in specifications
            .children
            .iter()
            .filter(|child| child.name == "SPECIFICATION")
        {
            let name = attr_any(specification, &["LONG-NAME", "NAME"])
                .unwrap_or("Specification")
                .to_owned();
            blocks.push(HtmlBlock::Heading {
                level: 2,
                text: name,
            });
            if let Some(children) = find_child(specification, "CHILDREN") {
                collect_hierarchy(
                    children,
                    0,
                    &requirements,
                    &mut ordered_ids,
                    &mut seen,
                    &mut warnings,
                );
            }
        }
    }
    for id in requirements.keys() {
        if seen.insert(id.clone()) {
            ordered_ids.push((0, id.clone()));
        }
    }
    let mut rows = Vec::with_capacity(ordered_ids.len());
    let mut details = Vec::with_capacity(ordered_ids.len());
    for (level, id) in ordered_ids {
        let Some(requirement) = requirements.get(&id) else {
            warnings.push(format!(
                "ReqIF hierarchy references missing SPEC-OBJECT {id:?}"
            ));
            continue;
        };
        let attribute_summary = requirement
            .attributes
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("; ");
        charge_rendered(
            &mut rendered_bytes,
            requirement.id.len()
                + requirement.name.len()
                + requirement.type_name.len()
                + requirement.description.len()
                + attribute_summary.len(),
        )?;
        rows.push(vec![
            level.to_string(),
            requirement.id.clone(),
            requirement.name.clone(),
            requirement.type_name.clone(),
        ]);
        details.push((
            requirement.id.clone(),
            requirement.name.clone(),
            requirement.description.clone(),
            attribute_summary,
        ));
    }
    if !rows.is_empty() {
        blocks.push(HtmlBlock::Table(TableData {
            headers: vec![
                "Lv".into(),
                "Identifier".into(),
                "Requirement".into(),
                "Type".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }));
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: "Requirement details".into(),
        });
        for (id, name, description, attributes) in details {
            if !description.is_empty() {
                push_text(
                    &mut blocks,
                    &mut rendered_bytes,
                    format!("{id} — {name}: {description}"),
                )?;
            } else if !name.is_empty() {
                push_text(&mut blocks, &mut rendered_bytes, format!("{id} — {name}"))?;
            }
            if !attributes.is_empty() {
                push_text(
                    &mut blocks,
                    &mut rendered_bytes,
                    format!("{id} attributes: {attributes}"),
                )?;
            }
        }
    }
    if !relations.is_empty() {
        blocks.push(HtmlBlock::Heading {
            level: 2,
            text: "Relations".into(),
        });
        blocks.push(HtmlBlock::Table(TableData {
            headers: vec![
                "Identifier".into(),
                "Type".into(),
                "Source".into(),
                "Target".into(),
            ],
            rows: relations
                .iter()
                .map(|relation| {
                    vec![
                        relation.id.clone(),
                        relation.type_name.clone(),
                        relation.source.clone(),
                        relation.target.clone(),
                    ]
                })
                .collect(),
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }));
    }
    if requirements.is_empty() && relations.is_empty() {
        return Err(Error::Unsupported(
            "ReqIF document contains no SPEC-OBJECT or SPEC-RELATION content".into(),
        ));
    }
    warnings.sort();
    warnings.dedup();
    Ok((blocks, warnings))
}

fn collect_hierarchy(
    element: &XmlElement,
    level: usize,
    requirements: &HashMap<String, Requirement>,
    ordered_ids: &mut Vec<(usize, String)>,
    seen: &mut HashSet<String>,
    warnings: &mut Vec<String>,
) {
    for hierarchy in element
        .children
        .iter()
        .filter(|child| child.name == "SPEC-HIERARCHY")
    {
        let id = find_child(hierarchy, "OBJECT")
            .and_then(|object| find_descendant(object, "SPEC-OBJECT-REF"))
            .map(element_value)
            .unwrap_or_default();
        if id.is_empty() {
            warnings.push("ReqIF SPEC-HIERARCHY without SPEC-OBJECT-REF was omitted".into());
        } else if !requirements.contains_key(&id) {
            warnings.push(format!(
                "ReqIF hierarchy references missing SPEC-OBJECT {id:?}"
            ));
        } else if seen.insert(id.clone()) {
            ordered_ids.push((level, id));
        }
        if let Some(children) = find_child(hierarchy, "CHILDREN") {
            collect_hierarchy(
                children,
                level + 1,
                requirements,
                ordered_ids,
                seen,
                warnings,
            );
        }
    }
}

fn collect_elements<'a>(element: &'a XmlElement, output: &mut Vec<&'a XmlElement>) {
    output.push(element);
    for child in &element.children {
        collect_elements(child, output);
    }
}

fn find_descendant<'a>(element: &'a XmlElement, name: &str) -> Option<&'a XmlElement> {
    if element.name == name {
        return Some(element);
    }
    element
        .children
        .iter()
        .find_map(|child| find_descendant(child, name))
}

fn find_child<'a>(element: &'a XmlElement, name: &str) -> Option<&'a XmlElement> {
    element.children.iter().find(|child| child.name == name)
}

fn attr<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attributes.get(name).map(String::as_str)
}

fn attr_any<'a>(element: &'a XmlElement, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| attr(element, name))
}

fn element_value(element: &XmlElement) -> String {
    if let Some(value) = attr_any(element, &["THE-VALUE", "VALUE", "LONG-NAME", "DESC"]) {
        return value.to_owned();
    }
    let mut value = element.text.trim().to_owned();
    for child in &element.children {
        let child_value = element_value(child);
        if !child_value.is_empty() {
            if !value.is_empty() {
                value.push(' ');
            }
            value.push_str(&child_value);
        }
    }
    value
}

fn attribute_value(element: &XmlElement) -> String {
    if let Some(value) = attr(element, "THE-VALUE") {
        return value.to_owned();
    }
    if let Some(value) = find_descendant(element, "THE-VALUE") {
        return element_value(value);
    }
    if element.name == "ATTRIBUTE-VALUE-ENUMERATION" {
        let values = element
            .children
            .iter()
            .filter(|child| child.name == "VALUES")
            .flat_map(|values| values.children.iter())
            .filter(|child| child.name == "ENUM-VALUE-REF")
            .map(element_value)
            .collect::<Vec<_>>();
        return values.join(", ");
    }
    element_value(element)
}

fn push_text(blocks: &mut Vec<HtmlBlock>, rendered_bytes: &mut usize, text: String) -> Result<()> {
    charge_rendered(rendered_bytes, text.len())?;
    blocks.push(HtmlBlock::Paragraph { text });
    Ok(())
}

fn charge_rendered(rendered_bytes: &mut usize, additional: usize) -> Result<()> {
    *rendered_bytes = rendered_bytes.saturating_add(additional);
    if *rendered_bytes > MAX_REQIF_RENDERED_BYTES {
        return Err(Error::LimitExceeded(format!(
            "ReqIF rendered text exceeds {MAX_REQIF_RENDERED_BYTES} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<REQ-IF xmlns="http://www.omg.org/spec/ReqIF/20110401/reqif.xsd">
 <THE-HEADER><REQ-IF-HEADER><TITLE>Vehicle requirements</TITLE><REQ-IF-VERSION>1.0</REQ-IF-VERSION></REQ-IF-HEADER></THE-HEADER>
 <CORE-CONTENT><REQ-IF-CONTENT>
  <SPEC-TYPES><SPEC-OBJECT-TYPE IDENTIFIER="REQ-TYPE" LONG-NAME="System Requirement"><SPEC-ATTRIBUTES><ATTRIBUTE-DEFINITION-STRING IDENTIFIER="REQ-TEXT" LONG-NAME="Text"/></SPEC-ATTRIBUTES></SPEC-OBJECT-TYPE><SPEC-RELATION-TYPE IDENTIFIER="DERIVE" LONG-NAME="Derives"/></SPEC-TYPES>
  <SPEC-OBJECTS><SPEC-OBJECT IDENTIFIER="R1" LONG-NAME="Brake response" DESC="The vehicle shall stop within the target distance."><TYPE><SPEC-OBJECT-TYPE-REF>REQ-TYPE</SPEC-OBJECT-TYPE-REF></TYPE><VALUES><ATTRIBUTE-VALUE-STRING THE-VALUE="Stop within 40 m"><DEFINITION><ATTRIBUTE-DEFINITION-STRING-REF>REQ-TEXT</ATTRIBUTE-DEFINITION-STRING-REF></DEFINITION></ATTRIBUTE-VALUE-STRING></VALUES></SPEC-OBJECT><SPEC-OBJECT IDENTIFIER="R2" LONG-NAME="Brake warning"><TYPE><SPEC-OBJECT-TYPE-REF>REQ-TYPE</SPEC-OBJECT-TYPE-REF></TYPE><VALUES><ATTRIBUTE-VALUE-STRING THE-VALUE="Warn before pad wear limit"><DEFINITION><ATTRIBUTE-DEFINITION-STRING-REF>REQ-TEXT</ATTRIBUTE-DEFINITION-STRING-REF></DEFINITION></ATTRIBUTE-VALUE-STRING></VALUES></SPEC-OBJECT></SPEC-OBJECTS>
  <SPEC-RELATIONS><SPEC-RELATION IDENTIFIER="REL1"><SOURCE><SPEC-OBJECT-REF>R1</SPEC-OBJECT-REF></SOURCE><TARGET><SPEC-OBJECT-REF>R2</SPEC-OBJECT-REF></TARGET><TYPE><SPEC-RELATION-TYPE-REF>DERIVE</SPEC-RELATION-TYPE-REF></TYPE></SPEC-RELATION></SPEC-RELATIONS>
  <SPECIFICATIONS><SPECIFICATION IDENTIFIER="SPEC1" LONG-NAME="Braking"><CHILDREN><SPEC-HIERARCHY IDENTIFIER="H1"><OBJECT><SPEC-OBJECT-REF>R1</SPEC-OBJECT-REF></OBJECT><CHILDREN><SPEC-HIERARCHY IDENTIFIER="H2"><OBJECT><SPEC-OBJECT-REF>R2</SPEC-OBJECT-REF></OBJECT></SPEC-HIERARCHY></CHILDREN></SPEC-HIERARCHY></CHILDREN></SPECIFICATION></SPECIFICATIONS>
 </REQ-IF-CONTENT></CORE-CONTENT>
</REQ-IF>"#;

    #[test]
    fn renders_requirements_hierarchy_values_and_relations() {
        let root = parse_reqif(SAMPLE.as_bytes()).unwrap();
        let (blocks, warnings) = render_reqif(&root).unwrap();
        let tables = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Table(table) => Some(table),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].rows.len(), 2);
        assert_eq!(tables[0].rows[0][2], "Brake response");
        assert_eq!(tables[0].rows[1][0], "1");
        assert!(blocks.iter().any(|block| matches!(
            block,
            HtmlBlock::Paragraph { text } if text.contains("The vehicle shall stop within the target distance")
        )));
        assert_eq!(tables[1].rows[0][1], "Derives");
        assert_eq!(tables[1].rows[0][2], "R1");
        assert!(warnings.is_empty());
    }

    #[test]
    fn rejects_wrong_namespace_and_doctype() {
        assert!(!looks_like_prefix(b"<REQ-IF/>"));
        assert!(parse_reqif(b"<REQ-IF xmlns=\"urn:wrong\"/>").is_err());
        let source = format!(
            "<!DOCTYPE REQ-IF SYSTEM \"https://example.invalid/reqif.dtd\"><REQ-IF xmlns=\"{REQIF_NS}\"/>"
        );
        assert!(parse_reqif(source.as_bytes()).is_err());
    }
}
