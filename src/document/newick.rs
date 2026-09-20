//! Bounded Newick phylogenetic-tree preview.
//!
//! Parentheses and comma topology are converted into the shared diagram graph
//! renderer. Node labels and branch lengths remain inert display text; no
//! sequence lookup, tree rooting, distance calculation, or external resource
//! access is performed.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::diagram::{DiagramEdge, DiagramGraph, DiagramNode, NodeShape, layout_and_render_graph};
use crate::error::{Error, Result};

const MAX_NEWICK_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NEWICK_NODES: usize = 100_000;
const MAX_NEWICK_DEPTH: usize = 128;
const MAX_NEWICK_LABEL_BYTES: usize = 4 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let trimmed = text.trim_start();
    trimmed.starts_with('(') && trimmed.contains(';')
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_NEWICK_BYTES),
        "Newick input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Newick input must be UTF-8/ASCII: {error}"))
    })?;
    let (graph, warnings) = parse_newick(&text)?;
    let mut page = layout_and_render_graph(&graph, options)?;
    page.source_format = "newick".into();
    page.title = "Newick phylogenetic tree".into();
    for warning in &warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(warnings)
}

#[derive(Clone, Debug)]
struct NodeRef {
    id: String,
    branch: Option<String>,
}

struct Parser<'a> {
    chars: Vec<char>,
    position: usize,
    depth: usize,
    next_id: usize,
    nodes: Vec<DiagramNode>,
    edges: Vec<DiagramEdge>,
    warnings: Vec<String>,
    _source: &'a str,
}

pub(crate) fn parse_newick(source: &str) -> Result<(DiagramGraph, Vec<String>)> {
    let mut parser = Parser {
        chars: source.chars().collect(),
        position: 0,
        depth: 0,
        next_id: 0,
        nodes: Vec::new(),
        edges: Vec::new(),
        warnings: Vec::new(),
        _source: source,
    };
    parser.skip_space_comments()?;
    let _root = parser.parse_subtree()?;
    parser.skip_space_comments()?;
    if parser.chars.get(parser.position) != Some(&';') {
        return Err(Error::InvalidInput("Newick tree must end with ';'".into()));
    }
    parser.position += 1;
    parser.skip_space_comments()?;
    if parser.position < parser.chars.len() {
        parser
            .warnings
            .push("additional Newick trees after the first ';' were ignored".into());
    }
    if parser.nodes.is_empty() {
        return Err(Error::InvalidInput("Newick contains no nodes".into()));
    }
    let graph = DiagramGraph {
        title: Some("Newick phylogenetic tree".into()),
        is_directed: true,
        nodes: parser.nodes,
        edges: parser.edges,
        raw_source: String::new(),
    };
    Ok((graph, dedup_warnings(parser.warnings)))
}

impl Parser<'_> {
    fn parse_subtree(&mut self) -> Result<NodeRef> {
        self.skip_space_comments()?;
        let node = if self.chars.get(self.position) == Some(&'(') {
            self.position += 1;
            self.depth += 1;
            if self.depth > MAX_NEWICK_DEPTH {
                return Err(Error::LimitExceeded(format!(
                    "Newick nesting exceeds {MAX_NEWICK_DEPTH}"
                )));
            }
            let mut children = Vec::new();
            loop {
                self.skip_space_comments()?;
                if matches!(self.chars.get(self.position), Some(')') | Some(',')) {
                    return Err(Error::InvalidInput(
                        "Newick internal node contains an empty child".into(),
                    ));
                }
                children.push(self.parse_subtree()?);
                self.skip_space_comments()?;
                match self.chars.get(self.position) {
                    Some(',') => {
                        self.position += 1;
                    }
                    Some(')') => {
                        self.position += 1;
                        break;
                    }
                    _ => {
                        return Err(Error::InvalidInput(
                            "Newick internal node is missing ',' or ')'".into(),
                        ));
                    }
                }
            }
            self.depth = self.depth.saturating_sub(1);
            let label = self.parse_label()?.unwrap_or_default();
            let branch = self.parse_branch_length()?;
            let id = self.add_node(
                if label.is_empty() {
                    format!("internal-{}", self.next_id)
                } else {
                    label.clone()
                },
                NodeShape::Rounded,
            )?;
            for child in children {
                self.edges.push(DiagramEdge {
                    from: id.clone(),
                    to: child.id,
                    label: child.branch,
                });
            }
            NodeRef { id, branch }
        } else {
            let label = self.parse_label()?.unwrap_or_default();
            if label.is_empty() {
                return Err(Error::InvalidInput("Newick leaf label is missing".into()));
            }
            let branch = self.parse_branch_length()?;
            let id = self.add_node(label, NodeShape::Circle)?;
            NodeRef { id, branch }
        };
        Ok(node)
    }

    fn parse_label(&mut self) -> Result<Option<String>> {
        self.skip_space_comments()?;
        let Some(&character) = self.chars.get(self.position) else {
            return Ok(None);
        };
        if matches!(character, ':' | ',' | ')' | ';') {
            return Ok(None);
        }
        let label = if character == '\'' {
            self.position += 1;
            let mut value = String::new();
            loop {
                let Some(next) = self.chars.get(self.position).copied() else {
                    return Err(Error::InvalidInput(
                        "Newick quoted label is unterminated".into(),
                    ));
                };
                self.position += 1;
                if next == '\'' {
                    if self.chars.get(self.position) == Some(&'\'') {
                        value.push('\'');
                        self.position += 1;
                    } else {
                        break;
                    }
                } else {
                    value.push(next);
                }
            }
            value
        } else {
            let start = self.position;
            while let Some(next) = self.chars.get(self.position).copied() {
                if next.is_whitespace() || matches!(next, ':' | ',' | ')' | ';' | '[' | ']') {
                    break;
                }
                self.position += 1;
            }
            self.chars[start..self.position].iter().collect()
        };
        if label.len() > MAX_NEWICK_LABEL_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Newick label exceeds {MAX_NEWICK_LABEL_BYTES} bytes"
            )));
        }
        Ok(Some(label))
    }

    fn parse_branch_length(&mut self) -> Result<Option<String>> {
        self.skip_space_comments()?;
        if self.chars.get(self.position) != Some(&':') {
            return Ok(None);
        }
        self.position += 1;
        let start = self.position;
        while let Some(next) = self.chars.get(self.position).copied() {
            if next.is_whitespace() || matches!(next, ',' | ')' | ';' | '[' | ']') {
                break;
            }
            self.position += 1;
        }
        let value: String = self.chars[start..self.position].iter().collect();
        let number = value
            .parse::<f64>()
            .map_err(|_| Error::InvalidInput("Newick branch length is invalid".into()))?;
        if !number.is_finite() {
            return Err(Error::InvalidInput(
                "Newick branch length is non-finite".into(),
            ));
        }
        Ok(Some(value))
    }

    fn add_node(&mut self, label: String, shape: NodeShape) -> Result<String> {
        if self.nodes.len() >= MAX_NEWICK_NODES {
            return Err(Error::LimitExceeded(format!(
                "Newick exceeds {MAX_NEWICK_NODES} nodes"
            )));
        }
        self.next_id += 1;
        let id = format!("newick-node-{}", self.next_id);
        self.nodes.push(DiagramNode {
            id: id.clone(),
            label,
            shape,
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
        Ok(id)
    }

    fn skip_space_comments(&mut self) -> Result<()> {
        loop {
            while self
                .chars
                .get(self.position)
                .is_some_and(|character| character.is_whitespace())
            {
                self.position += 1;
            }
            if self.chars.get(self.position) != Some(&'[') {
                return Ok(());
            }
            let mut depth = 0usize;
            while let Some(character) = self.chars.get(self.position).copied() {
                self.position += 1;
                match character {
                    '[' => {
                        depth += 1;
                        if depth > MAX_NEWICK_DEPTH {
                            return Err(Error::LimitExceeded(
                                "Newick comment nesting exceeds configured limit".into(),
                            ));
                        }
                    }
                    ']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if depth != 0 {
                return Err(Error::InvalidInput("Newick comment is unterminated".into()));
            }
        }
    }
}

fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
