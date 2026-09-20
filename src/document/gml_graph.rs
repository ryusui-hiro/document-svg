//! Bounded Graph Modeling Language (GML) graph preview.
//!
//! GML is a bracketed text graph format distinct from geographic GML XML.
//! This adapter accepts the common `graph [ node [...] edge [...] ]` subset,
//! preserving topology and labels while keeping arbitrary attributes inert.

use std::collections::HashSet;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::diagram::{DiagramEdge, DiagramGraph, DiagramNode, NodeShape, layout_and_render_graph};
use crate::error::{Error, Result};

const MAX_GML_BYTES: u64 = 64 * 1024 * 1024;
const MAX_GML_TOKENS: usize = 1_000_000;
const MAX_GML_TOKEN_BYTES: usize = 4096;
const MAX_GML_NODES: usize = 100_000;
const MAX_GML_EDGES: usize = 200_000;
const MAX_GML_LABEL_BYTES: usize = 1024 * 1024;
const MAX_GML_ID_BYTES: usize = 4096;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or_default();
    trimmed.starts_with("graph")
        && trimmed
            .as_bytes()
            .get(5)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'[')
        && trimmed.contains('[')
        && text.contains("node")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GML_BYTES),
        "Graph Modeling Language input",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("Graph GML input must be UTF-8: {error}")))?;
    let (graph, warnings) = parse_graph_gml(text)?;
    let mut page = layout_and_render_graph(&graph, options)?;
    page.source_format = "graph_gml".into();
    page.title = graph
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .map(|title| format!("Graph GML — {title}"))
        .unwrap_or_else(|| "Graph Modeling Language graph".into());
    page.description = format!(
        "Graph Modeling Language graph with {} nodes and {} edges",
        graph.nodes.len(),
        graph.edges.len()
    );
    for warning in &warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(warnings)
}

#[derive(Clone, Debug)]
struct Tokenizer<'a> {
    source: &'a str,
    position: usize,
}

fn tokenize(source: &str) -> Result<Vec<String>> {
    let mut tokenizer = Tokenizer {
        source,
        position: 0,
    };
    let mut tokens = Vec::new();
    while let Some(token) = tokenizer.next_token()? {
        if tokens.len() >= MAX_GML_TOKENS {
            return Err(Error::LimitExceeded(format!(
                "Graph GML exceeds {MAX_GML_TOKENS} tokens"
            )));
        }
        tokens.push(token);
    }
    Ok(tokens)
}

impl<'a> Tokenizer<'a> {
    fn next_token(&mut self) -> Result<Option<String>> {
        let bytes = self.source.as_bytes();
        while self.position < bytes.len() {
            match bytes[self.position] {
                b' ' | b'\t' | b'\r' | b'\n' => self.position += 1,
                b'#' => {
                    while self.position < bytes.len() && bytes[self.position] != b'\n' {
                        self.position += 1;
                    }
                }
                _ => break,
            }
        }
        if self.position >= bytes.len() {
            return Ok(None);
        }
        if matches!(bytes[self.position], b'[' | b']') {
            let token = (bytes[self.position] as char).to_string();
            self.position += 1;
            return Ok(Some(token));
        }
        if bytes[self.position] == b'"' {
            self.position += 1;
            let mut value = String::new();
            let mut escaped = false;
            while self.position < bytes.len() {
                let character = self.source[self.position..]
                    .chars()
                    .next()
                    .unwrap_or_default();
                self.position += character.len_utf8();
                if escaped {
                    value.push(match character {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    if value.len() > MAX_GML_LABEL_BYTES {
                        return Err(Error::LimitExceeded(format!(
                            "Graph GML quoted value exceeds {MAX_GML_LABEL_BYTES} bytes"
                        )));
                    }
                    return Ok(Some(value));
                } else {
                    value.push(character);
                }
            }
            return Err(Error::InvalidInput(
                "Graph GML quoted value is not closed".into(),
            ));
        }
        let start = self.position;
        while self.position < bytes.len()
            && !bytes[self.position].is_ascii_whitespace()
            && !matches!(bytes[self.position], b'[' | b']' | b'#')
        {
            self.position += 1;
        }
        let token = self.source.get(start..self.position).unwrap_or_default();
        if token.is_empty() {
            return Err(Error::InvalidInput(
                "Graph GML contains an invalid token".into(),
            ));
        }
        if token.len() > MAX_GML_TOKEN_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Graph GML token exceeds {MAX_GML_TOKEN_BYTES} bytes"
            )));
        }
        Ok(Some(token.to_string()))
    }
}

fn parse_graph_gml(text: &str) -> Result<(DiagramGraph, Vec<String>)> {
    if text.len() as u64 > MAX_GML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Graph GML input exceeds {MAX_GML_BYTES} bytes"
        )));
    }
    let tokens = tokenize(text)?;
    if tokens.len() < 2 || !tokens[0].eq_ignore_ascii_case("graph") || tokens[1] != "[" {
        return Err(Error::InvalidInput(
            "Graph GML must start with graph [".into(),
        ));
    }
    let mut graph = DiagramGraph {
        is_directed: false,
        ..Default::default()
    };
    let mut warnings = Vec::new();
    let mut seen_nodes = HashSet::new();
    let mut edge_count = 0usize;
    let mut index = 2usize;
    while index < tokens.len() && tokens[index] != "]" {
        let key = tokens[index].to_ascii_lowercase();
        index += 1;
        match key.as_str() {
            "node" => {
                let (node, next) = parse_node(&tokens, index, &mut warnings)?;
                index = next;
                if graph.nodes.len() >= MAX_GML_NODES {
                    return Err(Error::LimitExceeded(format!(
                        "Graph GML exceeds {MAX_GML_NODES} nodes"
                    )));
                }
                if !seen_nodes.insert(node.id.clone()) {
                    return Err(Error::InvalidInput(format!(
                        "Graph GML node id '{}' is duplicated",
                        node.id
                    )));
                }
                graph.nodes.push(node);
            }
            "edge" => {
                let (edge, next) = parse_edge(&tokens, index, &mut warnings)?;
                index = next;
                edge_count = edge_count.saturating_add(1);
                if edge_count > MAX_GML_EDGES {
                    return Err(Error::LimitExceeded(format!(
                        "Graph GML exceeds {MAX_GML_EDGES} edges"
                    )));
                }
                graph.edges.push(edge);
            }
            "directed" => {
                let (value, next) = scalar(&tokens, index)?;
                index = next;
                graph.is_directed = matches!(value.trim(), "1" | "true" | "TRUE");
            }
            "label" => {
                let (value, next) = scalar(&tokens, index)?;
                index = next;
                if value.len() > MAX_GML_LABEL_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "Graph GML label exceeds {MAX_GML_LABEL_BYTES} bytes"
                    )));
                }
                graph.title = Some(value);
            }
            _ => {
                let next = skip_value(&tokens, index)?;
                index = next;
                if key != "comment" {
                    push_warning_once(
                        &mut warnings,
                        "Graph GML attributes other than node/edge labels and directed were omitted",
                    );
                }
            }
        }
    }
    if index >= tokens.len() || tokens[index] != "]" {
        return Err(Error::InvalidInput(
            "Graph GML graph container is not closed".into(),
        ));
    }
    if index + 1 != tokens.len() {
        return Err(Error::InvalidInput(
            "Graph GML contains trailing tokens after the graph container".into(),
        ));
    }
    if graph.nodes.is_empty() {
        return Err(Error::InvalidInput("Graph GML contains no nodes".into()));
    }
    let before = graph.edges.len();
    graph
        .edges
        .retain(|edge| seen_nodes.contains(&edge.from) && seen_nodes.contains(&edge.to));
    if before != graph.edges.len() {
        warnings.push(format!(
            "Graph GML omitted {} edge(s) referencing unknown nodes",
            before - graph.edges.len()
        ));
    }
    warnings.sort();
    warnings.dedup();
    Ok((graph, warnings))
}

fn parse_node(
    tokens: &[String],
    start: usize,
    warnings: &mut Vec<String>,
) -> Result<(DiagramNode, usize)> {
    let (fields, index) = fields(tokens, start)?;
    let id = fields
        .get("id")
        .cloned()
        .ok_or_else(|| Error::InvalidInput("Graph GML node is missing id".into()))?;
    validate_id(&id, "node")?;
    let label = fields.get("label").cloned().unwrap_or_else(|| id.clone());
    if fields.contains_key("graphics") {
        push_warning_once(
            warnings,
            "Graph GML node graphics and coordinates were omitted",
        );
    }
    Ok((
        DiagramNode {
            id,
            label,
            shape: NodeShape::Box,
            x: 0.0,
            y: 0.0,
            width: 130.0,
            height: 46.0,
        },
        index,
    ))
}

fn parse_edge(
    tokens: &[String],
    start: usize,
    warnings: &mut Vec<String>,
) -> Result<(DiagramEdge, usize)> {
    let (fields, index) = fields(tokens, start)?;
    let from = fields
        .get("source")
        .cloned()
        .ok_or_else(|| Error::InvalidInput("Graph GML edge is missing source".into()))?;
    let to = fields
        .get("target")
        .cloned()
        .ok_or_else(|| Error::InvalidInput("Graph GML edge is missing target".into()))?;
    validate_id(&from, "edge source")?;
    validate_id(&to, "edge target")?;
    if fields.contains_key("graphics") {
        push_warning_once(
            warnings,
            "Graph GML edge graphics and coordinates were omitted",
        );
    }
    Ok((
        DiagramEdge {
            from,
            to,
            label: fields.get("label").cloned(),
        },
        index,
    ))
}

fn fields(
    tokens: &[String],
    start: usize,
) -> Result<(std::collections::HashMap<String, String>, usize)> {
    if tokens.get(start).map(String::as_str) != Some("[") {
        return Err(Error::InvalidInput(
            "Graph GML node/edge must contain a bracketed field list".into(),
        ));
    }
    let mut fields = std::collections::HashMap::new();
    let mut index = start + 1;
    while index < tokens.len() && tokens[index] != "]" {
        let key = tokens[index].to_ascii_lowercase();
        index += 1;
        if key == "node" || key == "edge" {
            index = skip_value(tokens, index)?;
            continue;
        }
        if tokens.get(index).map(String::as_str) == Some("[") {
            if key == "graphics" {
                fields.insert(key, "[graphics]".into());
            }
            index = skip_value(tokens, index)?;
            continue;
        }
        let (value, next) = scalar(tokens, index)?;
        index = next;
        if key == "id" || key == "label" || key == "source" || key == "target" || key == "graphics"
        {
            fields.insert(key, value);
        }
    }
    if index >= tokens.len() {
        return Err(Error::InvalidInput(
            "Graph GML node/edge container is not closed".into(),
        ));
    }
    Ok((fields, index + 1))
}

fn scalar(tokens: &[String], start: usize) -> Result<(String, usize)> {
    let value = tokens
        .get(start)
        .cloned()
        .ok_or_else(|| Error::InvalidInput("Graph GML field is missing a value".into()))?;
    if value == "[" || value == "]" {
        return Err(Error::InvalidInput(
            "Graph GML field value is not scalar".into(),
        ));
    }
    Ok((value, start + 1))
}

fn skip_value(tokens: &[String], start: usize) -> Result<usize> {
    if tokens.get(start).map(String::as_str) != Some("[") {
        return Ok(start.saturating_add(1));
    }
    let mut depth = 0usize;
    let mut index = start;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "[" => depth = depth.saturating_add(1),
            "]" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Ok(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    Err(Error::InvalidInput(
        "Graph GML nested field container is not closed".into(),
    ))
}

fn validate_id(id: &str, context: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_GML_ID_BYTES {
        return Err(Error::InvalidInput(format!(
            "Graph GML {context} id is empty or exceeds {MAX_GML_ID_BYTES} bytes"
        )));
    }
    Ok(())
}

fn push_warning_once(warnings: &mut Vec<String>, warning: &str) {
    if !warnings.iter().any(|item| item == warning) {
        warnings.push(warning.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{looks_like_prefix, parse_graph_gml};

    #[test]
    fn parses_gml_graph_nodes_edges_and_comments() {
        let source = r#"graph [
          directed 1
          label "Workflow"
          node [ id 1 label "Start" graphics [ x 1 y 2 ] ]
          node [ id 2 label "Done" ]
          edge [ source 1 target 2 label "go" ]
        ]"#;
        let (graph, warnings) = parse_graph_gml(source).unwrap();
        assert!(graph.is_directed);
        assert_eq!(graph.title.as_deref(), Some("Workflow"));
        assert_eq!(graph.nodes[0].label, "Start");
        assert_eq!(graph.edges[0].label.as_deref(), Some("go"));
        assert!(warnings.iter().any(|warning| warning.contains("graphics")));
    }

    #[test]
    fn distinguishes_graph_gml_from_geographic_xml_gml() {
        assert!(looks_like_prefix(b"graph [ node [ id 1 ] ]"));
        assert!(looks_like_prefix(b"# comment\ngraph [ node [ id 1 ] ]"));
        assert!(!looks_like_prefix(b"<gml><node/></gml>"));
    }

    #[test]
    fn rejects_unclosed_quotes() {
        let error = parse_graph_gml("graph [ node [ id 1 label \"bad ] ]").unwrap_err();
        assert!(error.to_string().contains("quoted"));
    }
}
