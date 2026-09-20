//! Streaming, bounded JSON configuration/data preview.
//!
//! Values are rendered as inert path/type/value text; no JSON field or URL is
//! executed, resolved, or fetched. The visitor avoids retaining a full generic
//! value tree in memory.

use std::fmt;
use std::path::Path;

use serde::de::Error as DeError;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};

const MAX_JSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 100;
const MAX_JSON_VALUES: usize = 200_000;
const MAX_JSON_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSON_PATH_BYTES: usize = 4 * 1024;
const MAX_JSON_RENDERED_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_JSON_OUTPUT_BLOCKS: usize = 200_000;

pub(crate) fn looks_like_json_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

#[derive(Default)]
struct PreviewState {
    blocks: Vec<HtmlBlock>,
    value_count: usize,
    text_bytes: usize,
    truncated_paths: usize,
    normalized_number_count: usize,
    limit_error: Option<String>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_JSON_BYTES),
        "JSON input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("JSON input must be UTF-8: {error}")))?;
    let (blocks, warnings) = parse_json_blocks(&text)?;
    let mut page_sink = JsonPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

struct JsonPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for JsonPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "json".into();
        if page.title.is_empty() {
            page.title = "JSON data".into();
        }
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

pub(crate) fn parse_json_blocks(text: &str) -> Result<(Vec<HtmlBlock>, Vec<String>)> {
    if text.len() as u64 > MAX_JSON_BYTES {
        return Err(Error::LimitExceeded(format!(
            "JSON input exceeds {MAX_JSON_BYTES} bytes"
        )));
    }
    let mut state = PreviewState::default();
    let mut deserializer = serde_json::Deserializer::from_str(text);
    let parse_result = JsonSeed {
        state: &mut state,
        path: "$".into(),
        depth: 0,
    }
    .deserialize(&mut deserializer);
    if let Err(error) = parse_result {
        if let Some(limit) = state.limit_error.take() {
            return Err(Error::LimitExceeded(limit));
        }
        return Err(Error::InvalidInput(format!("invalid JSON input: {error}")));
    }
    deserializer
        .end()
        .map_err(|error| Error::InvalidInput(format!("invalid trailing JSON data: {error}")))?;

    let mut blocks = vec![HtmlBlock::Heading {
        level: 1,
        text: "JSON data".into(),
    }];
    blocks.append(&mut state.blocks);
    let mut warnings = Vec::new();
    if state.truncated_paths > 0 {
        warnings.push(format!(
            "{0} JSON field path(s) were truncated to {1} bytes for display",
            state.truncated_paths, MAX_JSON_PATH_BYTES
        ));
    }
    if state.normalized_number_count > 0 {
        warnings.push(format!(
            "{} JSON decimal/exponent number(s) were normalized for display; original number spelling is not retained",
            state.normalized_number_count
        ));
    }
    Ok((blocks, warnings))
}

struct JsonSeed<'a> {
    state: &'a mut PreviewState,
    path: String,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for JsonSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> std::result::Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.depth > MAX_JSON_DEPTH {
            return self
                .state
                .limit::<D::Error>(format!("JSON nesting exceeds {MAX_JSON_DEPTH} levels"));
        }
        self.state.value_count = self.state.value_count.saturating_add(1);
        if self.state.value_count > MAX_JSON_VALUES {
            return self
                .state
                .limit::<D::Error>(format!("JSON contains more than {MAX_JSON_VALUES} values"));
        }
        deserializer.deserialize_any(JsonVisitor {
            state: self.state,
            path: self.path,
            depth: self.depth,
        })
    }
}

struct JsonVisitor<'a> {
    state: &'a mut PreviewState,
    path: String,
    depth: usize,
}

impl<'de> Visitor<'de> for JsonVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        self.state
            .add_scalar::<E>(&self.path, "boolean", if value { "true" } else { "false" })
    }

    fn visit_i64<E>(self, value: i64) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        self.state
            .add_scalar::<E>(&self.path, "number", &value.to_string())
    }

    fn visit_u64<E>(self, value: u64) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        self.state
            .add_scalar::<E>(&self.path, "number", &value.to_string())
    }

    fn visit_f64<E>(self, value: f64) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        self.state.normalized_number_count = self.state.normalized_number_count.saturating_add(1);
        self.state
            .add_scalar::<E>(&self.path, "number", &value.to_string())
    }

    fn visit_str<E>(self, value: &str) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        self.state.add_json_string::<E>(&self.path, value)
    }

    fn visit_string<E>(self, value: String) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        self.state.add_json_string::<E>(&self.path, &value)
    }

    fn visit_unit<E>(self) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        self.state.add_scalar::<E>(&self.path, "null", "null")
    }

    fn visit_none<E>(self) -> std::result::Result<(), E>
    where
        E: de::Error,
    {
        self.state.add_scalar::<E>(&self.path, "null", "null")
    }

    fn visit_some<D>(self, deserializer: D) -> std::result::Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        JsonSeed {
            state: self.state,
            path: self.path,
            depth: self.depth,
        }
        .deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut index = 0usize;
        while let Some(()) = sequence.next_element_seed(JsonSeed {
            state: self.state,
            path: format!("{}[{index}]", self.path),
            depth: self.depth + 1,
        })? {
            index = index.saturating_add(1);
        }
        if index == 0 {
            self.state
                .add_scalar::<A::Error>(&self.path, "array", "[]")?;
        }
        Ok(())
    }

    fn visit_map<A>(self, mut object: A) -> std::result::Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut count = 0usize;
        while let Some(key) = object.next_key::<String>()? {
            let path = join_path(&self.path, &key, self.state).map_err(A::Error::custom)?;
            object.next_value_seed(JsonSeed {
                state: self.state,
                path,
                depth: self.depth + 1,
            })?;
            count = count.saturating_add(1);
        }
        if count == 0 {
            self.state
                .add_scalar::<A::Error>(&self.path, "object", "{}")?;
        }
        Ok(())
    }
}

impl PreviewState {
    fn limit<E: de::Error>(&mut self, message: String) -> std::result::Result<(), E> {
        self.limit_error = Some(message.clone());
        Err(E::custom(message))
    }

    fn add_scalar<E: de::Error>(
        &mut self,
        path: &str,
        kind: &str,
        value: &str,
    ) -> std::result::Result<(), E> {
        let line = format!("{path} ({kind}): {value}");
        self.add_line::<E>(line)
    }

    fn add_json_string<E: de::Error>(
        &mut self,
        path: &str,
        value: &str,
    ) -> std::result::Result<(), E> {
        if value.len() > MAX_JSON_STRING_BYTES {
            return self.limit::<E>(format!("JSON string exceeds {MAX_JSON_STRING_BYTES} bytes"));
        }
        let encoded = serde_json::to_string(value).map_err(E::custom)?;
        self.add_scalar::<E>(path, "string", &encoded)
    }

    fn add_line<E: de::Error>(&mut self, line: String) -> std::result::Result<(), E> {
        if self.blocks.len() >= MAX_JSON_OUTPUT_BLOCKS {
            return self.limit::<E>(format!(
                "JSON preview exceeds {MAX_JSON_OUTPUT_BLOCKS} output values"
            ));
        }
        let text_bytes = self.text_bytes.saturating_add(line.len());
        if text_bytes > MAX_JSON_RENDERED_TEXT_BYTES {
            return self.limit::<E>(format!(
                "JSON preview text exceeds {MAX_JSON_RENDERED_TEXT_BYTES} bytes"
            ));
        }
        self.text_bytes = text_bytes;
        self.blocks.push(HtmlBlock::Paragraph { text: line });
        Ok(())
    }
}

fn join_path(
    path: &str,
    key: &str,
    state: &mut PreviewState,
) -> std::result::Result<String, &'static str> {
    if key.len() > MAX_JSON_STRING_BYTES {
        state.limit_error = Some(format!(
            "JSON object key exceeds {MAX_JSON_STRING_BYTES} bytes"
        ));
        return Err("JSON object key exceeds the size limit");
    }
    let simple_key = !key.is_empty()
        && key.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character == '$'
                || character.is_ascii_alphanumeric() && (index > 0 || !character.is_ascii_digit())
        });
    let child = if simple_key {
        format!("{path}.{key}")
    } else {
        let encoded = serde_json::to_string(key).unwrap_or_else(|_| "\"?\"".into());
        format!("{path}[{encoded}]")
    };
    if child.len() > MAX_JSON_PATH_BYTES {
        state.truncated_paths = state.truncated_paths.saturating_add(1);
        let mut end = MAX_JSON_PATH_BYTES;
        while !child.is_char_boundary(end) {
            end -= 1;
        }
        Ok(child[..end].to_owned())
    } else {
        Ok(child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_nested_json_values_and_escapes_script_like_strings() {
        let input = r#"{"service":{"enabled":true,"ports":[8080,8081]},"message":"<script>alert(1)</script>","empty":{}}"#;
        let (blocks, _) = parse_json_blocks(input).unwrap();
        let text = blocks
            .iter()
            .filter_map(|block| match block {
                HtmlBlock::Heading { text, .. } | HtmlBlock::Paragraph { text } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(text.contains(&"$.service.enabled (boolean): true"));
        assert!(text.contains(&"$.service.ports[1] (number): 8081"));
        assert!(text.contains(&"$.empty (object): {}"));
        assert!(text.contains(&r#"$.message (string): "<script>alert(1)</script>""#));
    }

    #[test]
    fn rejects_invalid_json_and_limits_depth() {
        assert!(matches!(
            parse_json_blocks("{\"key\":}"),
            Err(Error::InvalidInput(_))
        ));
        let nested = format!(
            "{}0{}",
            "[".repeat(MAX_JSON_DEPTH + 2),
            "]".repeat(MAX_JSON_DEPTH + 2)
        );
        assert!(matches!(
            parse_json_blocks(&nested),
            Err(Error::LimitExceeded(_))
        ));
    }

    #[test]
    fn warns_when_decimal_number_spelling_is_normalized() {
        let (blocks, warnings) = parse_json_blocks("{\"ratio\":1.5000}").unwrap();
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("number(s) were normalized"))
        );
        assert!(blocks.iter().any(|block| matches!(block, HtmlBlock::Paragraph { text } if text == "$.ratio (number): 1.5")));
    }

    #[test]
    fn does_not_confuse_structured_json_with_known_json_formats() {
        let config = br#"{"service":{"port":8080}}"#;
        assert!(looks_like_json_prefix(config));
        assert!(!looks_like_json_prefix(b"not json"));
    }
}
