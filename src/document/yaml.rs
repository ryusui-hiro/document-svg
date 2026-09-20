//! Bounded YAML 1.2 previews that keep aliases and tags inert.

use std::collections::HashSet;
use std::path::Path;

use yaml_rust2::parser::{Event, Parser};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_YAML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_YAML_LINES: usize = 100_000;
const MAX_YAML_LINE_BYTES: usize = 1024 * 1024;
const MAX_YAML_DEPTH: usize = 80;
const MAX_YAML_EVENTS: usize = 400_000;
const MAX_YAML_OUTPUT_BLOCKS: usize = 200_000;
const MAX_YAML_DOCUMENTS: usize = 100;
const MAX_YAML_SCALAR_BYTES: usize = 2 * 1024 * 1024;
const MAX_YAML_PATH_BYTES: usize = 4 * 1024;
const MAX_YAML_RENDERED_TEXT_BYTES: usize = 32 * 1024 * 1024;

enum Container {
    Sequence {
        path: String,
        next_index: usize,
        item_count: usize,
    },
    Mapping {
        path: String,
        expecting_key: bool,
        pending_key: Option<String>,
        entries: usize,
        keys: HashSet<String>,
    },
}

#[derive(Default)]
struct PreviewState {
    blocks: Vec<HtmlBlock>,
    stack: Vec<Container>,
    root_started: bool,
    documents: usize,
    events: usize,
    nodes: usize,
    text_bytes: usize,
    truncated_paths: usize,
    aliases: usize,
    tagged_nodes: usize,
    duplicate_keys: usize,
}

struct YamlPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for YamlPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "yaml".into();
        if page.title.is_empty() {
            page.title = "YAML data".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_YAML_BYTES),
        "YAML input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("YAML input must be UTF-8: {error}")))?;
    let (blocks, warnings) = parse_yaml_blocks(&text)?;
    if options.max_pages == 0 {
        return Err(Error::LimitExceeded(
            "YAML conversion requires at least one page; max_pages is zero".into(),
        ));
    }
    let mut page_sink = YamlPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

pub(crate) fn parse_yaml_blocks(text: &str) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if text.len() as u64 > MAX_YAML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "YAML input exceeds {MAX_YAML_BYTES} bytes"
        )));
    }
    for (line_number, line) in text.lines().enumerate() {
        if line_number >= MAX_YAML_LINES {
            return Err(Error::LimitExceeded(format!(
                "YAML input exceeds {MAX_YAML_LINES} lines"
            )));
        }
        if line.len() > MAX_YAML_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "YAML line exceeds {MAX_YAML_LINE_BYTES} bytes"
            )));
        }
    }

    let mut parser = Parser::new_from_str(text);
    let mut state = PreviewState::default();
    state.blocks.push(HtmlBlock::Heading {
        level: 1,
        text: "YAML data".into(),
    });
    loop {
        let (event, _) = parser
            .next_token()
            .map_err(|error| Error::InvalidInput(format!("invalid YAML input: {error}")))?;
        let is_end = event == Event::StreamEnd;
        state.process(event)?;
        if is_end {
            break;
        }
    }
    if !state.stack.is_empty() {
        return Err(invalid("parser ended with an unclosed collection"));
    }

    let mut warnings = Vec::new();
    if state.aliases > 0 {
        warnings.push(format!(
            "{} YAML alias reference(s) are shown as placeholders and were not expanded",
            state.aliases
        ));
    }
    if state.tagged_nodes > 0 {
        warnings.push(format!(
            "{} YAML custom-tagged node(s) are displayed as inert data; tag semantics were not applied",
            state.tagged_nodes
        ));
    }
    if state.duplicate_keys > 0 {
        warnings.push(format!(
            "{} duplicate YAML mapping key(s) are shown as separate path rows",
            state.duplicate_keys
        ));
    }
    if state.truncated_paths > 0 {
        warnings.push(format!(
            "{} YAML field path(s) were truncated to {MAX_YAML_PATH_BYTES} bytes for display",
            state.truncated_paths
        ));
    }
    Ok((state.blocks, warnings))
}

impl PreviewState {
    fn process(&mut self, event: Event) -> Result<()> {
        self.events = self.events.saturating_add(1);
        if self.events > MAX_YAML_EVENTS {
            return Err(Error::LimitExceeded(format!(
                "YAML stream exceeds {MAX_YAML_EVENTS} parser events"
            )));
        }
        match event {
            Event::StreamStart | Event::StreamEnd | Event::Nothing => Ok(()),
            Event::DocumentStart => {
                if !self.stack.is_empty() {
                    return Err(invalid(
                        "a new document started before the prior collection ended",
                    ));
                }
                self.documents = self.documents.saturating_add(1);
                if self.documents > MAX_YAML_DOCUMENTS {
                    return Err(Error::LimitExceeded(format!(
                        "YAML stream exceeds {MAX_YAML_DOCUMENTS} documents"
                    )));
                }
                self.root_started = false;
                self.blocks.push(HtmlBlock::Heading {
                    level: 2,
                    text: format!("Document {}", self.documents),
                });
                Ok(())
            }
            Event::DocumentEnd => {
                if !self.stack.is_empty() {
                    return Err(invalid("document ended before a collection was closed"));
                }
                if !self.root_started {
                    self.add_line("$ (empty document): \"\"".into())?;
                }
                Ok(())
            }
            Event::Scalar(value, _style, _anchor, tag) => {
                self.add_node()?;
                if tag.is_some() {
                    self.tagged_nodes = self.tagged_nodes.saturating_add(1);
                }
                if self.mapping_expects_key() {
                    return self.set_mapping_key(value);
                }
                if value.len() > MAX_YAML_SCALAR_BYTES {
                    return Err(Error::LimitExceeded(format!(
                        "YAML scalar exceeds {MAX_YAML_SCALAR_BYTES} bytes"
                    )));
                }
                let path = self.begin_value()?;
                let encoded = serde_json::to_string(&value)?;
                self.add_line(format!("{path} (scalar): {encoded}"))
            }
            Event::SequenceStart(_anchor, tag) => {
                self.add_node()?;
                if tag.is_some() {
                    self.tagged_nodes = self.tagged_nodes.saturating_add(1);
                }
                let path = self.begin_value()?;
                self.push_container(Container::Sequence {
                    path,
                    next_index: 0,
                    item_count: 0,
                })
            }
            Event::MappingStart(_anchor, tag) => {
                self.add_node()?;
                if tag.is_some() {
                    self.tagged_nodes = self.tagged_nodes.saturating_add(1);
                }
                let path = self.begin_value()?;
                self.push_container(Container::Mapping {
                    path,
                    expecting_key: true,
                    pending_key: None,
                    entries: 0,
                    keys: HashSet::new(),
                })
            }
            Event::SequenceEnd => {
                let Some(Container::Sequence {
                    path, item_count, ..
                }) = self.stack.pop()
                else {
                    return Err(invalid("sequence end does not match an open sequence"));
                };
                if item_count == 0 {
                    self.add_line(format!("{path} (sequence): []"))?;
                }
                Ok(())
            }
            Event::MappingEnd => {
                let Some(Container::Mapping {
                    path,
                    expecting_key,
                    entries,
                    ..
                }) = self.stack.pop()
                else {
                    return Err(invalid("mapping end does not match an open mapping"));
                };
                if !expecting_key {
                    return Err(invalid("mapping key has no value"));
                }
                if entries == 0 {
                    self.add_line(format!("{path} (mapping): {{}}"))?;
                }
                Ok(())
            }
            Event::Alias(anchor_id) => {
                self.add_node()?;
                self.aliases = self.aliases.saturating_add(1);
                if self.mapping_expects_key() {
                    return self.set_mapping_key(format!("*anchor-{anchor_id}"));
                }
                let path = self.begin_value()?;
                self.add_line(format!(
                    "{path} (alias): *anchor-{anchor_id} (not expanded)"
                ))
            }
        }
    }

    fn add_node(&mut self) -> Result<()> {
        self.nodes = self.nodes.saturating_add(1);
        if self.nodes > MAX_YAML_EVENTS {
            return Err(Error::LimitExceeded(format!(
                "YAML stream exceeds {MAX_YAML_EVENTS} nodes"
            )));
        }
        Ok(())
    }

    fn mapping_expects_key(&self) -> bool {
        matches!(
            self.stack.last(),
            Some(Container::Mapping {
                expecting_key: true,
                ..
            })
        )
    }

    fn set_mapping_key(&mut self, key: String) -> Result<()> {
        if key.len() > MAX_YAML_SCALAR_BYTES {
            return Err(Error::LimitExceeded(format!(
                "YAML mapping key exceeds {MAX_YAML_SCALAR_BYTES} bytes"
            )));
        }
        let Some(Container::Mapping {
            expecting_key,
            pending_key,
            keys,
            ..
        }) = self.stack.last_mut()
        else {
            return Err(invalid("mapping key appeared outside a mapping"));
        };
        if !*expecting_key {
            return Err(invalid("mapping value is missing before the next key"));
        }
        if !keys.insert(key.clone()) {
            // YAML duplicate keys are ambiguous across common loaders; the preview retains both.
            self.duplicate_keys = self.duplicate_keys.saturating_add(1);
        }
        *pending_key = Some(key);
        *expecting_key = false;
        Ok(())
    }

    fn begin_value(&mut self) -> Result<String> {
        if self.stack.is_empty() {
            if self.root_started {
                return Err(invalid("document contains more than one root node"));
            }
            self.root_started = true;
            return Ok("$".into());
        }
        let value_path = match self.stack.last_mut().expect("stack is non-empty") {
            Container::Sequence {
                path,
                next_index,
                item_count,
            } => {
                let value_path = format!("{path}[{next_index}]");
                *next_index = next_index.saturating_add(1);
                *item_count = item_count.saturating_add(1);
                value_path
            }
            Container::Mapping {
                path,
                expecting_key,
                pending_key,
                entries,
                ..
            } => {
                if *expecting_key {
                    return Err(Error::Unsupported(
                        "YAML complex mapping keys are not expanded in previews".into(),
                    ));
                }
                let key = pending_key
                    .take()
                    .ok_or_else(|| invalid("mapping value has no preceding key"))?;
                *expecting_key = true;
                *entries = entries.saturating_add(1);
                join_path(path, &key)
            }
        };
        if value_path.len() > MAX_YAML_PATH_BYTES {
            Ok(self.truncate_path(value_path))
        } else {
            Ok(value_path)
        }
    }

    fn push_container(&mut self, container: Container) -> Result<()> {
        if self.stack.len() >= MAX_YAML_DEPTH {
            return Err(Error::LimitExceeded(format!(
                "YAML nesting exceeds {MAX_YAML_DEPTH} levels"
            )));
        }
        self.stack.push(container);
        Ok(())
    }

    fn truncate_path(&mut self, path: String) -> String {
        self.truncated_paths = self.truncated_paths.saturating_add(1);
        let mut end = MAX_YAML_PATH_BYTES;
        while !path.is_char_boundary(end) {
            end -= 1;
        }
        path[..end].to_owned()
    }

    fn add_line(&mut self, line: String) -> Result<()> {
        if self.blocks.len() >= MAX_YAML_OUTPUT_BLOCKS {
            return Err(Error::LimitExceeded(format!(
                "YAML preview exceeds {MAX_YAML_OUTPUT_BLOCKS} rendered values"
            )));
        }
        let text_bytes = self.text_bytes.saturating_add(line.len());
        if text_bytes > MAX_YAML_RENDERED_TEXT_BYTES {
            return Err(Error::LimitExceeded(format!(
                "YAML preview text exceeds {MAX_YAML_RENDERED_TEXT_BYTES} bytes"
            )));
        }
        self.text_bytes = text_bytes;
        self.blocks.push(HtmlBlock::Paragraph { text: line });
        Ok(())
    }
}

fn join_path(parent: &str, key: &str) -> String {
    let simple = !key.is_empty()
        && key.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character == '$'
                || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
        });
    if simple {
        format!("{parent}.{key}")
    } else {
        let encoded = serde_json::to_string(key).unwrap_or_else(|_| "\"?\"".into());
        format!("{parent}[{encoded}]")
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(format!("invalid YAML input: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_nested_yaml_and_keeps_tags_and_aliases_inert() {
        let source = r#"---
service:
  name: "catalog-api"
  active: true
  regions: [west, east]
defaults: &defaults
  retries: 3
use_defaults: *defaults
literal: !include "./secrets.yml"
---
kind: second-document
"#;
        let (blocks, warnings) = parse_yaml_blocks(source).unwrap();
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Heading { text, .. } | HtmlBlock::Paragraph { text } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(text.contains(&"$.service.name (scalar): \"catalog-api\""));
        assert!(text.contains(&"$.service.regions[1] (scalar): \"east\""));
        assert!(
            text.iter()
                .any(|line| line.contains("alias") && line.contains("not expanded"))
        );
        assert!(text.contains(&"$.literal (scalar): \"./secrets.yml\""));
        assert!(text.contains(&"Document 2"));
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("alias reference"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("custom-tagged"))
        );
    }

    #[test]
    fn rejects_bad_yaml_and_complex_mapping_keys() {
        assert!(parse_yaml_blocks("key: [unterminated\n").is_err());
        assert!(matches!(
            parse_yaml_blocks("? [complex, key]\n: value\n"),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn enforces_yaml_line_depth_and_document_limits() {
        let overlong_line = format!("{}\n", "x".repeat(MAX_YAML_LINE_BYTES + 1));
        assert!(matches!(
            parse_yaml_blocks(&overlong_line),
            Err(Error::LimitExceeded(_))
        ));

        let mut nested = String::new();
        for level in 0..=MAX_YAML_DEPTH {
            nested.push_str(&" ".repeat(level * 2));
            nested.push_str(&format!("level{level}:\n"));
        }
        nested.push_str(&" ".repeat((MAX_YAML_DEPTH + 1) * 2));
        nested.push_str("value: done\n");
        assert!(matches!(
            parse_yaml_blocks(&nested),
            Err(Error::LimitExceeded(_))
        ));

        let documents = (0..=MAX_YAML_DOCUMENTS)
            .map(|_| "---\nvalue: 1\n")
            .collect::<String>();
        assert!(matches!(
            parse_yaml_blocks(&documents),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn rejects_alias_bombs_without_expanding_them() {
        let mut source = "root: &root [0]\n".to_owned();
        let mut previous = "root".to_owned();
        for index in 0..32 {
            source.push_str(&format!(
                "layer{index}: &layer{index} [*{previous}, *{previous}]\n"
            ));
            previous = format!("layer{index}");
        }
        let (_, warnings) = parse_yaml_blocks(&source).unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("not expanded"))
        );
    }
}
