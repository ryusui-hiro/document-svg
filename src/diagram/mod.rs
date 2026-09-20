//! Diagram parsing (Graphviz DOT and Mermaid), hierarchical layout, and reverse extraction.

pub mod d2;
pub mod excalidraw;
pub mod plantuml;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{
    IDENTITY, LineCap, LineJoin, Node, Page, Paint, SourceMeta, Stroke, TextAnchor, TextRun,
};

#[derive(Clone, Debug)]
pub struct DiagramNode {
    pub id: String,
    pub label: String,
    pub shape: NodeShape,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeShape {
    Box,
    Rounded,
    Circle,
    Diamond,
    Cylinder,
}

#[derive(Clone, Debug)]
pub struct DiagramEdge {
    pub from: String,
    pub to: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct DiagramGraph {
    pub title: Option<String>,
    pub is_directed: bool,
    pub nodes: Vec<DiagramNode>,
    pub edges: Vec<DiagramEdge>,
    pub raw_source: String,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let source_bytes = read_limited_file(path, options.max_input_bytes, "diagram input")?;
    let source = String::from_utf8(source_bytes)
        .map_err(|e| Error::InvalidInput(format!("diagram file is not valid UTF-8: {e}")))?;

    let trimmed = source.trim_start();
    if trimmed.starts_with("sequenceDiagram") {
        let seq = parse_sequence_diagram(&source)?;
        let page = layout_and_render_sequence(&seq, options)?;
        sink.consume(page)?;
        return Ok(Vec::new());
    }

    let is_mermaid = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mmd") || ext.eq_ignore_ascii_case("mermaid"))
        || trimmed.starts_with("graph")
        || trimmed.starts_with("flowchart")
        || trimmed.starts_with("sequenceDiagram")
        || trimmed.starts_with("classDiagram")
        || trimmed.starts_with("stateDiagram")
        || trimmed.starts_with("erDiagram")
        || trimmed.starts_with("gantt")
        || trimmed.starts_with("pie")
        || trimmed.starts_with("mindmap")
        || trimmed.starts_with("gitGraph");

    let graph = if is_mermaid {
        parse_mermaid(&source)?
    } else {
        parse_dot(&source)?
    };

    let page = layout_and_render_graph(&graph, options)?;
    sink.consume(page)?;
    Ok(Vec::new())
}

/// Parses Graphviz DOT syntax subset
pub fn parse_dot(source: &str) -> Result<DiagramGraph> {
    let mut graph = DiagramGraph {
        is_directed: true,
        raw_source: source.to_string(),
        ..Default::default()
    };

    let mut node_labels: HashMap<String, (String, NodeShape)> = HashMap::new();
    let mut edges: Vec<DiagramEdge> = Vec::new();
    let mut node_set: HashSet<String> = HashSet::new();

    let tokens = tokenize(source);
    let mut i = 0;

    // Check digraph / graph
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok == "digraph" || tok == "graph" {
            graph.is_directed = tok == "digraph";
            i += 1;
            if i < tokens.len() && tokens[i] != "{" {
                graph.title = Some(tokens[i].clone());
                i += 1;
            }
            break;
        }
        i += 1;
    }

    // Inside graph body
    while i < tokens.len() {
        let tok = &tokens[i];
        if tok == "}" {
            break;
        }

        // Edge: A -> B
        if i + 1 < tokens.len() && (tokens[i + 1] == "->" || tokens[i + 1] == "--") {
            let from = clean_id(tok);
            let mut j = i + 1;
            while j < tokens.len() && (tokens[j] == "->" || tokens[j] == "--") {
                j += 1;
                if j >= tokens.len() {
                    break;
                }
                let to = clean_id(&tokens[j]);
                node_set.insert(from.clone());
                node_set.insert(to.clone());

                let mut edge_label = None;
                if j + 1 < tokens.len() && tokens[j + 1] == "[" {
                    let (attrs, next_idx) = parse_attributes(&tokens, j + 1);
                    edge_label = attrs.get("label").cloned();
                    j = next_idx;
                }
                edges.push(DiagramEdge {
                    from: from.clone(),
                    to: to.clone(),
                    label: edge_label,
                });
                j += 1;
            }
            i = j;
            continue;
        } else if i + 1 < tokens.len() && tokens[i + 1] == "[" {
            let id = clean_id(tok);
            let (attrs, next_idx) = parse_attributes(&tokens, i + 1);
            let label = attrs.get("label").cloned().unwrap_or_else(|| id.clone());
            let shape = match attrs.get("shape").map(|s| s.as_str()) {
                Some("circle") | Some("ellipse") => NodeShape::Circle,
                Some("diamond") => NodeShape::Diamond,
                Some("cylinder") => NodeShape::Cylinder,
                Some("rounded") => NodeShape::Rounded,
                _ => NodeShape::Box,
            };
            node_labels.insert(id.clone(), (label, shape));
            node_set.insert(id);
            i = next_idx + 1;
            continue;
        } else if !tok.is_empty()
            && tok != ";"
            && tok != "{"
            && tok != "}"
            && !tok.starts_with("subgraph")
        {
            let id = clean_id(tok);
            if !id.is_empty() {
                node_set.insert(id);
            }
        }
        i += 1;
    }

    for id in node_set {
        let (label, shape) = node_labels
            .remove(&id)
            .unwrap_or_else(|| (id.clone(), NodeShape::Rounded));
        graph.nodes.push(DiagramNode {
            id,
            label,
            shape,
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 44.0,
        });
    }
    graph.edges = edges;
    graph.nodes.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(graph)
}

/// Parses Mermaid flowchart syntax subset
pub fn parse_mermaid(source: &str) -> Result<DiagramGraph> {
    let mut graph = DiagramGraph {
        is_directed: true,
        raw_source: source.to_string(),
        ..Default::default()
    };

    let mut node_map: HashMap<String, (String, NodeShape)> = HashMap::new();
    let mut edges: Vec<DiagramEdge> = Vec::new();

    for line in source.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with("%%")
            || line.starts_with("graph")
            || line.starts_with("flowchart")
            || line.starts_with("stateDiagram")
            || line.starts_with("classDiagram")
            || line.starts_with("erDiagram")
            || line.starts_with("gitGraph")
            || line.starts_with("subgraph")
            || line == "end"
            || line.starts_with("direction ")
            || line.starts_with("note ")
        {
            continue;
        }

        let mut current_segment = line;
        let mut had_arrow = false;

        while let Some((arrow_pos, arrow_len, is_bidirectional)) =
            find_mermaid_arrow(current_segment)
        {
            had_arrow = true;
            let left_part = current_segment[..arrow_pos].trim();
            let remainder = current_segment[arrow_pos + arrow_len..].trim();

            // Check for `A -- text --> B` inline arrow label
            let (actual_left, inline_label) = if let Some(dash_idx) = left_part.find("--") {
                let node_str = left_part[..dash_idx].trim();
                let lbl = left_part[dash_idx + 2..]
                    .trim()
                    .trim_matches('"')
                    .to_string();
                (node_str, if lbl.is_empty() { None } else { Some(lbl) })
            } else {
                (left_part, None)
            };

            let (from_id, from_label, from_shape) = parse_mermaid_node_token(actual_left);

            // Check for `-->|text| B` pipe label
            let (post_label_part, pipe_label) = if let Some(stripped) = remainder.strip_prefix('|')
            {
                if let Some(end_bar) = stripped.find('|') {
                    (
                        stripped[end_bar + 1..].trim(),
                        Some(stripped[..end_bar].trim().to_string()),
                    )
                } else {
                    (remainder, None)
                }
            } else {
                (remainder, None)
            };

            // If there is another arrow in post_label_part, the target node of this segment
            // is the part before that next arrow, and current_segment continues from post_label_part.
            let (actual_right, right_label, next_segment) =
                if let Some((next_pos, _, _)) = find_mermaid_arrow(post_label_part) {
                    let target_node = post_label_part[..next_pos].trim();
                    (target_node, None, post_label_part)
                } else if let Some(colon_pos) = post_label_part.find(':') {
                    let node_str = post_label_part[..colon_pos].trim();
                    let lbl = post_label_part[colon_pos + 1..]
                        .trim()
                        .trim_matches('"')
                        .to_string();
                    (node_str, if lbl.is_empty() { None } else { Some(lbl) }, "")
                } else {
                    (post_label_part, None, "")
                };

            let label = pipe_label.or(inline_label).or(right_label);
            let (to_id, to_label, to_shape) = parse_mermaid_node_token(actual_right);

            if !from_id.is_empty() {
                node_map
                    .entry(from_id.clone())
                    .or_insert((from_label, from_shape));
            }
            if !to_id.is_empty() {
                node_map
                    .entry(to_id.clone())
                    .or_insert((to_label, to_shape));
            }

            if !from_id.is_empty() && !to_id.is_empty() {
                edges.push(DiagramEdge {
                    from: from_id.clone(),
                    to: to_id.clone(),
                    label,
                });
                if is_bidirectional {
                    edges.push(DiagramEdge {
                        from: to_id,
                        to: from_id,
                        label: None,
                    });
                }
            }

            if next_segment.is_empty() {
                break;
            }
            current_segment = next_segment;
        }

        if !had_arrow {
            let (id, label, shape) = parse_mermaid_node_token(line);
            if !id.is_empty() {
                node_map.entry(id).or_insert((label, shape));
            }
        }
    }

    for (id, (label, shape)) in node_map {
        graph.nodes.push(DiagramNode {
            id,
            label,
            shape,
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 44.0,
        });
    }
    graph.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    graph.edges = edges;

    Ok(graph)
}

fn find_mermaid_arrow(s: &str) -> Option<(usize, usize, bool)> {
    if let Some(idx) = s.find("<-->") {
        Some((idx, 4, true))
    } else if let Some(idx) = s.find("-.->") {
        Some((idx, 4, false))
    } else if let Some(idx) = s.find("<|--") {
        Some((idx, 4, true))
    } else if let Some(idx) = s.find("--|>") {
        Some((idx, 4, false))
    } else if let Some(idx) = s.find("*--") {
        Some((idx, 3, false))
    } else if let Some(idx) = s.find("--*") {
        Some((idx, 3, false))
    } else if let Some(idx) = s.find("o--") {
        Some((idx, 3, false))
    } else if let Some(idx) = s.find("--o") {
        Some((idx, 3, false))
    } else if let Some(idx) = s.find("==>") {
        Some((idx, 3, false))
    } else if let Some(idx) = s.find("-->") {
        Some((idx, 3, false))
    } else {
        s.find("---").map(|idx| (idx, 3, false))
    }
}

fn parse_mermaid_node_token(token: &str) -> (String, String, NodeShape) {
    let t = token.trim();
    if t == "[*]" {
        return ("[*]".to_string(), "●".to_string(), NodeShape::Circle);
    }
    if let (Some(start), Some(end)) = (token.find("[["), token.rfind("]]"))
        && start + 2 <= end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 2..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Box);
    }
    if let (Some(start), Some(end)) = (token.find("[("), token.rfind(")]"))
        && start + 2 <= end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 2..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Cylinder);
    }
    if let (Some(start), Some(end)) = (token.find("(["), token.rfind("])"))
        && start + 2 <= end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 2..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Rounded);
    }
    if let (Some(start), Some(end)) = (token.find("((("), token.rfind(")))"))
        && start + 3 <= end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 3..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Circle);
    }
    if let (Some(start), Some(end)) = (token.find("(("), token.rfind("))"))
        && start + 2 <= end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 2..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Circle);
    }
    if let (Some(start), Some(end)) = (token.find("{{"), token.rfind("}}"))
        && start + 2 <= end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 2..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Diamond);
    }
    if let (Some(start), Some(end)) = (token.find('>'), token.rfind(']'))
        && start < end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 1..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Box);
    }
    if let (Some(start), Some(end)) = (token.find('['), token.rfind(']'))
        && start < end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 1..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Box);
    }
    if let (Some(start), Some(end)) = (token.find('{'), token.rfind('}'))
        && start < end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 1..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Diamond);
    }
    if let (Some(start), Some(end)) = (token.find('('), token.rfind(')'))
        && start < end
    {
        let id = token[..start].trim().to_string();
        let label = token[start + 1..end].trim().trim_matches('"').to_string();
        return (id, label, NodeShape::Rounded);
    }
    let id = clean_id(token);
    (id.clone(), id, NodeShape::Rounded)
}

fn clean_id(raw: &str) -> String {
    raw.trim().trim_matches('"').trim_matches(';').to_string()
}

fn tokenize(source: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '/' && chars.clone().nth(1) == Some('/') {
            for ch in chars.by_ref() {
                if ch == '\n' {
                    break;
                }
            }
        } else if c == '"' {
            chars.next();
            let mut s = String::new();
            for ch in chars.by_ref() {
                if ch == '"' {
                    break;
                }
                s.push(ch);
            }
            tokens.push(format!("\"{s}\""));
        } else if c == '-' && chars.clone().nth(1) == Some('>') {
            chars.next();
            chars.next();
            tokens.push("->".to_string());
        } else if c == '-' && chars.clone().nth(1) == Some('-') {
            chars.next();
            chars.next();
            tokens.push("--".to_string());
        } else if "{}[];=,".contains(c) {
            tokens.push(c.to_string());
            chars.next();
        } else {
            let mut s = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || "{}[];=,\"-/".contains(ch) {
                    break;
                }
                s.push(ch);
                chars.next();
            }
            if !s.is_empty() {
                tokens.push(s);
            }
        }
    }
    tokens
}

fn parse_attributes(tokens: &[String], start: usize) -> (HashMap<String, String>, usize) {
    let mut attrs = HashMap::new();
    let mut idx = start;
    if tokens.get(idx).map(|s| s.as_str()) != Some("[") {
        return (attrs, start);
    }
    idx += 1;
    while idx < tokens.len() {
        if tokens[idx] == "]" {
            return (attrs, idx);
        }
        let key = clean_id(&tokens[idx]);
        if idx + 2 < tokens.len() && tokens[idx + 1] == "=" {
            let val = clean_id(&tokens[idx + 2]);
            attrs.insert(key, val);
            idx += 3;
            if idx < tokens.len() && (tokens[idx] == "," || tokens[idx] == ";") {
                idx += 1;
            }
        } else {
            idx += 1;
        }
    }
    (attrs, idx)
}

/// Computes a hierarchical DAG layout (layered placement) and renders into a Page IR.
pub fn layout_and_render_graph(graph: &DiagramGraph, _options: &ConvertOptions) -> Result<Page> {
    let node_indices: HashMap<String, usize> = graph
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect();

    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for n in &graph.nodes {
        in_degree.insert(n.id.clone(), 0);
        adjacency.insert(n.id.clone(), Vec::new());
    }
    for e in &graph.edges {
        if node_indices.contains_key(&e.from) && node_indices.contains_key(&e.to) {
            *in_degree.entry(e.to.clone()).or_insert(0) += 1;
            adjacency
                .entry(e.from.clone())
                .or_default()
                .push(e.to.clone());
        }
    }

    let mut layers: Vec<Vec<String>> = Vec::new();
    let mut current_layer: Vec<String> = graph
        .nodes
        .iter()
        .filter(|n| in_degree.get(&n.id).copied().unwrap_or(0) == 0)
        .map(|n| n.id.clone())
        .collect();

    if current_layer.is_empty() && !graph.nodes.is_empty() {
        current_layer.push(graph.nodes[0].id.clone());
    }

    let mut placed: HashSet<String> = current_layer.iter().cloned().collect();
    layers.push(current_layer);

    while placed.len() < graph.nodes.len() {
        let mut next_layer = Vec::new();
        if let Some(prev) = layers.last() {
            for node_id in prev {
                if let Some(neighbors) = adjacency.get(node_id) {
                    for neighbor in neighbors {
                        if !placed.contains(neighbor) {
                            placed.insert(neighbor.clone());
                            next_layer.push(neighbor.clone());
                        }
                    }
                }
            }
        }
        if next_layer.is_empty() {
            for n in &graph.nodes {
                if !placed.contains(&n.id) {
                    placed.insert(n.id.clone());
                    next_layer.push(n.id.clone());
                    break;
                }
            }
        }
        layers.push(next_layer);
    }

    let node_width = 130.0;
    let node_height = 46.0;
    let horizontal_gap = 40.0;
    let vertical_gap = 60.0;
    let padding = 40.0;

    let mut node_positions: HashMap<String, (f64, f64)> = HashMap::new();
    let mut max_layer_width = 0.0f64;

    for layer in &layers {
        let count = layer.len() as f64;
        let layer_width = count * node_width + (count - 1.0).max(0.0) * horizontal_gap;
        if layer_width > max_layer_width {
            max_layer_width = layer_width;
        }
    }

    let total_height = layers.len() as f64 * node_height
        + (layers.len() as f64 - 1.0).max(0.0) * vertical_gap
        + padding * 2.0;
    let total_width = (max_layer_width + padding * 2.0).max(300.0);

    let mut cur_y = padding;
    for layer in &layers {
        let count = layer.len() as f64;
        let layer_width = count * node_width + (count - 1.0).max(0.0) * horizontal_gap;
        let start_x = (total_width - layer_width) / 2.0;

        for (col_idx, node_id) in layer.iter().enumerate() {
            let x = start_x + col_idx as f64 * (node_width + horizontal_gap);
            node_positions.insert(node_id.clone(), (x, cur_y));
        }
        cur_y += node_height + vertical_gap;
    }

    let mut page = Page::new(1, total_width, total_height, "diagram");
    page.embedded_source = Some(graph.raw_source.clone());

    // Draw edges first
    for edge in &graph.edges {
        if let (Some(&(x1, y1)), Some(&(x2, y2))) =
            (node_positions.get(&edge.from), node_positions.get(&edge.to))
        {
            let start_point = (x1 + node_width / 2.0, y1 + node_height);
            let end_point = (x2 + node_width / 2.0, y2);

            let stroke = Stroke {
                paint: Paint::solid("#4b5563"),
                width: 1.5,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            };

            let mid_y = (start_point.1 + end_point.1) / 2.0;
            let d = format!(
                "M {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2}",
                start_point.0,
                start_point.1,
                start_point.0,
                mid_y,
                end_point.0,
                mid_y,
                end_point.0,
                end_point.1 - 4.0
            );

            page.nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: String::new(),
                fill: Paint::None,
                stroke,
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });

            // Arrow head (only for directed graphs)
            if graph.is_directed {
                let arrow_tip = (end_point.0, end_point.1);
                let arrow_d = format!(
                    "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} Z",
                    arrow_tip.0,
                    arrow_tip.1,
                    arrow_tip.0 - 4.0,
                    arrow_tip.1 - 7.0,
                    arrow_tip.0 + 4.0,
                    arrow_tip.1 - 7.0
                );
                page.nodes.push(Node::Path {
                    id: String::new(),
                    d: arrow_d,
                    fill_rule: String::new(),
                    fill: Paint::solid("#4b5563"),
                    stroke: Stroke::default(),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }

            // Edge label if present
            if let Some(ref label) = edge.label {
                let mid_x = (start_point.0 + end_point.0) / 2.0;
                let mid_label_y = mid_y - 4.0;
                page.nodes.push(Node::Text {
                    id: String::new(),
                    x: mid_x,
                    y: mid_label_y,
                    runs: vec![TextRun {
                        text: label.clone(),
                        font_size: 11.0,
                        font_family: "Helvetica, Arial, sans-serif".to_string(),
                        fill: Paint::solid("#374151"),
                        ..Default::default()
                    }],
                    anchor: TextAnchor::Middle,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        }
    }

    // Draw nodes
    for node in &graph.nodes {
        if let Some(&(x, y)) = node_positions.get(&node.id) {
            let fill_paint = Paint::solid("#f3f4f6");
            let border_paint = Stroke {
                paint: Paint::solid("#2563eb"),
                width: 1.5,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Default::default()
            };

            let d = match node.shape {
                NodeShape::Circle => {
                    let cx = x + node_width / 2.0;
                    let cy = y + node_height / 2.0;
                    let r = (node_height / 2.0).min(node_width / 2.0);
                    let c = 0.5522847498 * r;
                    format!(
                        "M {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} Z",
                        cx,
                        cy - r,
                        cx + c,
                        cy - r,
                        cx + r,
                        cy - c,
                        cx + r,
                        cy,
                        cx + r,
                        cy + c,
                        cx + c,
                        cy + r,
                        cx,
                        cy + r,
                        cx - c,
                        cy + r,
                        cx - r,
                        cy + c,
                        cx - r,
                        cy,
                        cx - r,
                        cy - c,
                        cx - c,
                        cy - r,
                        cx,
                        cy - r
                    )
                }
                NodeShape::Rounded => {
                    let r = 8.0f64;
                    format!(
                        "M {:.2},{:.2} L {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} L {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} L {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} L {:.2},{:.2} Q {:.2},{:.2} {:.2},{:.2} Z",
                        x + r,
                        y,
                        x + node_width - r,
                        y,
                        x + node_width,
                        y,
                        x + node_width,
                        y + r,
                        x + node_width,
                        y + node_height - r,
                        x + node_width,
                        y + node_height,
                        x + node_width - r,
                        y + node_height,
                        x + r,
                        y + node_height,
                        x,
                        y + node_height,
                        x,
                        y + node_height - r,
                        x,
                        y + r,
                        x,
                        y,
                        x + r,
                        y
                    )
                }
                NodeShape::Diamond => {
                    let mx = x + node_width / 2.0;
                    let my = y + node_height / 2.0;
                    format!(
                        "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} Z",
                        mx,
                        y,
                        x + node_width,
                        my,
                        mx,
                        y + node_height,
                        x,
                        my
                    )
                }
                NodeShape::Cylinder => {
                    let _rx = node_width / 2.0;
                    let ry = 6.0;
                    let top_cy = y + ry;
                    let bot_cy = y + node_height - ry;
                    format!(
                        "M {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} L {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} C {:.2},{:.2} {:.2},{:.2} {:.2},{:.2} Z",
                        x,
                        top_cy,
                        x,
                        top_cy - ry * 1.33,
                        x + node_width,
                        top_cy - ry * 1.33,
                        x + node_width,
                        top_cy,
                        x + node_width,
                        top_cy + ry * 1.33,
                        x,
                        top_cy + ry * 1.33,
                        x,
                        top_cy,
                        x,
                        bot_cy,
                        x,
                        bot_cy + ry * 1.33,
                        x + node_width,
                        bot_cy + ry * 1.33,
                        x + node_width,
                        bot_cy,
                        x + node_width,
                        bot_cy - ry * 1.33,
                        x,
                        bot_cy - ry * 1.33,
                        x,
                        bot_cy
                    )
                }
                NodeShape::Box => {
                    format!(
                        "M {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} L {:.2},{:.2} Z",
                        x,
                        y,
                        x + node_width,
                        y,
                        x + node_width,
                        y + node_height,
                        x,
                        y + node_height
                    )
                }
            };

            page.nodes.push(Node::Path {
                id: String::new(),
                d,
                fill_rule: String::new(),
                fill: fill_paint,
                stroke: border_paint,
                transform: IDENTITY,
                clip_id: None,
                meta: SourceMeta::default(),
            });

            // Node text label
            let font_size = 13.0;
            let text_x = x + node_width / 2.0;
            let text_y = y + node_height / 2.0 + 4.5;
            page.nodes.push(Node::Text {
                id: String::new(),
                x: text_x,
                y: text_y,
                runs: vec![TextRun {
                    text: node.label.clone(),
                    font_size,
                    font_family: "Helvetica, Arial, sans-serif".to_string(),
                    fill: Paint::solid("#1e293b"),
                    ..Default::default()
                }],
                anchor: TextAnchor::Middle,
                transform: IDENTITY,
                opacity: 1.0,
                stroke: Stroke::default(),
                clip_id: None,
                meta: SourceMeta::default(),
            });
        }
    }

    Ok(page)
}

/// Reverse extraction: restores Graphviz DOT or Mermaid from SVG.
pub fn extract_diagram_from_svg(svg_bytes: &[u8]) -> Result<String> {
    let svg_text = std::str::from_utf8(svg_bytes)
        .map_err(|e| Error::InvalidInput(format!("SVG is not valid UTF-8: {e}")))?;

    // 1. Check content attribute
    if let Some(decoded) = crate::cad::svg_reader::extract_embedded_source(svg_bytes)
        && !decoded.trim().is_empty()
    {
        return Ok(decoded);
    }

    // 2. Geometric fallback
    let mut extracted_texts = Vec::new();
    let mut reader = Reader::from_str(svg_text);
    reader.config_mut().trim_text(true);

    let mut in_text = false;
    let mut current_text = String::new();

    while let Ok(event) = reader.read_event() {
        match event {
            Event::Start(e) if e.name().as_ref() == b"text" => {
                in_text = true;
                current_text.clear();
            }
            Event::Text(e) if in_text => {
                let bytes = e.as_ref();
                if let Ok(s) = std::str::from_utf8(bytes) {
                    current_text.push_str(s);
                }
            }
            Event::End(e) if e.name().as_ref() == b"text" => {
                in_text = false;
                let trimmed = current_text.trim();
                if !trimmed.is_empty() {
                    extracted_texts.push(trimmed.to_string());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    if extracted_texts.is_empty() {
        return Ok("digraph G {\n}\n".to_string());
    }

    let mut dot = String::from("digraph G {\n  node [shape=box, style=rounded];\n");
    for (i, t) in extracted_texts.iter().enumerate() {
        dot.push_str(&format!("  n{i} [label=\"{t}\"];\n"));
    }
    for i in 0..extracted_texts.len().saturating_sub(1) {
        dot.push_str(&format!("  n{i} -> n{};\n", i + 1));
    }
    dot.push_str("}\n");

    Ok(dot)
}

#[derive(Clone, Debug)]
pub struct SequenceMessage {
    pub from: String,
    pub to: String,
    pub text: String,
    pub is_dotted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SequenceParticipant {
    pub id: String,
    pub label: String,
    pub is_actor: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SequenceDiagram {
    pub participants: Vec<String>,
    pub participant_info: Vec<SequenceParticipant>,
    pub messages: Vec<SequenceMessage>,
    pub has_autonumber: bool,
    pub raw_source: String,
}

pub fn parse_sequence_diagram(source: &str) -> Result<SequenceDiagram> {
    let mut participants = Vec::new();
    let mut participant_info = Vec::new();
    let mut alias_map: HashMap<String, String> = HashMap::new();
    let mut messages = Vec::new();
    let mut has_autonumber = false;

    let add_participant = |id: &str,
                           label: &str,
                           is_actor: bool,
                           parts: &mut Vec<String>,
                           info: &mut Vec<SequenceParticipant>| {
        let id_clean = id.trim().trim_matches('"').to_string();
        let label_clean = label.trim().trim_matches('"').to_string();
        if id_clean.is_empty() {
            return;
        }
        let display = if label_clean.is_empty() {
            id_clean.clone()
        } else {
            label_clean
        };
        if !parts.contains(&display) {
            parts.push(display.clone());
            info.push(SequenceParticipant {
                id: id_clean,
                label: display,
                is_actor,
            });
        }
    };

    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("%%") || line.starts_with("sequenceDiagram") {
            continue;
        }

        if line == "autonumber" || line.starts_with("autonumber ") {
            has_autonumber = true;
            continue;
        }

        if let Some(rest) = line
            .strip_prefix("actor ")
            .or_else(|| line.strip_prefix("participant "))
        {
            let is_actor = line.starts_with("actor ");
            let rest = rest.trim();
            let (id, label) = if let Some(pos) = rest.find(" as ") {
                (rest[..pos].trim(), rest[pos + 4..].trim())
            } else {
                (rest, rest)
            };
            let id_clean = id.trim().trim_matches('"').to_string();
            let label_clean = label.trim().trim_matches('"').to_string();
            alias_map.insert(id_clean.clone(), label_clean.clone());
            add_participant(
                &id_clean,
                &label_clean,
                is_actor,
                &mut participants,
                &mut participant_info,
            );
            continue;
        }

        // Check arrow syntax: A->>B: text or A-->>B: text or A->B or A-->B
        let arrow_info = if let Some(pos) = line.find("-->>") {
            Some(("-->>", true, pos))
        } else if let Some(pos) = line.find("->>") {
            Some(("->>", false, pos))
        } else if let Some(pos) = line.find("-->") {
            Some(("-->", true, pos))
        } else {
            line.find("->").map(|pos| ("->", false, pos))
        };

        if let Some((arrow, is_dotted, arrow_pos)) = arrow_info {
            let from_part = line[..arrow_pos].trim();
            let rest = &line[arrow_pos + arrow.len()..];

            let (to_part, text_part) = if let Some(colon_pos) = rest.find(':') {
                (rest[..colon_pos].trim(), rest[colon_pos + 1..].trim())
            } else {
                (rest.trim(), "")
            };

            let from_resolved = alias_map
                .get(from_part)
                .cloned()
                .unwrap_or_else(|| from_part.to_string());
            let to_resolved = alias_map
                .get(to_part)
                .cloned()
                .unwrap_or_else(|| to_part.to_string());

            add_participant(
                from_part,
                &from_resolved,
                false,
                &mut participants,
                &mut participant_info,
            );
            add_participant(
                to_part,
                &to_resolved,
                false,
                &mut participants,
                &mut participant_info,
            );

            messages.push(SequenceMessage {
                from: from_resolved,
                to: to_resolved,
                text: text_part.to_string(),
                is_dotted,
            });
        }
    }

    Ok(SequenceDiagram {
        participants,
        participant_info,
        messages,
        has_autonumber,
        raw_source: source.to_string(),
    })
}

pub fn layout_and_render_sequence(
    seq: &SequenceDiagram,
    _options: &ConvertOptions,
) -> Result<Page> {
    let participants: Vec<SequenceParticipant> = if !seq.participant_info.is_empty() {
        seq.participant_info.clone()
    } else {
        seq.participants
            .iter()
            .map(|p| SequenceParticipant {
                id: p.clone(),
                label: p.clone(),
                is_actor: false,
            })
            .collect()
    };

    let actor_gap = 56.0;
    let margin_x = 48.0;
    let margin_top = 28.0;
    let margin_bottom = 28.0;
    let header_height = 64.0;
    let footer_height = 64.0;
    let message_gap = 48.0;

    // Calculate dynamic widths based on label lengths
    let widths: Vec<f64> = participants
        .iter()
        .map(|p| {
            let char_len = p.label.chars().count();
            (char_len as f64 * 7.5 + 32.0).max(104.0)
        })
        .collect();

    let mut participant_centers: HashMap<String, f64> = HashMap::new();
    let mut cur_x = margin_x;
    for (p, &w) in participants.iter().zip(widths.iter()) {
        let center_x = cur_x + w / 2.0;
        participant_centers.insert(p.label.clone(), center_x);
        participant_centers.insert(p.id.clone(), center_x);
        cur_x += w + actor_gap;
    }

    let total_width = (cur_x - actor_gap + margin_x).max(360.0);
    let messages_height = (seq.messages.len().max(1) as f64 + 1.0) * message_gap;
    let total_height = margin_top + header_height + messages_height + footer_height + margin_bottom;

    let mut page = Page::new(1, total_width, total_height, "sequence_diagram");
    page.embedded_source = Some(seq.raw_source.clone());

    let lifeline_top = margin_top + header_height;
    let lifeline_bottom = total_height - margin_bottom - footer_height;

    // 1. Draw participants (Top and Bottom) + Lifelines
    for (p, &w) in participants.iter().zip(widths.iter()) {
        let center_x = *participant_centers.get(&p.label).unwrap();

        // Lifeline (dashed vertical line)
        let lifeline_d = format!(
            "M {:.1},{:.1} L {:.1},{:.1}",
            center_x, lifeline_top, center_x, lifeline_bottom
        );
        page.nodes.push(Node::Path {
            id: String::new(),
            d: lifeline_d,
            fill_rule: String::new(),
            fill: Paint::None,
            stroke: Stroke {
                paint: Paint::solid("#94a3b8"),
                width: 1.5,
                dash_array: vec![4.0, 4.0],
                ..Default::default()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta::default(),
        });

        // Helper to draw participant at top or bottom
        let draw_participant = |is_top: bool, page_nodes: &mut Vec<Node>| {
            let base_y = if is_top { margin_top } else { lifeline_bottom };

            if p.is_actor {
                // Draw stick figure
                let head_cy = base_y + 11.0;
                let head_d = format!(
                    "M {:.1},{:.1} a 6.5 6.5 0 1 0 13 0 a 6.5 6.5 0 1 0 -13 0 Z",
                    center_x - 6.5,
                    head_cy
                );
                page_nodes.push(Node::Path {
                    id: String::new(),
                    d: head_d,
                    fill_rule: String::new(),
                    fill: Paint::solid("#f1f5f9"),
                    stroke: Stroke {
                        paint: Paint::solid("#2563eb"),
                        width: 1.6,
                        ..Default::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });

                // Body: torso, arms, legs
                let neck_y = base_y + 17.5;
                let waist_y = base_y + 29.0;
                let arms_y = base_y + 21.0;
                let feet_y = base_y + 42.0;

                let body_d = format!(
                    "M {:.1},{:.1} L {:.1},{:.1} M {:.1},{:.1} L {:.1},{:.1} M {:.1},{:.1} L {:.1},{:.1} M {:.1},{:.1} L {:.1},{:.1}",
                    center_x,
                    neck_y,
                    center_x,
                    waist_y,
                    center_x - 12.0,
                    arms_y,
                    center_x + 12.0,
                    arms_y,
                    center_x,
                    waist_y,
                    center_x - 9.0,
                    feet_y,
                    center_x,
                    waist_y,
                    center_x + 9.0,
                    feet_y
                );
                page_nodes.push(Node::Path {
                    id: String::new(),
                    d: body_d,
                    fill_rule: String::new(),
                    fill: Paint::None,
                    stroke: Stroke {
                        paint: Paint::solid("#2563eb"),
                        width: 1.6,
                        ..Default::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });

                // Text under actor
                page_nodes.push(Node::Text {
                    id: String::new(),
                    x: center_x,
                    y: base_y + 56.0,
                    runs: vec![TextRun {
                        text: p.label.clone(),
                        font_size: 12.0,
                        font_family: "Helvetica, Arial, sans-serif".to_string(),
                        bold: true,
                        fill: Paint::solid("#0f172a"),
                        ..Default::default()
                    }],
                    anchor: TextAnchor::Middle,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            } else {
                // Rounded participant box
                let box_h = 36.0;
                let box_x = center_x - w / 2.0;
                let box_y = base_y + (header_height - box_h) / 2.0;
                let box_d = format!(
                    "M {:.1},{:.1} h {:.1} a 4 4 0 0 1 4 4 v {:.1} a 4 4 0 0 1 -4 4 h -{:.1} a 4 4 0 0 1 -4 -4 v -{:.1} a 4 4 0 0 1 4 -4 Z",
                    box_x + 4.0,
                    box_y,
                    w - 8.0,
                    box_h - 8.0,
                    w - 8.0,
                    box_h - 8.0
                );
                page_nodes.push(Node::Path {
                    id: String::new(),
                    d: box_d,
                    fill_rule: String::new(),
                    fill: Paint::solid("#f1f5f9"),
                    stroke: Stroke {
                        paint: Paint::solid("#2563eb"),
                        width: 1.5,
                        ..Default::default()
                    },
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });

                // Text centered in box
                page_nodes.push(Node::Text {
                    id: String::new(),
                    x: center_x,
                    y: box_y + box_h / 2.0 + 4.0,
                    runs: vec![TextRun {
                        text: p.label.clone(),
                        font_size: 12.0,
                        font_family: "Helvetica, Arial, sans-serif".to_string(),
                        bold: true,
                        fill: Paint::solid("#0f172a"),
                        ..Default::default()
                    }],
                    anchor: TextAnchor::Middle,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }
        };

        draw_participant(true, &mut page.nodes);
        draw_participant(false, &mut page.nodes);
    }

    // 2. Draw Messages
    let mut cur_msg_y = lifeline_top + message_gap;
    for (msg_idx, msg) in seq.messages.iter().enumerate() {
        if let (Some(&x1), Some(&x2)) = (
            participant_centers.get(&msg.from),
            participant_centers.get(&msg.to),
        ) {
            let display_text = if seq.has_autonumber {
                format!("{}: {}", msg_idx + 1, msg.text)
            } else {
                msg.text.clone()
            };

            let is_self = (x1 - x2).abs() < 1.0;
            if is_self {
                // Self-loopback arrow
                let loop_w = 36.0;
                let loop_h = 22.0;
                let arrow_end_x = x1 + 2.0;
                let arrow_end_y = cur_msg_y + loop_h;

                let loop_d = format!(
                    "M {:.1},{:.1} h {:.1} v {:.1} L {:.1},{:.1}",
                    x1, cur_msg_y, loop_w, loop_h, arrow_end_x, arrow_end_y
                );
                let mut stroke = Stroke {
                    paint: Paint::solid("#334155"),
                    width: 1.5,
                    ..Default::default()
                };
                if msg.is_dotted {
                    stroke.dash_array = vec![4.0, 3.0];
                }
                page.nodes.push(Node::Path {
                    id: String::new(),
                    d: loop_d,
                    fill_rule: String::new(),
                    fill: Paint::None,
                    stroke,
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });

                // Arrow head pointing left
                let head_d = format!(
                    "M {:.1},{:.1} L {:.1},{:.1} L {:.1},{:.1} Z",
                    arrow_end_x,
                    arrow_end_y,
                    arrow_end_x + 6.0,
                    arrow_end_y - 3.5,
                    arrow_end_x + 6.0,
                    arrow_end_y + 3.5
                );
                page.nodes.push(Node::Path {
                    id: String::new(),
                    d: head_d,
                    fill_rule: String::new(),
                    fill: Paint::solid("#334155"),
                    stroke: Stroke::default(),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });

                // Text to the right of the lifeline
                page.nodes.push(Node::Text {
                    id: String::new(),
                    x: x1 + 8.0,
                    y: cur_msg_y - 5.0,
                    runs: vec![TextRun {
                        text: display_text,
                        font_size: 11.0,
                        font_family: "Helvetica, Arial, sans-serif".to_string(),
                        fill: Paint::solid("#1e293b"),
                        ..Default::default()
                    }],
                    anchor: TextAnchor::Start,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            } else {
                let is_forward = x2 >= x1;
                let arrow_end_x = if is_forward { x2 - 2.0 } else { x2 + 2.0 };

                // Message Arrow Line
                let arrow_line_d = format!(
                    "M {:.1},{:.1} L {:.1},{:.1}",
                    x1, cur_msg_y, arrow_end_x, cur_msg_y
                );
                let mut stroke = Stroke {
                    paint: Paint::solid("#334155"),
                    width: 1.5,
                    ..Default::default()
                };
                if msg.is_dotted {
                    stroke.dash_array = vec![4.0, 3.0];
                }
                page.nodes.push(Node::Path {
                    id: String::new(),
                    d: arrow_line_d,
                    fill_rule: String::new(),
                    fill: Paint::None,
                    stroke,
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });

                // Arrow head
                let head_d = if is_forward {
                    format!(
                        "M {:.1},{:.1} L {:.1},{:.1} L {:.1},{:.1} Z",
                        arrow_end_x,
                        cur_msg_y,
                        arrow_end_x - 6.0,
                        cur_msg_y - 3.5,
                        arrow_end_x - 6.0,
                        cur_msg_y + 3.5
                    )
                } else {
                    format!(
                        "M {:.1},{:.1} L {:.1},{:.1} L {:.1},{:.1} Z",
                        arrow_end_x,
                        cur_msg_y,
                        arrow_end_x + 6.0,
                        cur_msg_y - 3.5,
                        arrow_end_x + 6.0,
                        cur_msg_y + 3.5
                    )
                };
                page.nodes.push(Node::Path {
                    id: String::new(),
                    d: head_d,
                    fill_rule: String::new(),
                    fill: Paint::solid("#334155"),
                    stroke: Stroke::default(),
                    transform: IDENTITY,
                    clip_id: None,
                    meta: SourceMeta::default(),
                });

                // Message text label above line
                let mid_x = (x1 + x2) / 2.0;
                page.nodes.push(Node::Text {
                    id: String::new(),
                    x: mid_x,
                    y: cur_msg_y - 6.0,
                    runs: vec![TextRun {
                        text: display_text,
                        font_size: 11.0,
                        font_family: "Helvetica, Arial, sans-serif".to_string(),
                        fill: Paint::solid("#1e293b"),
                        ..Default::default()
                    }],
                    anchor: TextAnchor::Middle,
                    transform: IDENTITY,
                    opacity: 1.0,
                    stroke: Stroke::default(),
                    clip_id: None,
                    meta: SourceMeta::default(),
                });
            }

            cur_msg_y += message_gap;
        }
    }

    Ok(page)
}
