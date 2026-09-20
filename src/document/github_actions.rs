//! Bounded GitHub Actions workflow previews.
//!
//! Workflow YAML is summarized as a job plan. Shell commands, expressions,
//! environment and secret values, reusable workflows, actions, artifacts and
//! runner/network operations remain inert; no workflow is dispatched or run.

use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_WORKFLOW_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WORKFLOW_LINES: usize = 500_000;
const MAX_WORKFLOW_LINE_BYTES: usize = 1024 * 1024;
const MAX_WORKFLOW_JOBS: usize = 100_000;
const MAX_WORKFLOW_STRING_BYTES: usize = 2 * 1024 * 1024;

/// Detect a workflow YAML document conservatively from its required `jobs` map.
pub(crate) fn looks_like_yaml_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let mut jobs = false;
    let mut job_entry = false;
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "..." {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let Some((raw_key, _)) = trimmed.split_once(':') else {
            continue;
        };
        let key = clean_key(raw_key);
        if indent == 0 {
            jobs = key == "jobs";
            job_entry = false;
        } else if jobs && indent == 2 {
            job_entry = true;
        } else if jobs
            && job_entry
            && indent >= 4
            && matches!(
                key.as_str(),
                "name" | "runs-on" | "needs" | "steps" | "uses" | "strategy" | "if" | "permissions"
            )
        {
            return true;
        }
    }
    false
}

struct WorkflowPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for WorkflowPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "github-actions".into();
        if page.title.is_empty() {
            page.title = "GitHub Actions workflow".into();
        }
        page.description =
            "GitHub Actions jobs and steps are rendered as inert metadata; no workflow execution or dispatch is performed".into();
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
        options.max_input_bytes.min(MAX_WORKFLOW_BYTES),
        "GitHub Actions workflow input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("GitHub Actions workflow must be UTF-8: {error}"))
    })?;
    let (table, metadata, warnings) = parse_yaml(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "GitHub Actions workflow".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = WorkflowPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_yaml(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_WORKFLOW_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GitHub Actions workflow exceeds {MAX_WORKFLOW_BYTES} bytes"
        )));
    }
    let (_, mut warnings) = crate::document::yaml::parse_yaml_blocks(text)?;
    let mut parser = WorkflowParser::default();
    for (line_number, raw) in text.lines().enumerate() {
        if line_number >= MAX_WORKFLOW_LINES {
            return Err(Error::LimitExceeded(format!(
                "GitHub Actions workflow exceeds {MAX_WORKFLOW_LINES} lines"
            )));
        }
        if raw.len() > MAX_WORKFLOW_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "GitHub Actions workflow line {} exceeds {MAX_WORKFLOW_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        parser.line(raw)?;
    }
    parser.finish()?;
    if parser.rows.is_empty() {
        return Err(Error::InvalidInput(
            "GitHub Actions workflow requires a non-empty jobs map".into(),
        ));
    }
    warnings.push("GitHub Actions run/uses commands, expressions, environment and secret values are omitted; actions, workflows, artifacts, runners and network operations are never executed".into());
    warnings.push("Workflow triggers, permissions, matrix expansion, reusable workflows and expressions are summarized without evaluation".into());
    let mut metadata = vec![
        format!("Jobs: {}", parser.rows.len()),
        format!("Triggers: {}", parser.triggers),
    ];
    if !parser.workflow_name.is_empty() {
        metadata.insert(0, format!("Name: {}", truncate(&parser.workflow_name)));
    }
    if parser.top_level_permissions {
        metadata.push("Top-level permissions: present".into());
    }
    Ok((table(parser.rows), metadata.join("\n"), warnings))
}

#[derive(Default)]
struct WorkflowParser {
    in_jobs: bool,
    top_section: Option<String>,
    current_field: Option<String>,
    current: Option<WorkflowJob>,
    rows: Vec<Vec<String>>,
    workflow_name: String,
    triggers: usize,
    top_level_permissions: bool,
}

#[derive(Default)]
struct WorkflowJob {
    id: String,
    name: String,
    runner: String,
    needs: usize,
    steps: usize,
    actions: usize,
    commands: usize,
    matrix: usize,
}

impl WorkflowParser {
    fn line(&mut self, raw: &str) -> Result<()> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Ok(());
        }
        if trimmed == "---" || trimmed == "..." {
            self.flush_job()?;
            self.in_jobs = false;
            self.top_section = None;
            self.current_field = None;
            return Ok(());
        }
        let indent = raw.len() - raw.trim_start().len();
        if indent == 0 {
            self.flush_job()?;
            self.current_field = None;
            let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
                self.in_jobs = false;
                self.top_section = None;
                return Ok(());
            };
            let key = clean_key(raw_key);
            let value = scalar(raw_value);
            if key == "name" && !value.is_empty() {
                self.workflow_name = truncate(&value);
            }
            if key == "on" {
                self.top_section = Some("on".into());
                self.triggers = self.triggers.saturating_add(inline_count(&value));
            } else if key == "permissions" {
                self.top_level_permissions = true;
                self.top_section = Some("permissions".into());
            } else {
                self.top_section = None;
            }
            self.in_jobs = key == "jobs";
            return Ok(());
        }
        if self.in_jobs {
            if indent == 2 {
                self.flush_job()?;
                let Some((raw_key, _)) = trimmed.split_once(':') else {
                    self.current_field = None;
                    return Ok(());
                };
                self.current = Some(WorkflowJob {
                    id: clean_key(raw_key),
                    ..WorkflowJob::default()
                });
                self.current_field = None;
                return Ok(());
            }
            if indent == 4 {
                let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
                    return Ok(());
                };
                let key = clean_key(raw_key);
                let value = scalar(raw_value);
                self.current_field = Some(key.clone());
                if let Some(job) = self.current.as_mut() {
                    match key.as_str() {
                        "name" => job.name = truncate(&value),
                        "runs-on" => job.runner = truncate(&value),
                        "needs" => job.needs += inline_count(&value),
                        "steps" => job.steps += inline_count(&value),
                        "strategy" => job.matrix += inline_count(&value),
                        _ => {}
                    }
                }
                return Ok(());
            }
            if indent == 6
                && let Some(field) = self.current_field.as_deref()
                && let Some(job) = self.current.as_mut()
            {
                if field == "steps" && trimmed.starts_with('-') {
                    job.steps = job.steps.saturating_add(1);
                    if trimmed.contains("uses:") {
                        job.actions = job.actions.saturating_add(1);
                    }
                    if trimmed.contains("run:") {
                        job.commands = job.commands.saturating_add(1);
                    }
                } else if field == "needs" && (trimmed.starts_with('-') || trimmed.contains(':')) {
                    job.needs = job.needs.saturating_add(1);
                } else if field == "strategy"
                    && clean_key(trimmed.split_once(':').map_or(trimmed, |(key, _)| key))
                        == "matrix"
                {
                    job.matrix = job.matrix.saturating_add(1);
                }
            } else if indent >= 8
                && let Some(field) = self.current_field.as_deref()
                && field == "steps"
                && let Some(job) = self.current.as_mut()
            {
                let key = trimmed
                    .split_once(':')
                    .map_or_else(String::new, |(key, _)| clean_key(key));
                if key == "uses" {
                    job.actions = job.actions.saturating_add(1);
                } else if key == "run" {
                    job.commands = job.commands.saturating_add(1);
                }
            }
            return Ok(());
        }
        if self.top_section.as_deref() == Some("on")
            && indent == 2
            && (trimmed.split_once(':').is_some() || trimmed.starts_with('-'))
        {
            self.triggers = self.triggers.saturating_add(1);
        }
        Ok(())
    }

    fn flush_job(&mut self) -> Result<()> {
        let Some(job) = self.current.take() else {
            return Ok(());
        };
        if self.rows.len() >= MAX_WORKFLOW_JOBS {
            return Err(Error::LimitExceeded(format!(
                "GitHub Actions jobs exceed {MAX_WORKFLOW_JOBS}"
            )));
        }
        let label = if job.name.is_empty() {
            job.id.clone()
        } else {
            format!("{} ({})", job.id, job.name)
        };
        let runner = if job.runner.is_empty() {
            "—".into()
        } else {
            job.runner
        };
        let execution = if job.matrix == 0 {
            format!("A:{} R:{}", job.actions, job.commands)
        } else {
            format!("A:{} R:{} M", job.actions, job.commands)
        };
        self.rows.push(vec![
            truncate(&label),
            truncate(&runner),
            format!("N:{} S:{}", job.needs, job.steps),
            execution,
        ]);
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.flush_job()
    }
}

fn table(rows: Vec<Vec<String>>) -> TableData {
    TableData {
        headers: vec![
            "Job".into(),
            "Runner".into(),
            "Flow".into(),
            "Execution".into(),
        ],
        rows,
        alignments: vec![TableAlign::Left; 4],
        raw_source: String::new(),
    }
}

fn clean_key(raw: &str) -> String {
    raw.trim().trim_matches(['"', '\'']).to_owned()
}

fn scalar(raw: &str) -> String {
    raw.trim()
        .split_once(" #")
        .map_or(raw.trim(), |(value, _)| value.trim())
        .trim_matches(['"', '\''])
        .to_owned()
}

fn inline_count(value: &str) -> usize {
    let value = value.trim();
    if value.is_empty() || value == "{}" || value == "[]" || value == "null" {
        return 0;
    }
    if value.starts_with('[') {
        return value
            .trim_matches(['[', ']'])
            .split(',')
            .filter(|part| !part.trim().is_empty())
            .count();
    }
    if value.starts_with('{') {
        return value
            .trim_matches(['{', '}'])
            .split(',')
            .filter(|part| part.split_once(':').is_some())
            .count();
    }
    if value.starts_with('-') { 1 } else { 0 }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_WORKFLOW_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_WORKFLOW_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_workflow_yaml() {
        assert!(looks_like_yaml_prefix(
            b"name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: cargo test\n"
        ));
        assert!(!looks_like_yaml_prefix(
            b"services:\n  web:\n    image: nginx\n"
        ));
    }

    #[test]
    fn omits_workflow_commands_and_counts_steps() {
        let (table, metadata, warnings) = parse_yaml(
            "name: CI\non: [push, pull_request]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    needs: [lint]\n    steps:\n      - uses: actions/checkout@v4\n      - run: echo very-secret\n",
        )
        .unwrap();
        assert!(metadata.contains("Jobs: 1"));
        assert_eq!(table.rows[0][2], "N:1 S:2");
        assert_eq!(table.rows[0][3], "A:1 R:1");
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("very-secret"))
        );
        assert!(warnings.iter().any(|warning| warning.contains("never")));
    }
}
