//! Small, bounded XML tree for geographic interchange formats.

use std::collections::HashMap;
use std::io::Cursor;

use quick_xml::NsReader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use quick_xml::name::ResolveResult;

use crate::error::{Error, Result};

pub(crate) struct XmlElement {
    pub name: String,
    pub namespace: Option<String>,
    pub text: String,
    pub attributes: HashMap<String, String>,
    pub children: Vec<XmlElement>,
}

impl XmlElement {
    fn new(name: String, namespace: Option<String>) -> Self {
        Self {
            name,
            namespace,
            text: String::new(),
            attributes: HashMap::new(),
            children: Vec::new(),
        }
    }

    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }

    pub fn children_named(&self, name: &str) -> impl Iterator<Item = &XmlElement> {
        self.children.iter().filter(move |child| child.name == name)
    }
}

pub(crate) struct XmlLimits {
    pub max_events: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_text_bytes: usize,
}

pub(crate) fn parse_xml_tree(
    bytes: &[u8],
    limits: &XmlLimits,
    context: &str,
) -> Result<XmlElement> {
    let mut reader = NsReader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack = Vec::<XmlElement>::new();
    let mut root = None;
    let mut event_count = 0usize;
    let mut node_count = 0usize;
    let mut text_bytes = 0usize;
    loop {
        event_count = event_count.saturating_add(1);
        if event_count > limits.max_events {
            return Err(Error::LimitExceeded(format!(
                "{context} exceeds {} XML events",
                limits.max_events
            )));
        }
        let (namespace, event) = reader.read_resolved_event_into(&mut buffer)?;
        match event {
            Event::Start(element) => {
                node_count = node_count.saturating_add(1);
                if node_count > limits.max_nodes || stack.len() >= limits.max_depth {
                    return Err(Error::LimitExceeded(format!(
                        "{context} XML node/depth limit exceeded"
                    )));
                }
                let mut node = XmlElement::new(
                    decoded_name(crate::ooxml::local_name(element.name().as_ref()), context)?,
                    decode_namespace(namespace, context)?,
                );
                for attribute in element.attributes().with_checks(true) {
                    let attribute = attribute.map_err(|error| {
                        Error::InvalidInput(format!("invalid {context} XML attribute: {error}"))
                    })?;
                    let name = decoded_name(attribute.key.as_ref(), context)?;
                    let value = attribute
                        .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                        .map_err(|error| {
                            Error::InvalidInput(format!(
                                "invalid {context} XML attribute value: {error}"
                            ))
                        })?
                        .into_owned();
                    text_bytes = text_bytes
                        .saturating_add(name.len())
                        .saturating_add(value.len());
                    check_text_budget(text_bytes, limits, context)?;
                    node.attributes.insert(name, value);
                }
                stack.push(node);
            }
            Event::Empty(element) => {
                node_count = node_count.saturating_add(1);
                if node_count > limits.max_nodes || stack.len() >= limits.max_depth {
                    return Err(Error::LimitExceeded(format!(
                        "{context} XML node/depth limit exceeded"
                    )));
                }
                let mut node = XmlElement::new(
                    decoded_name(crate::ooxml::local_name(element.name().as_ref()), context)?,
                    decode_namespace(namespace, context)?,
                );
                for attribute in element.attributes().with_checks(true) {
                    let attribute = attribute.map_err(|error| {
                        Error::InvalidInput(format!("invalid {context} XML attribute: {error}"))
                    })?;
                    let name = decoded_name(attribute.key.as_ref(), context)?;
                    let value = attribute
                        .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                        .map_err(|error| {
                            Error::InvalidInput(format!(
                                "invalid {context} XML attribute value: {error}"
                            ))
                        })?
                        .into_owned();
                    text_bytes = text_bytes
                        .saturating_add(name.len())
                        .saturating_add(value.len());
                    check_text_budget(text_bytes, limits, context)?;
                    node.attributes.insert(name, value);
                }
                append_element(&mut stack, &mut root, node, context)?;
            }
            Event::Text(event) => {
                let decoded = event.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid {context} XML text: {error}"))
                })?;
                let text = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    Error::InvalidInput(format!("invalid {context} XML entity: {error}"))
                })?;
                append_text(&mut stack, &text, &mut text_bytes, limits, context)?;
            }
            Event::CData(event) => {
                let text = event.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid {context} XML CDATA: {error}"))
                })?;
                append_text(&mut stack, &text, &mut text_bytes, limits, context)?;
            }
            Event::GeneralRef(reference) => {
                let text = crate::ooxml::decode_xml_reference(&reference, context)?;
                append_text(&mut stack, &text, &mut text_bytes, limits, context)?;
            }
            Event::End(_) => {
                let element = stack.pop().ok_or_else(|| {
                    Error::InvalidInput(format!("{context} XML has an unmatched closing tag"))
                })?;
                append_element(&mut stack, &mut root, element, context)?;
            }
            Event::DocType(_) => {
                return Err(Error::InvalidInput(format!(
                    "{context} document type declarations are not supported"
                )));
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    if !stack.is_empty() {
        return Err(Error::InvalidInput(format!(
            "{context} XML ended with unclosed elements"
        )));
    }
    root.ok_or_else(|| Error::InvalidInput(format!("{context} XML document is empty")))
}

pub(crate) fn looks_like_root(
    bytes: &[u8],
    expected_local_name: &[u8],
    expected_namespace: Option<&[u8]>,
) -> bool {
    let mut reader = NsReader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_resolved_event_into(&mut buffer) {
            Ok((namespace, Event::Start(element))) | Ok((namespace, Event::Empty(element))) => {
                let namespace_matches = match (namespace, expected_namespace) {
                    (_, None) => true,
                    (ResolveResult::Bound(actual), Some(expected)) => actual.as_ref() == expected,
                    _ => false,
                };
                return namespace_matches
                    && crate::ooxml::local_name(element.name().as_ref()) == expected_local_name;
            }
            Ok((_, Event::Eof)) | Err(_) => return false,
            _ => buffer.clear(),
        }
        buffer.clear();
    }
}

fn decode_namespace(namespace: ResolveResult<'_>, context: &str) -> Result<Option<String>> {
    match namespace {
        ResolveResult::Bound(namespace) => Ok(Some(decoded_name(namespace.as_ref(), context)?)),
        ResolveResult::Unbound => Ok(None),
        ResolveResult::Unknown(prefix) => Err(Error::InvalidInput(format!(
            "{context} XML uses an undeclared namespace prefix '{}'",
            String::from_utf8_lossy(&prefix)
        ))),
    }
}

fn decoded_name(bytes: &[u8], context: &str) -> Result<String> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|error| Error::InvalidInput(format!("invalid {context} XML name: {error}")))
}

fn check_text_budget(count: usize, limits: &XmlLimits, context: &str) -> Result<()> {
    if count > limits.max_text_bytes {
        return Err(Error::LimitExceeded(format!(
            "{context} XML text/attributes exceed {} bytes",
            limits.max_text_bytes
        )));
    }
    Ok(())
}

fn append_text(
    stack: &mut [XmlElement],
    text: &str,
    text_bytes: &mut usize,
    limits: &XmlLimits,
    context: &str,
) -> Result<()> {
    *text_bytes = text_bytes.saturating_add(text.len());
    check_text_budget(*text_bytes, limits, context)?;
    if let Some(element) = stack.last_mut() {
        element.text.push_str(text);
    }
    Ok(())
}

fn append_element(
    stack: &mut [XmlElement],
    root: &mut Option<XmlElement>,
    element: XmlElement,
    context: &str,
) -> Result<()> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(element);
    } else if root.replace(element).is_some() {
        return Err(Error::InvalidInput(format!(
            "{context} XML has multiple root elements"
        )));
    }
    Ok(())
}
