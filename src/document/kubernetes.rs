//! Bounded Kubernetes API object / manifest previews.
//!
//! Kubernetes objects identify their schema with `apiVersion`, `kind`, and
//! `metadata`. This adapter lists those identities and bounded shape counts;
//! Secret values, scripts, templates and spec payloads remain inert. No
//! kubectl, Helm, cluster endpoint, external reference, or resource URL is
//! contacted.

use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_K8S_JSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_K8S_YAML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_K8S_DEPTH: usize = 100;
const MAX_K8S_VALUES: usize = 300_000;
const MAX_K8S_RESOURCES: usize = 100_000;
const MAX_K8S_LINES: usize = 500_000;
const MAX_K8S_LINE_BYTES: usize = 1024 * 1024;
const MAX_K8S_STRING_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn looks_like_json_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"apiVersion\"")
        && text.contains("\"kind\"")
        && text.contains("\"metadata\"")
}

pub(crate) fn looks_like_yaml_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let api = text
        .lines()
        .any(|line| line.trim_start().starts_with("apiVersion:"));
    let kind = text
        .lines()
        .any(|line| line.trim_start().starts_with("kind:"));
    let metadata = text
        .lines()
        .any(|line| line.trim_start().starts_with("metadata:"));
    api && kind && metadata
}

struct KubernetesPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for KubernetesPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "kubernetes".into();
        if page.title.is_empty() {
            page.title = "Kubernetes manifests".into();
        }
        page.description = "Kubernetes object identities are rendered inertly; cluster operations and Secret values are not accessed".into();
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
        options.max_input_bytes.min(MAX_K8S_JSON_BYTES),
        "Kubernetes manifest input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Kubernetes manifest must be UTF-8: {error}"))
    })?;
    let (table, metadata, warnings) = if text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('{')
    {
        parse_json(&text)?
    } else {
        parse_yaml(&text)?
    };
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "Kubernetes manifests".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = KubernetesPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_json(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_K8S_JSON_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Kubernetes JSON exceeds {MAX_K8S_JSON_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid Kubernetes JSON: {error}")))?;
    let mut count = 0usize;
    count_values(&value, 0, &mut count)?;
    let mut rows = Vec::new();
    let mut secret_count = 0usize;
    let mut list_count = 0usize;
    collect_json_resource(&value, 1, &mut rows, &mut secret_count, &mut list_count)?;
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "Kubernetes JSON contains no apiVersion/kind/metadata resource".into(),
        ));
    }
    let mut warnings = vec!["Kubernetes specs, status payloads, Secret values, scripts, templates, external references and URLs are summarized inertly; no kubectl, Helm, cluster request or resource fetch is performed".into()];
    if secret_count > 0 {
        warnings.push(format!(
            "{secret_count} Kubernetes Secret resource(s) had data values omitted"
        ));
    }
    if list_count > 0 {
        warnings.push(format!(
            "{list_count} Kubernetes List object(s) were expanded into resource rows"
        ));
    }
    let metadata = format!("Resources: {}", rows.len());
    Ok((table(rows), metadata, warnings))
}

fn collect_json_resource(
    value: &Value,
    document: usize,
    rows: &mut Vec<Vec<String>>,
    secret_count: &mut usize,
    list_count: &mut usize,
) -> Result<()> {
    if rows.len() >= MAX_K8S_RESOURCES {
        return Err(Error::LimitExceeded(format!(
            "Kubernetes resources exceed {MAX_K8S_RESOURCES}"
        )));
    }
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    if object.get("kind").and_then(Value::as_str) == Some("List")
        && let Some(items) = object.get("items").and_then(Value::as_array)
    {
        *list_count += 1;
        for item in items {
            collect_json_resource(item, document, rows, secret_count, list_count)?;
        }
        return Ok(());
    }
    let (Some(api), Some(kind), Some(meta)) = (
        object.get("apiVersion").and_then(Value::as_str),
        object.get("kind").and_then(Value::as_str),
        object.get("metadata").and_then(Value::as_object),
    ) else {
        return Ok(());
    };
    let name = meta
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("(unnamed)");
    let namespace = meta
        .get("namespace")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let labels = meta
        .get("labels")
        .and_then(Value::as_object)
        .map_or(0, |v| v.len());
    let annotations = meta
        .get("annotations")
        .and_then(Value::as_object)
        .map_or(0, |v| v.len());
    let spec = object
        .get("spec")
        .and_then(Value::as_object)
        .map_or(0, |v| v.len());
    let status = object
        .get("status")
        .and_then(Value::as_object)
        .map_or(0, |v| v.len());
    let secret_keys = if kind.eq_ignore_ascii_case("Secret") {
        *secret_count += 1;
        object
            .get("data")
            .and_then(Value::as_object)
            .map_or(0, |v| v.len())
            + object
                .get("stringData")
                .and_then(Value::as_object)
                .map_or(0, |v| v.len())
    } else {
        0
    };
    let identity = if namespace.is_empty() {
        name.to_owned()
    } else {
        format!("{namespace}/{name}")
    };
    let shape = format!("spec:{spec}; labels:{labels}; annotations:{annotations}");
    let state = if kind.eq_ignore_ascii_case("Secret") {
        format!("status:{status}; secret keys:{secret_keys} (values omitted)")
    } else {
        format!("status:{status}")
    };
    rows.push(vec![
        truncate(&format!("{kind}/{identity}")),
        truncate(api),
        truncate(&shape),
        truncate(&state),
        document.to_string(),
    ]);
    Ok(())
}

fn parse_yaml(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_K8S_YAML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Kubernetes YAML exceeds {MAX_K8S_YAML_BYTES} bytes"
        )));
    }
    crate::document::yaml::parse_yaml_blocks(text)?;
    let mut rows = Vec::new();
    let mut current = YamlResource::default();
    let mut resources = 0usize;
    let mut secrets = 0usize;
    let mut skipped = 0usize;
    for (line_number, raw) in text.lines().enumerate() {
        if line_number >= MAX_K8S_LINES {
            return Err(Error::LimitExceeded(format!(
                "Kubernetes YAML exceeds {MAX_K8S_LINES} lines"
            )));
        }
        if raw.len() > MAX_K8S_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Kubernetes YAML line {} exceeds {MAX_K8S_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "..." {
            continue;
        }
        if trimmed == "---" {
            if current.has_identity() {
                push_yaml_resource(&mut rows, &mut current, &mut resources, &mut secrets)?;
            }
            current = YamlResource::default();
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = raw_key.trim().trim_matches(['"', '\'']);
        let value = scalar(raw_value);
        if indent == 0 {
            if key == "apiVersion"
                || key == "kind"
                || key == "metadata"
                || key == "spec"
                || key == "status"
                || key == "data"
                || key == "stringData"
            {
                if key == "apiVersion" {
                    current.api = value;
                } else if key == "kind" {
                    current.kind = value;
                } else if key == "metadata" {
                    current.section = "metadata".into();
                } else if key == "spec" {
                    current.section = "spec".into();
                } else if key == "status" {
                    current.section = "status".into();
                } else if key == "data" || key == "stringData" {
                    current.section = key.into();
                }
            } else {
                skipped += 1;
            }
            continue;
        }
        match current.section.as_str() {
            "metadata" => {
                if indent == 2 && key == "name" {
                    current.name = value;
                } else if indent == 2 && key == "namespace" {
                    current.namespace = value;
                } else if indent == 2 && (key == "labels" || key == "annotations") {
                    current.meta_map = key.into();
                } else if indent >= 4
                    && (current.meta_map == "labels" || current.meta_map == "annotations")
                    && !key.is_empty()
                {
                    if current.meta_map == "labels" {
                        current.labels += 1;
                    } else {
                        current.annotations += 1;
                    }
                }
            }
            "spec" => {
                if indent == 2 && !key.is_empty() {
                    current.spec += 1;
                }
            }
            "status" => {
                if indent == 2 && !key.is_empty() {
                    current.status += 1;
                }
            }
            "data" | "stringData" => {
                if indent == 2 && !key.is_empty() {
                    current.secret_keys += 1;
                }
            }
            _ => {}
        }
    }
    if current.has_identity() {
        push_yaml_resource(&mut rows, &mut current, &mut resources, &mut secrets)?;
    }
    if rows.is_empty() {
        return Err(Error::InvalidInput(
            "Kubernetes YAML contains no apiVersion/kind/metadata resource".into(),
        ));
    }
    let mut warnings = vec!["Kubernetes specs, status payloads, Secret values, scripts, templates, external references and URLs are summarized inertly; no kubectl, Helm, cluster request or resource fetch is performed".into()];
    if secrets > 0 {
        warnings.push(format!(
            "{secrets} Kubernetes Secret resource(s) had data values omitted"
        ));
    }
    if skipped > 0 {
        warnings.push(format!(
            "{skipped} non-resource top-level YAML field/document line(s) were omitted"
        ));
    }
    Ok((table(rows), format!("Resources: {resources}"), warnings))
}

#[derive(Default)]
struct YamlResource {
    api: String,
    kind: String,
    name: String,
    namespace: String,
    section: String,
    meta_map: String,
    labels: usize,
    annotations: usize,
    spec: usize,
    status: usize,
    secret_keys: usize,
}
impl YamlResource {
    fn has_identity(&self) -> bool {
        !self.api.is_empty() && !self.kind.is_empty() && !self.name.is_empty()
    }
}

fn push_yaml_resource(
    rows: &mut Vec<Vec<String>>,
    resource: &mut YamlResource,
    count: &mut usize,
    secrets: &mut usize,
) -> Result<()> {
    if rows.len() >= MAX_K8S_RESOURCES {
        return Err(Error::LimitExceeded(format!(
            "Kubernetes resources exceed {MAX_K8S_RESOURCES}"
        )));
    }
    *count += 1;
    let secret = resource.kind.eq_ignore_ascii_case("Secret");
    if secret {
        *secrets += 1;
    }
    let identity = if resource.namespace.is_empty() {
        resource.name.clone()
    } else {
        format!("{}/{}", resource.namespace, resource.name)
    };
    let shape = format!(
        "spec:{}; labels:{}; annotations:{}",
        resource.spec, resource.labels, resource.annotations
    );
    let state = if secret {
        format!(
            "status:{}; secret keys:{} (values omitted)",
            resource.status, resource.secret_keys
        )
    } else {
        format!("status:{}", resource.status)
    };
    rows.push(vec![
        truncate(&format!("{}/{}", resource.kind, identity)),
        truncate(&resource.api),
        truncate(&shape),
        truncate(&state),
        count.to_string(),
    ]);
    Ok(())
}

fn table(rows: Vec<Vec<String>>) -> TableData {
    TableData {
        headers: vec![
            "Resource".into(),
            "API version".into(),
            "Spec shape".into(),
            "Status / Secret".into(),
            "Doc".into(),
        ],
        rows,
        alignments: vec![TableAlign::Left; 5],
        raw_source: String::new(),
    }
}
fn scalar(value: &str) -> String {
    value
        .trim()
        .split_once(" #")
        .map_or(value.trim(), |(v, _)| v.trim())
        .trim_matches(['"', '\''])
        .to_owned()
}
fn truncate(value: &str) -> String {
    if value.len() <= MAX_K8S_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_K8S_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_K8S_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "Kubernetes JSON nesting exceeds {MAX_K8S_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_K8S_VALUES {
        return Err(Error::LimitExceeded(format!(
            "Kubernetes JSON contains more than {MAX_K8S_VALUES} values"
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
        Value::String(text) if text.len() > MAX_K8S_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "Kubernetes JSON string exceeds {MAX_K8S_STRING_BYTES} bytes"
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
                if depth > MAX_K8S_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "Kubernetes JSON nesting exceeds {MAX_K8S_DEPTH} levels"
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
    fn previews_json_resources_and_omits_secret_values() {
        let source = r#"{"apiVersion":"v1","kind":"Secret","metadata":{"name":"demo","namespace":"qa","labels":{"app":"demo"}},"type":"Opaque","data":{"password":"c2VjcmV0"}}"#;
        let (table, metadata, warnings) = parse_json(source).unwrap();
        assert!(metadata.contains("Resources: 1"));
        assert!(table.rows[0][0].contains("Secret/qa/demo"));
        assert!(table.rows[0][3].contains("values omitted"));
        assert!(warnings.iter().any(|warning| warning.contains("Secret")));
    }
    #[test]
    fn previews_yaml_documents() {
        let source = "apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\n  namespace: prod\nspec:\n  replicas: 2\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: web\nspec:\n  ports:\n    - port: 80\n";
        let (table, metadata, _) = parse_yaml(source).unwrap();
        assert!(metadata.contains("Resources: 2"));
        assert_eq!(table.rows.len(), 2);
        assert!(table.rows[0][0].contains("Deployment/prod/web"));
    }
    #[test]
    fn rejects_generic_json() {
        assert!(parse_json("{\"kind\":\"Thing\"}").is_err());
    }
}
