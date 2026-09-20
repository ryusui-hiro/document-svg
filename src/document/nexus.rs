//! Bounded NEXUS phylogenetic-tree preview.
//!
//! NEXUS is a block-oriented container. This reader extracts the first
//! `TREE`/`UTREE` statement from a `BEGIN TREES` block and delegates topology
//! parsing to the bounded Newick reader. Taxa, translate maps, and other
//! blocks are retained only as inert diagnostics.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::diagram::layout_and_render_graph;
use crate::document::newick::parse_newick;
use crate::error::{Error, Result};

const MAX_NEXUS_BYTES: u64 = 128 * 1024 * 1024;
const MAX_NEXUS_LINES: usize = 2_000_000;
const MAX_NEXUS_LINE_BYTES: usize = 1 << 20;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.to_ascii_uppercase().starts_with("#NEXUS"))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_NEXUS_BYTES),
        "NEXUS input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("NEXUS input must be UTF-8/ASCII: {error}"))
    })?;
    let (newick, mut warnings) = extract_first_tree(&text)?;
    let (graph, newick_warnings) = parse_newick(&newick)?;
    warnings.extend(newick_warnings);
    let mut page = layout_and_render_graph(&graph, options)?;
    page.source_format = "nexus".into();
    page.title = "NEXUS phylogenetic tree".into();
    page.description =
        "The first NEXUS TREE statement is rendered inertly; taxa and translation maps are not resolved".into();
    for warning in &warnings {
        page.warn(warning.clone());
    }
    sink.consume(page)?;
    Ok(dedup_warnings(warnings))
}

fn extract_first_tree(source: &str) -> Result<(String, Vec<String>)> {
    let lines = source.lines().collect::<Vec<_>>();
    if lines.len() > MAX_NEXUS_LINES {
        return Err(Error::LimitExceeded(format!(
            "NEXUS input exceeds {MAX_NEXUS_LINES} lines"
        )));
    }
    for line in &lines {
        if line.len() > MAX_NEXUS_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "NEXUS line exceeds {MAX_NEXUS_LINE_BYTES} bytes"
            )));
        }
    }
    let first = lines
        .iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
        .ok_or_else(|| Error::InvalidInput("NEXUS input is empty".into()))?;
    if !first.to_ascii_uppercase().starts_with("#NEXUS") {
        return Err(Error::InvalidInput(
            "NEXUS input must begin with #NEXUS".into(),
        ));
    }

    let mut in_trees = false;
    let mut collecting = false;
    let mut candidate = String::new();
    let mut saw_translate = false;
    let mut saw_other_blocks = false;
    for line in &lines {
        let trimmed = line.trim();
        let upper = trimmed.to_ascii_uppercase();
        if !in_trees {
            if upper.starts_with("BEGIN TREES") {
                in_trees = true;
            } else if upper.starts_with("BEGIN ") && !upper.starts_with("BEGIN TREES") {
                saw_other_blocks = true;
            }
            continue;
        }
        if upper.starts_with("TRANSLATE") {
            saw_translate = true;
        }
        if upper.starts_with("END") || upper.starts_with("ENDBLOCK") {
            if collecting {
                return Err(Error::InvalidInput(
                    "NEXUS TREE statement is not terminated before END".into(),
                ));
            }
            continue;
        }
        if !collecting {
            let is_tree = upper.starts_with("TREE ") || upper.starts_with("UTREE ");
            if !is_tree {
                continue;
            }
            let equal = trimmed
                .find('=')
                .ok_or_else(|| Error::InvalidInput("NEXUS TREE statement is missing '='".into()))?;
            candidate.push_str(trimmed[equal + 1..].trim());
            collecting = true;
        } else {
            candidate.push(' ');
            candidate.push_str(trimmed);
        }
        if let Some(end) = find_statement_terminator(&candidate) {
            candidate.truncate(end + 1);
            break;
        }
    }
    if !in_trees {
        return Err(Error::InvalidInput(
            "NEXUS input has no BEGIN TREES block".into(),
        ));
    }
    if candidate.is_empty() {
        return Err(Error::InvalidInput(
            "NEXUS TREES block contains no TREE or UTREE statement".into(),
        ));
    }
    if find_statement_terminator(&candidate).is_none() {
        return Err(Error::InvalidInput(
            "NEXUS TREE statement is not terminated with ';'".into(),
        ));
    }
    let mut warnings = vec![
        "NEXUS TAXA/TREES metadata and non-tree blocks were kept inert; only the first tree was rendered".into(),
    ];
    if saw_translate {
        warnings.push("NEXUS TRANSLATE map was not applied to tree labels".into());
    }
    if saw_other_blocks {
        warnings.push("NEXUS non-TREES blocks were ignored".into());
    }
    Ok((candidate, warnings))
}

fn find_statement_terminator(source: &str) -> Option<usize> {
    let mut comment_depth = 0usize;
    let mut quoted = false;
    for (index, character) in source.char_indices() {
        match character {
            '\'' if comment_depth == 0 => quoted = !quoted,
            '[' if !quoted => comment_depth = comment_depth.saturating_add(1),
            ']' if !quoted => comment_depth = comment_depth.saturating_sub(1),
            ';' if !quoted && comment_depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

fn dedup_warnings(warnings: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    warnings
        .into_iter()
        .filter(|warning| seen.insert(warning.clone()))
        .collect()
}
