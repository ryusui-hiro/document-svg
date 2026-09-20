//! D2 declarative diagram language parser and SVG vector renderer.
//!
//! Supports D2 connection syntax (`x -> y: label`), multi-hop edges (`a -> b -> c`),
//! and custom node shapes/labels.

use std::collections::HashSet;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::diagram::{DiagramEdge, DiagramGraph, DiagramNode, NodeShape, layout_and_render_graph};
use crate::error::{Error, Result};

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(path, options.max_input_bytes, "D2 input")?;
    let source = String::from_utf8(bytes)
        .map_err(|e| Error::InvalidInput(format!("D2 file is not valid UTF-8: {e}")))?;

    let graph = parse_d2(&source)?;
    let page = layout_and_render_graph(&graph, options)?;
    sink.consume(page)?;
    Ok(Vec::new())
}

pub fn parse_d2(source: &str) -> Result<DiagramGraph> {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut node_set = HashSet::new();

    let add_node_if_missing = |id: &str,
                               label: Option<String>,
                               shape: NodeShape,
                               nodes: &mut Vec<DiagramNode>,
                               node_set: &mut HashSet<String>| {
        let clean_id = id.trim().trim_matches('"').to_string();
        if !clean_id.is_empty() && !node_set.contains(&clean_id) {
            node_set.insert(clean_id.clone());
            let final_label = label.unwrap_or_else(|| clean_id.clone());
            nodes.push(DiagramNode {
                id: clean_id,
                label: final_label,
                shape,
                x: 0.0,
                y: 0.0,
                width: 140.0,
                height: 50.0,
            });
        }
    };

    let mut container_stack: Vec<String> = Vec::new();
    let mut diagram_title: Option<String> = None;
    // Depth of nested `{`..`}` blocks under a reserved top-level keyword (vars,
    // classes, theme, layers, scenarios) whose contents are D2 configuration,
    // not diagram nodes. `id: {` inside `vars: { d2-config: { ... } }` would
    // otherwise be parsed like any other container and drawn as a box.
    let mut ignore_depth: usize = 0;

    for line in source.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with("//") || line.is_empty() || line == "{" {
            continue;
        }

        if ignore_depth > 0 {
            if line == "}" {
                ignore_depth -= 1;
            } else if line.ends_with('{') {
                ignore_depth += 1;
            }
            continue;
        }

        if line == "}" {
            container_stack.pop();
            continue;
        }

        // Check for block open: id: Label { or id {
        if line.ends_with('{') {
            let trimmed_brace = line.trim_end_matches('{').trim();
            let id = if let Some(colon_pos) = trimmed_brace.find(':') {
                trimmed_brace[..colon_pos].trim().trim_matches('"')
            } else {
                trimmed_brace.trim_matches('"')
            };
            if is_reserved_d2_keyword(id) {
                ignore_depth = 1;
                continue;
            }
            if let Some(colon_pos) = trimmed_brace.find(':') {
                let label = trimmed_brace[colon_pos + 1..].trim().trim_matches('"');
                if !id.is_empty() {
                    let lbl = if label.is_empty() {
                        None
                    } else {
                        Some(label.to_string())
                    };
                    add_node_if_missing(id, lbl, NodeShape::Rounded, &mut nodes, &mut node_set);
                    container_stack.push(id.to_string());
                }
            } else if !trimmed_brace.is_empty() {
                add_node_if_missing(id, None, NodeShape::Rounded, &mut nodes, &mut node_set);
                container_stack.push(id.to_string());
            }
            continue;
        }

        // Check for edge: <->, ->, <-, or --
        let arrow_delim = if line.contains("<->") {
            Some("<->")
        } else if line.contains("->") {
            Some("->")
        } else if line.contains("<-") {
            Some("<-")
        } else if line.contains("--") {
            Some("--")
        } else {
            None
        };

        if let Some(delim) = arrow_delim {
            let (edge_part, label_part) = if let Some(colon_pos) = line.find(':') {
                (
                    &line[..colon_pos],
                    Some(line[colon_pos + 1..].trim().trim_matches('"').to_string()),
                )
            } else {
                (line, None)
            };

            let segments: Vec<&str> = edge_part.split(delim).map(str::trim).collect();
            for i in 0..segments.len().saturating_sub(1) {
                let seg_a = segments[i].trim_matches('"');
                let seg_b = segments[i + 1].trim_matches('"');
                if !seg_a.is_empty() && !seg_b.is_empty() {
                    add_node_if_missing(seg_a, None, NodeShape::Rounded, &mut nodes, &mut node_set);
                    add_node_if_missing(seg_b, None, NodeShape::Rounded, &mut nodes, &mut node_set);

                    let (from, to) = if delim == "<-" {
                        (seg_b, seg_a)
                    } else {
                        (seg_a, seg_b)
                    };

                    edges.push(DiagramEdge {
                        from: from.to_string(),
                        to: to.to_string(),
                        label: if i == 0 { label_part.clone() } else { None },
                    });

                    if delim == "<->" {
                        edges.push(DiagramEdge {
                            from: to.to_string(),
                            to: from.to_string(),
                            label: None,
                        });
                    }
                }
            }
            continue;
        }

        // Node declaration or property: id: Label or id.shape: cylinder
        if let Some(colon_pos) = line.find(':') {
            let left = line[..colon_pos].trim();
            let right = line[colon_pos + 1..].trim().trim_matches('"').trim();

            if left == "title" {
                diagram_title = Some(right.to_string());
                continue;
            }

            if left == "direction"
                || left.starts_with("grid-")
                || left == "vars"
                || left == "classes"
                || left == "theme"
                || left == "layers"
                || left == "scenarios"
            {
                continue;
            }

            if left == "shape" {
                let shape = parse_d2_shape(right);
                if let Some(parent_id) = container_stack.last()
                    && let Some(n) = nodes.iter_mut().find(|n| &n.id == parent_id)
                {
                    n.shape = shape;
                }
                continue;
            } else if left == "label" {
                if let Some(parent_id) = container_stack.last()
                    && let Some(n) = nodes.iter_mut().find(|n| &n.id == parent_id)
                {
                    n.label = right.to_string();
                }
                continue;
            } else if left.starts_with("style")
                || left == "icon"
                || left == "link"
                || left == "near"
                || left == "tooltip"
            {
                continue;
            }

            if left.contains(".shape") {
                let id = left.replace(".shape", "").trim().to_string();
                let shape = parse_d2_shape(right);
                if let Some(n) = nodes.iter_mut().find(|n| n.id == id) {
                    n.shape = shape;
                } else {
                    add_node_if_missing(&id, None, shape, &mut nodes, &mut node_set);
                }
            } else if right.starts_with('{') && right.ends_with('}') && right.len() >= 2 {
                // Compact inline attributes: `id: {shape: hexagon; style.fill: red}`.
                // Pull out the shape/label keys instead of using the raw `{...}`
                // text as the node's visible label.
                let inner = &right[1..right.len() - 1];
                let mut shape = None;
                let mut label = None;
                for part in inner.split(';') {
                    let part = part.trim();
                    if let Some(value) = part.strip_prefix("shape:") {
                        shape = Some(parse_d2_shape(value.trim()));
                    } else if let Some(value) = part.strip_prefix("label:") {
                        label = Some(value.trim().trim_matches('"').to_string());
                    }
                }
                let id = left.trim_matches('"').to_string();
                if let Some(n) = nodes.iter_mut().find(|n| n.id == id) {
                    if let Some(shape) = shape {
                        n.shape = shape;
                    }
                    if let Some(label) = label {
                        n.label = label;
                    }
                } else {
                    add_node_if_missing(
                        &id,
                        label,
                        shape.unwrap_or(NodeShape::Rounded),
                        &mut nodes,
                        &mut node_set,
                    );
                }
            } else if !left.contains('{') && !left.contains('}') {
                let id = left.trim_matches('"').to_string();
                if let Some(n) = nodes.iter_mut().find(|n| n.id == id) {
                    n.label = right.to_string();
                } else {
                    add_node_if_missing(
                        &id,
                        Some(right.to_string()),
                        NodeShape::Rounded,
                        &mut nodes,
                        &mut node_set,
                    );
                }
            }
        }
    }

    if nodes.is_empty() {
        return Err(Error::InvalidInput(
            "no nodes or connections found in D2 file".into(),
        ));
    }

    Ok(DiagramGraph {
        title: diagram_title,
        is_directed: true,
        nodes,
        edges,
        raw_source: source.to_string(),
    })
}

/// True for D2's reserved top-level block keywords (`vars: { ... }`,
/// `classes: { ... }`, etc.). Their bodies are configuration, not diagram
/// content, and must never be parsed into nodes even when nested (e.g. a
/// `d2-config` block inside `vars`).
fn is_reserved_d2_keyword(id: &str) -> bool {
    matches!(id, "vars" | "classes" | "theme" | "layers" | "scenarios") || id.starts_with("grid-")
}

fn parse_d2_shape(s: &str) -> NodeShape {
    match s.to_ascii_lowercase().as_str() {
        "cylinder" | "sql_table" | "queue" => NodeShape::Cylinder,
        "diamond" => NodeShape::Diamond,
        "circle" | "oval" => NodeShape::Circle,
        "rectangle" | "square" | "page" | "parallelogram" | "document" | "step" | "callout" => {
            NodeShape::Box
        }
        _ => NodeShape::Rounded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node<'a>(graph: &'a DiagramGraph, id: &str) -> Option<&'a DiagramNode> {
        graph.nodes.iter().find(|n| n.id == id)
    }

    /// `vars: { ... }` is D2's reserved configuration block (variables,
    /// theme overrides, layout engine choice, etc.), not diagram content.
    /// A prior version of this parser only recognized `vars` on a bare
    /// `vars: value` line, so the block-opening form `vars: {` fell through
    /// to the generic container-node path and drew "vars", every key nested
    /// inside it (e.g. `d2-config`), and their values as visible boxes.
    #[test]
    fn reserved_vars_block_and_nested_keys_are_not_drawn_as_nodes() {
        let source = "vars: {\n  d2-config: {\n    layout-engine: elk\n  }\n}\na -> b\n";
        let graph = parse_d2(source).unwrap();
        for id in ["vars", "d2-config", "layout-engine", "elk"] {
            assert!(
                node(&graph, id).is_none(),
                "{id:?} should not become a node"
            );
        }
        assert!(node(&graph, "a").is_some());
        assert!(node(&graph, "b").is_some());
    }

    /// Compact inline attributes (`id: {shape: hexagon}`) are common in
    /// real-world D2 (e.g. terrastruct/d2's own docs). The generic
    /// `id: value` case used to take the whole `{shape: hexagon}` string
    /// literally as the node's label instead of parsing the shape out of it.
    #[test]
    fn inline_brace_shape_is_parsed_not_shown_as_literal_text() {
        let graph = parse_d2("ui: {shape: cylinder}\n").unwrap();
        let ui = node(&graph, "ui").expect("ui node");
        assert_eq!(ui.shape, NodeShape::Cylinder);
        assert_eq!(ui.label, "ui");
    }

    #[test]
    fn inline_brace_label_and_shape_are_both_applied() {
        let graph = parse_d2("db: {shape: cylinder; label: \"Database\"}\n").unwrap();
        let db = node(&graph, "db").expect("db node");
        assert_eq!(db.shape, NodeShape::Cylinder);
        assert_eq!(db.label, "Database");
    }
}
