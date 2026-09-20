//! Bounded GEXF 1.3 graph preview.
//!
//! GEXF (Graph Exchange XML Format) is commonly emitted by Gephi. This
//! adapter keeps the static node/edge topology and labels while leaving
//! dynamic spells, attributes, layouts, plugins, and external resources inert.

use std::collections::HashSet;
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::diagram::{DiagramEdge, DiagramGraph, DiagramNode, NodeShape, layout_and_render_graph};
use crate::error::{Error, Result};

const MAX_GEXF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GEXF_EVENTS: usize = 1_000_000;
const MAX_GEXF_DEPTH: usize = 128;
const MAX_GEXF_NODES: usize = 100_000;
const MAX_GEXF_EDGES: usize = 200_000;
const MAX_GEXF_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_GEXF_LABEL_BYTES: usize = 1024 * 1024;
const MAX_GEXF_ID_BYTES: usize = 4096;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    text.contains("<gexf") && text.contains("<node")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GEXF_BYTES),
        "GEXF input",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("GEXF input must be UTF-8: {error}")))?;
    let (graph, warnings) = parse_gexf(text)?;
    let mut page = layout_and_render_graph(&graph, options)?;
    page.source_format = "gexf".into();
    page.title = graph
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .map(|title| format!("GEXF — {title}"))
        .unwrap_or_else(|| "GEXF graph".into());
    page.description = format!(
        "GEXF graph with {} nodes and {} edges",
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
    dynamic: bool,
    attributes: bool,
}

enum CaptureTarget {
    NodeLabel,
    EdgeLabel,
}

fn parse_gexf(text: &str) -> Result<(DiagramGraph, Vec<String>)> {
    if text.len() as u64 > MAX_GEXF_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GEXF input exceeds {MAX_GEXF_BYTES} bytes"
        )));
    }
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut state = ParserState::default();
    let mut root_seen = false;
    loop {
        state.events = state.events.saturating_add(1);
        if state.events > MAX_GEXF_EVENTS {
            return Err(Error::LimitExceeded(format!(
                "GEXF exceeds {MAX_GEXF_EVENTS} XML events"
            )));
        }
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let local = local_name(start.name().as_ref());
                if !root_seen {
                    root_seen = true;
                    if local != "gexf" {
                        return Err(Error::InvalidInput("GEXF root element must be gexf".into()));
                    }
                }
                state.depth = state.depth.saturating_add(1);
                if state.depth > MAX_GEXF_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "GEXF XML nesting exceeds {MAX_GEXF_DEPTH} levels"
                    )));
                }
                handle_start(&mut state, &start, &local)?;
            }
            Ok(Event::Empty(empty)) => {
                state.events = state.events.saturating_add(1);
                let local = local_name(empty.name().as_ref());
                if !root_seen {
                    root_seen = true;
                    if local != "gexf" {
                        return Err(Error::InvalidInput("GEXF root element must be gexf".into()));
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
                    .map_err(|error| Error::InvalidInput(format!("invalid GEXF text: {error}")))?;
                let value = quick_xml::escape::unescape(&decoded).map_err(|error| {
                    Error::InvalidInput(format!("invalid GEXF entity: {error}"))
                })?;
                append_capture(&mut state, value.as_ref())?;
            }
            Ok(Event::CData(event)) => {
                let value = event
                    .decode()
                    .map_err(|error| Error::InvalidInput(format!("invalid GEXF CDATA: {error}")))?;
                append_capture(&mut state, value.as_ref())?;
            }
            Ok(Event::DocType(_)) => {
                return Err(Error::Unsupported(
                    "GEXF DTD and external entity declarations are unsupported".into(),
                ));
            }
            Ok(Event::Eof) => break,
            Ok(Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::GeneralRef(_)) => {}
            Err(error) => {
                return Err(Error::InvalidInput(format!("invalid GEXF XML: {error}")));
            }
        }
    }
    if !root_seen {
        return Err(Error::InvalidInput("GEXF input is empty".into()));
    }
    if state.node.is_some() || state.edge.is_some() {
        return Err(Error::InvalidInput(
            "GEXF input ended inside a node or edge".into(),
        ));
    }
    if state.graph.nodes.is_empty() {
        return Err(Error::InvalidInput("GEXF contains no nodes".into()));
    }
    if state.nested_nodes > 0 {
        state.warnings.push(format!(
            "GEXF omitted {} nested node element(s); compound-node layout is not reconstructed",
            state.nested_nodes
        ));
    }
    if state.dynamic {
        state.warnings.push(
            "GEXF dynamic spells/time attributes were omitted; the static topology is shown".into(),
        );
    }
    if state.attributes {
        state
            .warnings
            .push("GEXF node/edge attributes were omitted from the diagram labels".into());
    }
    let before = state.graph.edges.len();
    state.graph.edges.retain(|edge| {
        state.seen_nodes.contains(&edge.from) && state.seen_nodes.contains(&edge.to)
    });
    let omitted_edges = before.saturating_sub(state.graph.edges.len());
    if omitted_edges > 0 {
        state.warnings.push(format!(
            "GEXF omitted {omitted_edges} edge(s) referencing unknown nodes"
        ));
    }
    Ok((state.graph, dedup_warnings(state.warnings)))
}

fn handle_start(state: &mut ParserState, start: &BytesStart<'_>, local: &str) -> Result<()> {
    match local {
        "graph" => {
            if state.graph_seen {
                return Err(Error::Unsupported(
                    "GEXF multiple graph elements are unsupported".into(),
                ));
            }
            state.graph_seen = true;
            state.graph.is_directed = true;
            if let Some(edge_type) = attr(start, "defaultedgetype") {
                state.graph.is_directed = !edge_type.eq_ignore_ascii_case("undirected");
            }
            state.graph.title = attr(start, "id");
            if attr(start, "mode").is_some_and(|mode| mode.eq_ignore_ascii_case("dynamic")) {
                state.dynamic = true;
            }
        }
        "node" => {
            if state.node.is_some() {
                state.nested_nodes = state.nested_nodes.saturating_add(1);
                state.nested_node_depth = state.nested_node_depth.saturating_add(1);
                return Ok(());
            }
            if state.graph.nodes.len() >= MAX_GEXF_NODES {
                return Err(Error::LimitExceeded(format!(
                    "GEXF exceeds {MAX_GEXF_NODES} nodes"
                )));
            }
            let id = attr(start, "id")
                .ok_or_else(|| Error::InvalidInput("GEXF node is missing its id".into()))?;
            validate_id(&id, "node")?;
            if !state.seen_nodes.insert(id.clone()) {
                return Err(Error::InvalidInput(format!(
                    "GEXF node id '{id}' is duplicated"
                )));
            }
            let label = attr(start, "label").unwrap_or_default();
            if label.len() > MAX_GEXF_LABEL_BYTES {
                return Err(Error::LimitExceeded(format!(
                    "GEXF label exceeds {MAX_GEXF_LABEL_BYTES} bytes"
                )));
            }
            state.node = Some(NodeBuilder { label, id });
        }
        "edge" => {
            if state.edge.is_some() {
                return Ok(());
            }
            if state.seen_edges >= MAX_GEXF_EDGES {
                return Err(Error::LimitExceeded(format!(
                    "GEXF exceeds {MAX_GEXF_EDGES} edges"
                )));
            }
            let from = attr(start, "source")
                .ok_or_else(|| Error::InvalidInput("GEXF edge is missing its source".into()))?;
            let to = attr(start, "target")
                .ok_or_else(|| Error::InvalidInput("GEXF edge is missing its target".into()))?;
            validate_id(&from, "edge source")?;
            validate_id(&to, "edge target")?;
            let label = attr(start, "label");
            if label
                .as_ref()
                .is_some_and(|value| value.len() > MAX_GEXF_LABEL_BYTES)
            {
                return Err(Error::LimitExceeded(format!(
                    "GEXF label exceeds {MAX_GEXF_LABEL_BYTES} bytes"
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
        "attvalues" | "attvalue" | "attributes" | "spells" | "spell" => {
            if matches!(local, "attvalues" | "attvalue" | "attributes") {
                state.attributes = true;
            }
            if matches!(local, "spells" | "spell") {
                state.dynamic = true;
            }
        }
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
    if state.text_bytes > MAX_GEXF_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GEXF text exceeds {MAX_GEXF_TEXT_BYTES} bytes"
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
    if label.len().saturating_add(text.len()) > MAX_GEXF_LABEL_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GEXF label exceeds {MAX_GEXF_LABEL_BYTES} bytes"
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
    if id.is_empty() || id.len() > MAX_GEXF_ID_BYTES {
        return Err(Error::InvalidInput(format!(
            "GEXF {context} id is empty or exceeds {MAX_GEXF_ID_BYTES} bytes"
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
    use super::{looks_like_prefix, parse_gexf};

    #[test]
    fn parses_static_gexf_nodes_edges_and_labels() {
        let source = r#"<?xml version="1.0"?>
<gexf xmlns="http://gexf.net/1.3" version="1.3">
  <graph mode="static" defaultedgetype="directed" id="workflow">
    <nodes><node id="a" label="Start"/><node id="b"><label>Done</label></node></nodes>
    <edges><edge id="e" source="a" target="b" label="approve"/></edges>
  </graph>
</gexf>"#;
        let (graph, warnings) = parse_gexf(source).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(graph.title.as_deref(), Some("workflow"));
        assert_eq!(graph.nodes[0].label, "Start");
        assert_eq!(graph.nodes[1].label, "Done");
        assert_eq!(graph.edges[0].label.as_deref(), Some("approve"));
    }

    #[test]
    fn recognizes_gexf_without_confusing_generic_xml() {
        assert!(looks_like_prefix(
            b"<gexf><graph><nodes><node id='a'/></nodes></graph></gexf>"
        ));
        assert!(!looks_like_prefix(b"<graphml><node id='a'/></graphml>"));
    }

    #[test]
    fn rejects_dtd_before_external_processing() {
        let source = "<!DOCTYPE gexf SYSTEM 'https://example.invalid/gexf.dtd'><gexf/>";
        let error = parse_gexf(source).unwrap_err();
        assert!(error.to_string().contains("DTD"));
    }
}
