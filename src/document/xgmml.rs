//! Bounded XGMML graph preview.
//!
//! XGMML is an XML evolution of GML used by Cytoscape. The preview keeps the
//! graph/node/edge labels and topology, while attribute columns, graphics
//! coordinates, nested subgraphs, and external resources remain inert.

use std::collections::HashSet;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::diagram::{DiagramEdge, DiagramGraph, DiagramNode, NodeShape, layout_and_render_graph};
use crate::error::{Error, Result};

const MAX_XGMML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_XGMML_EVENTS: usize = 1_000_000;
const MAX_XGMML_DEPTH: usize = 128;
const MAX_XGMML_NODES: usize = 100_000;
const MAX_XGMML_EDGES: usize = 200_000;
const MAX_XGMML_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_XGMML_LABEL_BYTES: usize = 1024 * 1024;
const MAX_XGMML_ID_BYTES: usize = 4096;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<graph")
        && text.contains("directed=")
        && text.contains("<node")
        && (text.contains("xmlns:xgmml") || text.contains("xgmml"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_XGMML_BYTES),
        "XGMML input",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("XGMML input must be UTF-8: {error}")))?;
    let (graph, warnings) = parse_xgmml(text)?;
    let mut page = layout_and_render_graph(&graph, options)?;
    page.source_format = "xgmml".into();
    page.title = graph
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .map(|title| format!("XGMML — {title}"))
        .unwrap_or_else(|| "XGMML graph".into());
    page.description = format!(
        "XGMML graph with {} nodes and {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );
    for warning in &warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(warnings)
}

struct NodeBuilder {
    id: String,
    label: String,
}

struct EdgeBuilder {
    from: String,
    to: String,
    label: Option<String>,
}

#[derive(Default)]
struct ParserState {
    graph: DiagramGraph,
    warnings: Vec<String>,
    depth: usize,
    events: usize,
    text_bytes: usize,
    node: Option<NodeBuilder>,
    edge: Option<EdgeBuilder>,
    capture: Option<CaptureTarget>,
    seen_nodes: HashSet<String>,
    seen_edges: usize,
    nested_nodes: usize,
    nested_node_depth: usize,
    graph_seen: bool,
    attributes: bool,
    graphics: bool,
}

enum CaptureTarget {
    NodeLabel,
    EdgeLabel,
}

fn parse_xgmml(text: &str) -> Result<(DiagramGraph, Vec<String>)> {
    if text.len() as u64 > MAX_XGMML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XGMML input exceeds {MAX_XGMML_BYTES} bytes"
        )));
    }
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut state = ParserState::default();
    let mut root_seen = false;
    loop {
        state.events = state.events.saturating_add(1);
        if state.events > MAX_XGMML_EVENTS {
            return Err(Error::LimitExceeded(format!(
                "XGMML exceeds {MAX_XGMML_EVENTS} XML events"
            )));
        }
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let local = local_name(start.name().as_ref());
                if !root_seen {
                    root_seen = true;
                    if local != "graph" {
                        return Err(Error::InvalidInput(
                            "XGMML root element must be graph".into(),
                        ));
                    }
                }
                state.depth = state.depth.saturating_add(1);
                if state.depth > MAX_XGMML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "XGMML XML nesting exceeds {MAX_XGMML_DEPTH} levels"
                    )));
                }
                handle_start(&mut state, &start, &local)?;
            }
            Ok(Event::Empty(empty)) => {
                state.events = state.events.saturating_add(1);
                let local = local_name(empty.name().as_ref());
                if !root_seen {
                    root_seen = true;
                    if local != "graph" {
                        return Err(Error::InvalidInput(
                            "XGMML root element must be graph".into(),
                        ));
                    }
                }
                handle_start(&mut state, &empty, &local)?;
                handle_end(&mut state, &local)?;
            }
            Ok(Event::End(end)) => {
                let local = local_name(end.name().as_ref());
                handle_end(&mut state, &local)?;
                state.depth = state.depth.saturating_sub(1);
            }
            Ok(Event::Text(event)) => {
                let decoded = event
                    .decode()
                    .map_err(|error| Error::InvalidInput(format!("invalid XGMML text: {error}")))?;
                let value = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    Error::InvalidInput(format!("invalid XGMML entity: {error}"))
                })?;
                append_capture(&mut state, value.as_ref())?;
            }
            Ok(Event::CData(event)) => {
                let value = event.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid XGMML CDATA: {error}"))
                })?;
                append_capture(&mut state, value.as_ref())?;
            }
            Ok(Event::DocType(_)) => {
                return Err(Error::Unsupported(
                    "XGMML DTD and external entity declarations are unsupported".into(),
                ));
            }
            Ok(Event::Eof) => break,
            Ok(Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::GeneralRef(_)) => {}
            Err(error) => {
                return Err(Error::InvalidInput(format!("invalid XGMML XML: {error}")));
            }
        }
    }
    if !root_seen {
        return Err(Error::InvalidInput("XGMML input is empty".into()));
    }
    if state.node.is_some() || state.edge.is_some() {
        return Err(Error::InvalidInput(
            "XGMML input ended inside a node or edge".into(),
        ));
    }
    if state.graph.nodes.is_empty() {
        return Err(Error::InvalidInput("XGMML contains no nodes".into()));
    }
    if state.nested_nodes > 0 {
        state.warnings.push(format!(
            "XGMML omitted {} nested node element(s); compound-node layout is not reconstructed",
            state.nested_nodes
        ));
    }
    if state.attributes {
        state
            .warnings
            .push("XGMML node/edge att attributes were omitted from diagram labels".into());
    }
    if state.graphics {
        state
            .warnings
            .push("XGMML graphics and layout coordinates were omitted; an automatic layered layout was used".into());
    }
    let before = state.graph.edges.len();
    state.graph.edges.retain(|edge| {
        state.seen_nodes.contains(&edge.from) && state.seen_nodes.contains(&edge.to)
    });
    let omitted_edges = before.saturating_sub(state.graph.edges.len());
    if omitted_edges > 0 {
        state.warnings.push(format!(
            "XGMML omitted {omitted_edges} edge(s) referencing unknown nodes"
        ));
    }
    Ok((state.graph, dedup_warnings(state.warnings)))
}

fn handle_start(state: &mut ParserState, start: &BytesStart<'_>, local: &str) -> Result<()> {
    match local {
        "graph" => {
            if state.graph_seen {
                return Err(Error::Unsupported(
                    "XGMML multiple graph elements are unsupported".into(),
                ));
            }
            state.graph_seen = true;
            let directed = attr(start, "directed")
                .map(|value| matches!(value.trim(), "1" | "true" | "TRUE"))
                .unwrap_or(true);
            state.graph.is_directed = directed;
            state.graph.title = attr(start, "label").or_else(|| attr(start, "id"));
        }
        "node" => {
            if state.node.is_some() {
                state.nested_nodes = state.nested_nodes.saturating_add(1);
                state.nested_node_depth = state.nested_node_depth.saturating_add(1);
                return Ok(());
            }
            if state.graph.nodes.len() >= MAX_XGMML_NODES {
                return Err(Error::LimitExceeded(format!(
                    "XGMML exceeds {MAX_XGMML_NODES} nodes"
                )));
            }
            let id = attr(start, "id")
                .ok_or_else(|| Error::InvalidInput("XGMML node is missing its id".into()))?;
            validate_id(&id, "node")?;
            if !state.seen_nodes.insert(id.clone()) {
                return Err(Error::InvalidInput(format!(
                    "XGMML node id '{id}' is duplicated"
                )));
            }
            let label = attr(start, "label").unwrap_or_default();
            if label.len() > MAX_XGMML_LABEL_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "XGMML label exceeds {MAX_XGMML_LABEL_BYTES} bytes"
                )));
            }
            state.node = Some(NodeBuilder { id, label });
        }
        "edge" => {
            if state.edge.is_some() {
                return Ok(());
            }
            if state.seen_edges >= MAX_XGMML_EDGES {
                return Err(Error::LimitExceeded(format!(
                    "XGMML exceeds {MAX_XGMML_EDGES} edges"
                )));
            }
            let from = attr(start, "source")
                .ok_or_else(|| Error::InvalidInput("XGMML edge is missing its source".into()))?;
            let to = attr(start, "target")
                .ok_or_else(|| Error::InvalidInput("XGMML edge is missing its target".into()))?;
            validate_id(&from, "edge source")?;
            validate_id(&to, "edge target")?;
            let label = attr(start, "label");
            if label
                .as_ref()
                .is_some_and(|value| value.len() > MAX_XGMML_LABEL_BYTES)
            {
                return Err(Error::LimitExceeded(format!(
                    "XGMML label exceeds {MAX_XGMML_LABEL_BYTES} bytes"
                )));
            }
            state.edge = Some(EdgeBuilder { from, to, label });
            state.seen_edges = state.seen_edges.saturating_add(1);
        }
        "label" => {
            if state.node.is_some() && state.nested_node_depth == 0 {
                if let Some(node) = state.node.as_mut() {
                    node.label.clear();
                }
                state.capture = Some(CaptureTarget::NodeLabel);
            } else if state.edge.is_some() {
                if let Some(edge) = state.edge.as_mut() {
                    edge.label = Some(String::new());
                }
                state.capture = Some(CaptureTarget::EdgeLabel);
            }
        }
        "att" => state.attributes = true,
        "graphics" | "position" | "size" | "Line" | "Shape" => state.graphics = true,
        _ => {}
    }
    Ok(())
}

fn handle_end(state: &mut ParserState, local: &str) -> Result<()> {
    match local {
        "label" => state.capture = None,
        "node" => {
            if state.nested_node_depth > 0 {
                state.nested_node_depth = state.nested_node_depth.saturating_sub(1);
                return Ok(());
            }
            if let Some(node) = state.node.take() {
                state.graph.nodes.push(DiagramNode {
                    id: node.id,
                    label: if node.label.trim().is_empty() {
                        "node".into()
                    } else {
                        node.label.trim().to_string()
                    },
                    shape: NodeShape::Box,
                    x: 0.0,
                    y: 0.0,
                    width: 130.0,
                    height: 46.0,
                });
            }
        }
        "edge" => {
            if let Some(edge) = state.edge.take() {
                state.graph.edges.push(DiagramEdge {
                    from: edge.from,
                    to: edge.to,
                    label: edge.label.filter(|label| !label.trim().is_empty()),
                });
            }
        }
        _ => {}
    }
    Ok(())
}

fn append_capture(state: &mut ParserState, text: &str) -> Result<()> {
    if state.capture.is_none() || text.is_empty() {
        return Ok(());
    }
    state.text_bytes = state.text_bytes.saturating_add(text.len());
    if state.text_bytes > MAX_XGMML_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XGMML text exceeds {MAX_XGMML_TEXT_BYTES} bytes"
        )));
    }
    match state.capture {
        Some(CaptureTarget::NodeLabel) => {
            if let Some(node) = state.node.as_mut() {
                append_label(&mut node.label, text)?;
            }
        }
        Some(CaptureTarget::EdgeLabel) => {
            if let Some(edge) = state.edge.as_mut() {
                let label = edge.label.get_or_insert_with(String::new);
                append_label(label, text)?;
            }
        }
        None => {}
    }
    Ok(())
}

fn append_label(label: &mut String, text: &str) -> Result<()> {
    if label.len().saturating_add(text.len()) > MAX_XGMML_LABEL_BYTES {
        return Err(Error::LimitExceeded(format!(
            "XGMML label exceeds {MAX_XGMML_LABEL_BYTES} bytes"
        )));
    }
    label.push_str(text);
    Ok(())
}

fn attr(start: &BytesStart<'_>, name: &str) -> Option<String> {
    start
        .attributes()
        .flatten()
        .find(|attribute| local_name(attribute.key.as_ref()).eq_ignore_ascii_case(name))
        .and_then(|attribute| {
            attribute
                .decoded_and_normalized_value(quick_xml::XmlVersion::Implicit1_0, start.decoder())
                .ok()
        })
        .map(|value| value.into_owned())
}

fn validate_id(id: &str, context: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_XGMML_ID_BYTES {
        return Err(Error::InvalidInput(format!(
            "XGMML {context} id is empty or exceeds {MAX_XGMML_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn local_name(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|name| name.rsplit(':').next())
        .unwrap_or_default()
        .to_string()
}

fn dedup_warnings(mut warnings: Vec<String>) -> Vec<String> {
    warnings.sort();
    warnings.dedup();
    warnings
}

#[cfg(test)]
mod tests {
    use super::{looks_like_prefix, parse_xgmml};

    #[test]
    fn parses_cytoscape_style_nodes_edges_and_attributes() {
        let source = r#"<?xml version="1.0"?>
<graph xmlns:xgmml="http://www.cs.rpi.edu/XGMML" directed="1" id="g" label="Network">
  <node id="1" label="A"><att name="kind" type="string" value="start"/></node>
  <node id="2" label="B"/>
  <edge id="e" source="1" target="2" label="connect"/>
</graph>"#;
        let (graph, warnings) = parse_xgmml(source).unwrap();
        assert!(warnings.iter().any(|warning| warning.contains("att")));
        assert_eq!(graph.title.as_deref(), Some("Network"));
        assert_eq!(graph.nodes[0].label, "A");
        assert_eq!(graph.edges[0].label.as_deref(), Some("connect"));
    }

    #[test]
    fn recognizes_xgmml_markers_without_confusing_graphml() {
        assert!(looks_like_prefix(
            b"<graph xmlns:xgmml='urn:xgmml' directed='1'><node id='a'/></graph>"
        ));
        assert!(!looks_like_prefix(
            b"<graphml><graph edgedefault='directed'><node id='a'/></graph></graphml>"
        ));
    }

    #[test]
    fn rejects_dtds_before_external_processing() {
        let source = "<!DOCTYPE graph SYSTEM 'https://example.invalid/xgmml.dtd'><graph xmlns:xgmml='urn:xgmml' directed='1'/>";
        let error = parse_xgmml(source).unwrap_err();
        assert!(error.to_string().contains("DTD"));
    }
}
