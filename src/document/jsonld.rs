//! Bounded JSON-LD 1.1 linked-data preview.
//!
//! This is deliberately a presentation adapter rather than a JSON-LD
//! expansion engine. It keeps compact terms as written, displays node
//! relationships as subject/predicate/object rows, and never dereferences
//! remote contexts or executes framing/inference.

use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages_with_warnings};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_JSONLD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSONLD_DEPTH: usize = 100;
const MAX_JSONLD_VALUES: usize = 200_000;
const MAX_JSONLD_STATEMENTS: usize = 200_000;
const MAX_JSONLD_TERM_CHARS: usize = 512;
const MAX_JSONLD_TEXT_BYTES: usize = 64 * 1024 * 1024;

/// Returns true for a JSON prefix that has the characteristic JSON-LD context
/// and graph/node keywords, while avoiding broad generic-JSON classification.
pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let has_context = text.contains("\"@context\"");
    let has_graph_or_node =
        text.contains("\"@graph\"") || text.contains("\"@id\"") || text.contains("\"@type\"");
    has_context && has_graph_or_node
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_JSONLD_BYTES),
        "JSON-LD input",
    )?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Error::InvalidInput(format!("JSON-LD input must be UTF-8: {error}")))?;
    let (rows, has_graph, warnings) = parse_jsonld(text)?;
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "JSON-LD input contains no renderable node statements".into(),
        ));
    }
    let mut headers = vec!["Subject".into(), "Predicate".into(), "Object".into()];
    if has_graph {
        headers.push("Graph".into());
    }
    let mut table_rows = rows;
    if has_graph {
        for row in &mut table_rows {
            row.resize(4, String::new());
        }
    }
    let table = TableData {
        headers,
        rows: table_rows,
        alignments: vec![TableAlign::Left; if has_graph { 4 } else { 3 }],
        raw_source: String::new(),
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "JSON-LD statements".into(),
        },
        HtmlBlock::Table(table),
    ];
    render_blocks_to_pages_with_warnings(&blocks, sink, options, &warnings)?;
    Ok(dedup_warnings(warnings))
}

struct WalkState {
    rows: Vec<Vec<String>>,
    has_graph: bool,
    warnings: Vec<String>,
    value_count: usize,
    text_bytes: usize,
    blank_counter: usize,
    contexts: usize,
    remote_contexts: usize,
    imports: usize,
}

fn parse_jsonld(text: &str) -> Result<(Vec<Vec<String>>, bool, Vec<String>)> {
    if text.len() as u64 > MAX_JSONLD_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON-LD input exceeds {MAX_JSONLD_BYTES} bytes"
        )));
    }
    preflight_json_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid JSON-LD input: {error}")))?;
    let mut state = WalkState {
        rows: Vec::new(),
        has_graph: false,
        warnings: Vec::new(),
        value_count: 0,
        text_bytes: 0,
        blank_counter: 0,
        contexts: 0,
        remote_contexts: 0,
        imports: 0,
    };
    walk_document(&value, None, 0, &mut state)?;
    if state.contexts > 0 {
        state.warnings.push(format!(
            "JSON-LD @context was retained as compact terms without namespace expansion ({})",
            state.contexts
        ));
    }
    if state.remote_contexts > 0 {
        state.warnings.push(format!(
            "{} remote JSON-LD context reference(s) were not fetched",
            state.remote_contexts
        ));
    }
    if state.imports > 0 {
        state.warnings.push(format!(
            "{} JSON-LD @import directive(s) were not resolved",
            state.imports
        ));
    }
    if state.has_graph {
        for row in &mut state.rows {
            row.resize(4, String::new());
        }
    }
    Ok((state.rows, state.has_graph, state.warnings))
}

/// Reject deeply nested JSON before `serde_json` builds its value tree. This
/// keeps hostile nesting from consuming the parser's call stack first.
fn preflight_json_depth(text: &str) -> Result<()> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for byte in text.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > MAX_JSONLD_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "JSON-LD nesting exceeds {MAX_JSONLD_DEPTH} levels"
                    )));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

fn walk_document(
    value: &Value,
    graph: Option<&str>,
    depth: usize,
    state: &mut WalkState,
) -> Result<()> {
    check_depth_and_count(depth, state)?;
    match value {
        Value::Array(values) => {
            for item in values {
                walk_document(item, graph, depth + 1, state)?;
            }
        }
        Value::Object(object) => {
            if let Some(context) = object.get("@context") {
                record_context(context, state);
            }
            if let Some(included) = object.get("@included") {
                walk_document(included, graph, depth + 1, state)?;
            }
            if let Some(graph_value) = object.get("@graph") {
                let graph_name = object.get("@id").and_then(value_term);
                let active_graph = graph_name.as_deref().or(graph);
                if active_graph.is_some() {
                    state.has_graph = true;
                }
                walk_document(graph_value, active_graph, depth + 1, state)?;
                // A graph object can also carry an ordinary node type/property.
                if object.keys().any(|key| !key.starts_with('@')) {
                    walk_node(object, graph, depth, state)?;
                }
            } else {
                walk_node(object, graph, depth, state)?;
            }
        }
        _ => push_warning_once(state, "JSON-LD top-level scalar value was omitted"),
    }
    Ok(())
}

fn walk_node(
    object: &serde_json::Map<String, Value>,
    graph: Option<&str>,
    depth: usize,
    state: &mut WalkState,
) -> Result<()> {
    let has_node_content = object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "@context" | "@graph" | "@included" | "@index" | "@reverse"
        )
    });
    if !has_node_content {
        return Ok(());
    }
    let subject = if let Some(id) = object.get("@id").and_then(value_term) {
        id
    } else {
        state.blank_counter = state.blank_counter.saturating_add(1);
        format!("_:b{}", state.blank_counter)
    };
    if let Some(types) = object.get("@type") {
        for value in values(types) {
            push_statement(
                state,
                &subject,
                "@type",
                &value_term(value).unwrap_or_default(),
                graph,
            )?;
        }
    }
    if object.contains_key("@reverse") {
        push_warning_once(
            state,
            "JSON-LD @reverse properties were omitted from the preview",
        );
    }
    for (predicate, value) in object {
        if predicate.starts_with('@') {
            continue;
        }
        for item in values(value) {
            let object_term = object_value_term(item, depth + 1, graph, state)?;
            push_statement(state, &subject, predicate, &object_term, graph)?;
        }
    }
    Ok(())
}

fn object_value_term(
    value: &Value,
    depth: usize,
    graph: Option<&str>,
    state: &mut WalkState,
) -> Result<String> {
    check_depth_and_count(depth, state)?;
    if let Some(id) = value.get("@id").and_then(value_term) {
        if let Value::Object(object) = value
            && object.keys().any(|key| !key.starts_with('@'))
        {
            walk_node(object, graph, depth + 1, state)?;
        }
        return Ok(id);
    }
    if let Some(literal) = value.get("@value") {
        let mut term = value_term(literal).unwrap_or_default();
        if let Some(language) = value.get("@language").and_then(value_term) {
            term.push_str(" @");
            term.push_str(&language);
        }
        if let Some(datatype) = value.get("@type").and_then(value_term) {
            term.push_str(" ^^ ");
            term.push_str(&datatype);
        }
        return Ok(truncate(&term));
    }
    if let Some(list) = value.get("@list") {
        let mut items = Vec::new();
        for item in values(list) {
            let term = if item.is_object() {
                object_value_term(item, depth + 1, graph, state)?
            } else {
                value_term(item).unwrap_or_default()
            };
            items.push(truncate(&term));
        }
        return Ok(truncate(&format!("[{}]", items.join(", "))));
    }
    if value.is_object() {
        state.blank_counter = state.blank_counter.saturating_add(1);
        let blank = format!("_:b{}", state.blank_counter);
        if let Value::Object(object) = value {
            walk_node(object, graph, depth + 1, state)?;
        }
        return Ok(blank);
    }
    Ok(truncate(&value_term(value).unwrap_or_default()))
}

fn push_statement(
    state: &mut WalkState,
    subject: &str,
    predicate: &str,
    object: &str,
    graph: Option<&str>,
) -> Result<()> {
    if state.rows.len() >= MAX_JSONLD_STATEMENTS {
        return Err(Error::LimitExceeded(format!(
            "JSON-LD exceeds {MAX_JSONLD_STATEMENTS} statements"
        )));
    }
    let mut row = vec![truncate(subject), truncate(predicate), truncate(object)];
    if let Some(graph) = graph {
        state.has_graph = true;
        row.push(truncate(graph));
    } else if state.has_graph {
        row.push(String::new());
    }
    state.text_bytes = state
        .text_bytes
        .saturating_add(row.iter().map(String::len).sum::<usize>());
    if state.text_bytes > MAX_JSONLD_TEXT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON-LD rendered text exceeds {MAX_JSONLD_TEXT_BYTES} bytes"
        )));
    }
    state.rows.push(row);
    Ok(())
}

fn record_context(value: &Value, state: &mut WalkState) {
    state.contexts = state.contexts.saturating_add(1);
    for item in values(value) {
        if item
            .as_str()
            .is_some_and(|text| text.starts_with("http://") || text.starts_with("https://"))
        {
            state.remote_contexts = state.remote_contexts.saturating_add(1);
        }
        if let Value::Object(object) = item
            && object.contains_key("@import")
        {
            state.imports = state.imports.saturating_add(1);
        }
    }
}

fn values(value: &Value) -> Vec<&Value> {
    value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(value))
        .iter()
        .collect()
}

fn value_term(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Null => Some("null".into()),
        _ => None,
    }
}

fn truncate(value: &str) -> String {
    value.chars().take(MAX_JSONLD_TERM_CHARS).collect()
}

fn check_depth_and_count(depth: usize, state: &mut WalkState) -> Result<()> {
    if depth > MAX_JSONLD_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "JSON-LD nesting exceeds {MAX_JSONLD_DEPTH} levels"
        )));
    }
    state.value_count = state.value_count.saturating_add(1);
    if state.value_count > MAX_JSONLD_VALUES {
        return Err(Error::LimitExceeded(format!(
            "JSON-LD contains more than {MAX_JSONLD_VALUES} values"
        )));
    }
    Ok(())
}

fn dedup_warnings(mut warnings: Vec<String>) -> Vec<String> {
    warnings.sort();
    warnings.dedup();
    warnings
}

fn push_warning_once(state: &mut WalkState, warning: &str) {
    if !state.warnings.iter().any(|item| item == warning) {
        state.warnings.push(warning.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{looks_like_prefix, parse_jsonld, preflight_json_depth};

    #[test]
    fn renders_context_nodes_literals_and_named_graphs() {
        let source = r#"{
          "@context": {"name": "https://schema.org/name"},
          "@graph": [
            {"@id": "urn:book:1", "@type": "Book", "name": "A title", "author": {"@id": "urn:person:1"}},
            {"@id": "urn:graph:1", "@graph": [{"@id": "urn:book:2", "name": {"@value": "Deux", "@language": "fr"}}]}
          ]
        }"#;
        let (rows, has_graph, warnings) = parse_jsonld(source).unwrap();
        assert!(has_graph);
        assert!(
            rows.iter()
                .any(|row| row[1] == "name" && row[2] == "A title")
        );
        assert!(rows.iter().any(|row| row[2] == "urn:person:1"));
        assert!(rows.iter().any(|row| row[3] == "urn:graph:1"));
        assert!(warnings.iter().any(|warning| warning.contains("@context")));
    }

    #[test]
    fn recognizes_jsonld_shape_without_confusing_plain_json() {
        assert!(looks_like_prefix(b"{\"@context\":{},\"@id\":\"x\"}"));
        assert!(!looks_like_prefix(b"{\"context\":{},\"id\":\"x\"}"));
    }

    #[test]
    fn rejects_excessive_json_nesting_before_value_parsing() {
        let nested = "[".repeat(101) + &"]".repeat(101);
        let error = preflight_json_depth(&nested).unwrap_err();
        assert!(error.to_string().contains("nesting"));
    }

    #[test]
    fn reports_remote_contexts_without_fetching_them() {
        let source = r#"{"@context":"https://example.invalid/context.jsonld","@id":"urn:x","name":"offline"}"#;
        let (rows, _, warnings) = parse_jsonld(source).unwrap();
        assert_eq!(rows[0][2], "offline");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not fetched"))
        );
    }
}
