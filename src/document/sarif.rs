//! Bounded SARIF 2.1.0 static-analysis result previews.
//!
//! SARIF is a JSON interchange format for code-scanning tools. This adapter
//! aggregates results by tool/rule/severity and intentionally omits messages,
//! source locations, snippets, URIs, fingerprints and fix payloads. It never
//! uploads, resolves or executes anything from the report.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_SARIF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SARIF_DEPTH: usize = 100;
const MAX_SARIF_VALUES: usize = 300_000;
const MAX_SARIF_RUNS: usize = 100_000;
const MAX_SARIF_RULES: usize = 100_000;
const MAX_SARIF_RESULTS: usize = 300_000;
const MAX_SARIF_STRING_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"runs\"")
        && text.contains("\"tool\"")
        && text.contains("\"version\"")
        && (text.contains("2.1.0") || text.contains("sarif"))
}

struct SarifPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for SarifPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "sarif".into();
        if page.title.is_empty() {
            page.title = "SARIF static analysis".into();
        }
        page.description =
            "SARIF tools, rules and result severities are rendered as inert metadata; messages, locations and upload operations are omitted".into();
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
        options.max_input_bytes.min(MAX_SARIF_BYTES),
        "SARIF input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("SARIF input must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "SARIF static analysis".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = SarifPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default, Clone, Copy)]
struct SeverityCounts {
    error: usize,
    warning: usize,
    note: usize,
    other: usize,
}

impl SeverityCounts {
    fn add_level(&mut self, level: &str) {
        match level.to_ascii_lowercase().as_str() {
            "error" => self.error = self.error.saturating_add(1),
            "warning" => self.warning = self.warning.saturating_add(1),
            "note" => self.note = self.note.saturating_add(1),
            _ => self.other = self.other.saturating_add(1),
        }
    }

    fn total(self) -> usize {
        self.error
            .saturating_add(self.warning)
            .saturating_add(self.note)
            .saturating_add(self.other)
    }
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_SARIF_BYTES {
        return Err(Error::LimitExceeded(format!(
            "SARIF input exceeds {MAX_SARIF_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid SARIF JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("SARIF root must be an object".into()))?;
    let version = root
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput("SARIF requires a version string".into()))?;
    if version != "2.1.0" {
        return Err(Error::Unsupported(format!(
            "SARIF version {version} is unsupported; only 2.1.0 is accepted"
        )));
    }
    let runs = root
        .get("runs")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidInput("SARIF requires a runs array".into()))?;
    if runs.is_empty() {
        return Err(Error::InvalidInput("SARIF runs array is empty".into()));
    }
    if runs.len() > MAX_SARIF_RUNS {
        return Err(Error::LimitExceeded(format!(
            "SARIF runs exceed {MAX_SARIF_RUNS}"
        )));
    }
    let mut rows = BTreeMap::<String, SeverityCounts>::new();
    let mut total_rules = 0usize;
    let mut total_results = 0usize;
    for run in runs {
        let Some(run_object) = run.as_object() else {
            continue;
        };
        let (tool_name, rule_levels) = tool_info(run_object, &mut total_rules)?;
        let results = run_object
            .get("results")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        for result in results {
            total_results = total_results.saturating_add(1);
            if total_results > MAX_SARIF_RESULTS {
                return Err(Error::LimitExceeded(format!(
                    "SARIF results exceed {MAX_SARIF_RESULTS}"
                )));
            }
            let Some(result_object) = result.as_object() else {
                continue;
            };
            let rule = result_object
                .get("ruleId")
                .and_then(Value::as_str)
                .or_else(|| {
                    result_object
                        .get("ruleIndex")
                        .and_then(Value::as_u64)
                        .and_then(|index| {
                            rule_levels.get(index as usize).map(|(id, _)| id.as_str())
                        })
                })
                .unwrap_or("(unassigned rule)");
            let default_level = result_object
                .get("ruleIndex")
                .and_then(Value::as_u64)
                .and_then(|index| {
                    rule_levels
                        .get(index as usize)
                        .map(|(_, level)| level.as_str())
                })
                .unwrap_or("warning");
            let level = result_object
                .get("level")
                .and_then(Value::as_str)
                .unwrap_or(default_level);
            let key = format!("{}\u{1f}{rule}", tool_name);
            rows.entry(key).or_default().add_level(level);
        }
    }
    let table_rows = if rows.is_empty() {
        vec![vec![
            "(no results)".into(),
            "—".into(),
            "0".into(),
            "0".into(),
            "0 / 0".into(),
            "0".into(),
        ]]
    } else {
        rows.into_iter()
            .map(|(key, counts)| {
                let (tool, rule) = key.split_once('\u{1f}').unwrap_or(("(unknown tool)", &key));
                vec![
                    truncate(tool),
                    truncate(rule),
                    counts.error.to_string(),
                    counts.warning.to_string(),
                    format!("{} / {}", counts.note, counts.other),
                    counts.total().to_string(),
                ]
            })
            .collect::<Vec<_>>()
    };
    let metadata = format!(
        "Version: {version}\nRuns: {}\nRules: {total_rules}\nResults: {total_results}\nRule groups: {}",
        runs.len(),
        table_rows.len()
    );
    let warnings = vec![
        "SARIF messages, descriptions, source locations, artifact URIs, snippets, fingerprints, fixes and invocation details are omitted; uploads, URL fetches and code execution are never performed".into(),
        "SARIF results are aggregated by tool/rule and severity without evaluating rule metadata or alert content".into(),
    ];
    Ok((
        TableData {
            headers: vec![
                "Tool".into(),
                "Rule".into(),
                "Err".into(),
                "Warn".into(),
                "Note/oth".into(),
                "N".into(),
            ],
            rows: table_rows,
            alignments: vec![TableAlign::Left; 6],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn tool_info(
    run: &serde_json::Map<String, Value>,
    total_rules: &mut usize,
) -> Result<(String, Vec<(String, String)>)> {
    let tool = run
        .get("tool")
        .and_then(Value::as_object)
        .and_then(|tool| tool.get("driver"))
        .and_then(Value::as_object);
    let tool_name = tool
        .and_then(|tool| tool.get("name"))
        .and_then(Value::as_str)
        .map_or_else(|| "(unknown tool)".into(), truncate);
    let rules = tool
        .and_then(|tool| tool.get("rules"))
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if rules.len() > MAX_SARIF_RULES {
        return Err(Error::LimitExceeded(format!(
            "SARIF rules exceed {MAX_SARIF_RULES}"
        )));
    }
    *total_rules = total_rules.saturating_add(rules.len());
    if *total_rules > MAX_SARIF_RULES {
        return Err(Error::LimitExceeded(format!(
            "SARIF rules exceed {MAX_SARIF_RULES}"
        )));
    }
    let mut levels = Vec::with_capacity(rules.len());
    for (index, rule) in rules.iter().enumerate() {
        let id = rule
            .as_object()
            .and_then(|rule| rule.get("id"))
            .and_then(Value::as_str)
            .map_or_else(|| format!("rule-{index}"), truncate);
        let level = rule
            .as_object()
            .and_then(|rule| rule.get("defaultConfiguration"))
            .and_then(Value::as_object)
            .and_then(|config| config.get("level"))
            .and_then(Value::as_str)
            .unwrap_or("warning")
            .to_owned();
        levels.push((id, level));
    }
    Ok((tool_name, levels))
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_SARIF_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_SARIF_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_SARIF_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "SARIF JSON nesting exceeds {MAX_SARIF_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_SARIF_VALUES {
        return Err(Error::LimitExceeded(format!(
            "SARIF JSON contains more than {MAX_SARIF_VALUES} values"
        )));
    }
    match value {
        Value::Array(values) => {
            for item in values {
                count_values(item, depth + 1, count)?;
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                count_values(item, depth + 1, count)?;
            }
        }
        Value::String(value) if value.len() > MAX_SARIF_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "SARIF JSON string exceeds {MAX_SARIF_STRING_BYTES} bytes"
            )));
        }
        _ => {}
    }
    Ok(())
}

fn preflight_depth(text: &str) -> Result<()> {
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
                depth += 1;
                if depth > MAX_SARIF_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "SARIF JSON nesting exceeds {MAX_SARIF_DEPTH} levels"
                    )));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_sarif_signature() {
        assert!(looks_like_prefix(
            br#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"scanner"}}}]}"#
        ));
        assert!(!looks_like_prefix(b"{\"version\":\"1\",\"runs\":[]}"));
    }

    #[test]
    fn aggregates_levels_without_alert_payloads() {
        let (table, metadata, warnings) = parse(
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"scanner","rules":[{"id":"SEC-1","defaultConfiguration":{"level":"error"}}]}},"results":[{"ruleId":"SEC-1","message":{"text":"very-secret"},"level":"error","locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///private"}}}]},{"ruleId":"SEC-1","level":"note"}]}]}"#,
        )
        .unwrap();
        assert!(metadata.contains("Results: 2"));
        assert_eq!(table.rows[0][2], "1");
        assert_eq!(table.rows[0][4], "1 / 0");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret") || value.contains("private"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("omitted")));
    }

    #[test]
    fn accepts_empty_result_runs() {
        let (table, metadata, _) = parse(
            r#"{"version":"2.1.0","runs":[{"tool":{"driver":{"name":"clean"}},"results":[]}]}"#,
        )
        .unwrap();
        assert!(metadata.contains("Results: 0"));
        assert_eq!(table.rows[0][0], "(no results)");
    }
}
