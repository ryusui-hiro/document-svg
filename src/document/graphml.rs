//! Bounded GraphML 1.0 graph preview.
//!
//! GraphML is parsed as a data graph and rendered with the existing layered
//! diagram layout. Layout coordinates, custom key semantics, and vendor
//! extensions remain inert; labels and basic yFiles shape hints are retained.

use std::collections::HashSet;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::diagram::{DiagramEdge, DiagramGraph, DiagramNode, NodeShape, layout_and_render_graph};
use crate::error::{Error, Result};

const MAX_GRAPHML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GRAPHML_EVENTS: usize = 1_000_000;
const MAX_GRAPHML_DEPTH: usize = 128;
const MAX_GRAPHML_NODES: usize = 100_000;
const MAX_GRAPHML_EDGES: usize = 200_000;
const MAX_GRAPHML_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_GRAPHML_LABEL_BYTES: usize = 1024 * 1024;
const MAX_GRAPHML_ID_BYTES: usize = 4096;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<graphml") && text.contains("<node")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GRAPHML_BYTES),
        "GraphML input",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("GraphML input must be UTF-8: {error}")))?;
    let (mut graph, warnings) = parse_graphml(text)?;
    graph.raw_source.clear();
    let mut page = layout_and_render_graph(&graph, options)?;
    page.source_format = "graphml".into();
    page.title = graph
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .map(|title| format!("GraphML — {title}"))
        .unwrap_or_else(|| "GraphML graph".into());
    page.description = format!(
        "GraphML graph with {} nodes and {} edges",
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
    shape: NodeShape,
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
    unsupported_graphs: usize,
}

enum CaptureTarget {
    NodeLabel,
    EdgeLabel,
}

fn parse_graphml(text: &str) -> Result<(DiagramGraph, Vec<String>)> {
    if text.len() as u64 > MAX_GRAPHML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GraphML input exceeds {MAX_GRAPHML_BYTES} bytes"
        )));
    }
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut state = ParserState::default();
    let mut root_seen = false;
    loop {
        state.events = state.events.saturating_add(1);
        if state.events > MAX_GRAPHML_EVENTS {
            return Err(Error::LimitExceeded(format!(
                "GraphML exceeds {MAX_GRAPHML_EVENTS} XML events"
            )));
        }
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let local = local_name(start.name().as_ref());
                if !root_seen {
                    root_seen = true;
                    if local != "graphml" {
                        return Err(Error::InvalidInput(
                            "GraphML root element must be graphml".into(),
                        ));
                    }
                }
                state.depth = state.depth.saturating_add(1);
                if state.depth > MAX_GRAPHML_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "GraphML XML nesting exceeds {MAX_GRAPHML_DEPTH} levels"
                    )));
                }
                handle_start(&mut state, &start, &local)?;
            }
            Ok(Event::Empty(empty)) => {
                state.events = state.events.saturating_add(1);
                let local = local_name(empty.name().as_ref());
                if !root_seen {
                    root_seen = true;
                    if local != "graphml" {
                        return Err(Error::InvalidInput(
                            "GraphML root element must be graphml".into(),
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
            Ok(Event::Text(text_event)) => {
                let decoded = text_event.decode().map_err(|error| {
                    Error::InvalidInput(format!("invalid GraphML text: {error}"))
                })?;
                let value = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    Error::InvalidInput(format!("invalid GraphML entity: {error}"))
                })?;
                append_capture(&mut state, value.as_ref())?;
            }
            Ok(Event::CData(cdata)) => {
                let value = String::from_utf8_lossy(cdata.as_ref());
                append_capture(&mut state, &value)?;
            }
            Ok(Event::DocType(_)) => {
                return Err(Error::Unsupported(
                    "GraphML DTD and external entity declarations are unsupported".into(),
                ));
            }
            Ok(Event::Eof) => break,
            Ok(Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::GeneralRef(_)) => {}
            Err(error) => {
                return Err(Error::InvalidInput(format!("invalid GraphML XML: {error}")));
            }
        }
    }
    if !root_seen {
        return Err(Error::InvalidInput("GraphML input is empty".into()));
    }
    if state.node.is_some() || state.edge.is_some() {
        return Err(Error::InvalidInput(
            "GraphML input ended inside a node or edge".into(),
        ));
    }
    if state.graph.nodes.is_empty() {
        return Err(Error::InvalidInput("GraphML contains no nodes".into()));
    }
    if state.nested_nodes > 0 {
        state.warnings.push(format!(
            "GraphML omitted {0} nested node element(s); compound-node layout is not reconstructed",
            state.nested_nodes
        ));
    }
    if state.unsupported_graphs > 0 {
        state.warnings.push(format!(
            "GraphML omitted {0} nested graph element(s); only the first graph is rendered",
            state.unsupported_graphs
        ));
    }
    let known = state.seen_nodes;
    let before = state.graph.edges.len();
    state
        .graph
        .edges
        .retain(|edge| known.contains(&edge.from) && known.contains(&edge.to));
    let omitted_edges = before.saturating_sub(state.graph.edges.len());
    if omitted_edges > 0 {
        state.warnings.push(format!(
            "GraphML omitted {omitted_edges} edge(s) referencing unknown nodes"
        ));
    }
    Ok((state.graph, dedup_warnings(state.warnings)))
}

fn handle_start(state: &mut ParserState, start: &BytesStart<'_>, local: &str) -> Result<()> {
    match local {
        "graph" => {
            if state.graph_seen {
                state.unsupported_graphs = state.unsupported_graphs.saturating_add(1);
            }
            state.graph_seen = true;
            state.graph.is_directed = true;
            if let Some(default) = attr(start, "edgedefault") {
                state.graph.is_directed = !default.eq_ignore_ascii_case("undirected");
            }
            if state.graph.title.is_none() {
                state.graph.title = attr(start, "id");
            }
        }
        "node" => {
            if state.node.is_some() {
                state.nested_nodes = state.nested_nodes.saturating_add(1);
                state.nested_node_depth = state.nested_node_depth.saturating_add(1);
                return Ok(());
            }
            if state.graph.nodes.len() >= MAX_GRAPHML_NODES {
                return Err(Error::LimitExceeded(format!(
                    "GraphML exceeds {MAX_GRAPHML_NODES} nodes"
                )));
            }
            let id = attr(start, "id").ok_or_else(|| {
                Error::InvalidInput("GraphML node is missing its id attribute".into())
            })?;
            validate_id(&id, "node")?;
            if !state.seen_nodes.insert(id.clone()) {
                return Err(Error::InvalidInput(format!(
                    "GraphML node id '{id}' is duplicated"
                )));
            }
            state.node = Some(NodeBuilder {
                label: String::new(),
                id,
                shape: NodeShape::Box,
            });
        }
        "edge" => {
            if state.edge.is_some() {
                return Ok(());
            }
            if state.seen_edges >= MAX_GRAPHML_EDGES {
                return Err(Error::LimitExceeded(format!(
                    "GraphML exceeds {MAX_GRAPHML_EDGES} edges"
                )));
            }
            let from = attr(start, "source").ok_or_else(|| {
                Error::InvalidInput("GraphML edge is missing its source attribute".into())
            })?;
            let to = attr(start, "target").ok_or_else(|| {
                Error::InvalidInput("GraphML edge is missing its target attribute".into())
            })?;
            validate_id(&from, "edge source")?;
            validate_id(&to, "edge target")?;
            if let Some(directed) = attr(start, "directed") {
                let edge_is_directed =
                    matches!(directed.to_ascii_lowercase().as_str(), "true" | "1");
                if edge_is_directed != state.graph.is_directed {
                    push_warning_once(
                        state,
                        "GraphML per-edge directed overrides were flattened to the graph default",
                    );
                }
            }
            state.edge = Some(EdgeBuilder {
                from,
                to,
                label: None,
            });
            state.seen_edges = state.seen_edges.saturating_add(1);
        }
        "nodelabel" => {
            if state.node.is_some() && state.nested_node_depth == 0 {
                if let Some(node) = state.node.as_mut() {
                    node.label.clear();
                }
                state.capture = Some(CaptureTarget::NodeLabel);
            }
        }
        "label" => {
            if state.edge.is_some() {
                if let Some(edge) = state.edge.as_mut() {
                    edge.label = Some(String::new());
                }
                state.capture = Some(CaptureTarget::EdgeLabel);
            }
        }
        "shape" => {
            if state.nested_node_depth == 0
                && let Some(node) = state.node.as_mut()
                && let Some(shape) = attr(start, "type")
            {
                node.shape = match shape.to_ascii_lowercase().as_str() {
                    "ellipse" | "circle" => NodeShape::Circle,
                    "roundrectangle" | "roundedrectangle" | "rounded" => NodeShape::Rounded,
                    "diamond" => NodeShape::Diamond,
                    "cylinder" | "database" => NodeShape::Cylinder,
                    _ => NodeShape::Box,
                };
            }
        }
        "data" => {
            if state
                .node
                .as_ref()
                .is_some_and(|node| node.label.is_empty())
                && state.nested_node_depth == 0
            {
                state.capture = Some(CaptureTarget::NodeLabel);
            } else if state.edge.is_some() {
                state.capture = Some(CaptureTarget::EdgeLabel);
            }
        }
        _ => {}
    }
    Ok(())
}

fn handle_end(state: &mut ParserState, local: &str) -> Result<()> {
    match local {
        "nodelabel" | "label" | "data" => state.capture = None,
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
                    shape: node.shape,
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
    if state.text_bytes > MAX_GRAPHML_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GraphML text exceeds {MAX_GRAPHML_TEXT_BYTES} bytes"
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
    if label.len().saturating_add(text.len()) > MAX_GRAPHML_LABEL_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GraphML label exceeds {MAX_GRAPHML_LABEL_BYTES} bytes"
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
    if id.is_empty() || id.len() > MAX_GRAPHML_ID_BYTES {
        return Err(Error::InvalidInput(format!(
            "GraphML {context} id is empty or exceeds {MAX_GRAPHML_ID_BYTES} bytes"
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

fn push_warning_once(state: &mut ParserState, warning: &str) {
    if !state.warnings.iter().any(|item| item == warning) {
        state.warnings.push(warning.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{looks_like_prefix, parse_graphml};

    #[test]
    fn parses_nodes_edges_yfiles_labels_and_shapes() {
        let source = r#"<?xml version="1.0"?>
<graphml xmlns="http://graphml.graphdrawing.org/xmlns" xmlns:y="http://www.yworks.com/xml/graphml">
  <graph id="workflow" edgedefault="directed">
    <node id="start"><data key="d"><y:ShapeNode><y:NodeLabel>Start</y:NodeLabel><y:Shape type="ellipse"/></y:ShapeNode></data></node>
    <node id="finish"><data key="d"><y:NodeLabel>Finish</y:NodeLabel></data></node>
    <edge id="e1" source="start" target="finish"><data key="label">go</data></edge>
  </graph>
</graphml>"#;
        let (graph, warnings) = parse_graphml(source).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(graph.title.as_deref(), Some("workflow"));
        assert_eq!(graph.nodes[0].label, "Start");
        assert_eq!(graph.edges[0].label.as_deref(), Some("go"));
    }

    #[test]
    fn requires_graphml_markers_for_sniffing() {
        assert!(looks_like_prefix(
            b"<graphml><graph><node id='a'/><edge source='a' target='a'/></graph></graphml>"
        ));
        assert!(!looks_like_prefix(b"<xml><node/><edge/></xml>"));
    }

    #[test]
    fn rejects_dtds_before_external_processing() {
        let source = "<!DOCTYPE graphml SYSTEM 'https://example.invalid/graphml.dtd'><graphml/>";
        let error = parse_graphml(source).unwrap_err();
        assert!(error.to_string().contains("DTD"));
    }
}
