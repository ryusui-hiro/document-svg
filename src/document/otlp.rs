//! Bounded OpenTelemetry Protocol (OTLP) JSON previews.
//!
//! OTLP JSON payloads contain traces, metrics, logs and profiles with nested
//! attributes and bodies. This adapter renders signal/scope counts and optional
//! service metadata only; attribute values, log bodies, exemplars, links and
//! endpoints remain inert and are never exported, fetched or executed.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_OTLP_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OTLP_DEPTH: usize = 100;
const MAX_OTLP_VALUES: usize = 300_000;
const MAX_OTLP_ROWS: usize = 200_000;
const MAX_OTLP_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_OTLP_DISPLAY_BYTES: usize = 256;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) {
        return SIGNAL_KEYS.iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_array)
                .is_some_and(|items| !items.is_empty())
        });
    }
    SIGNAL_KEYS
        .iter()
        .any(|key| text.contains(&format!("\"{key}\"")))
}

const SIGNAL_KEYS: &[&str] = &[
    "resourceSpans",
    "resourceMetrics",
    "resourceLogs",
    "resourceProfiles",
];

struct OtlpPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for OtlpPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "otlp-json".into();
        if page.title.is_empty() {
            page.title = "OpenTelemetry OTLP JSON".into();
        }
        page.description =
            "OTLP JSON signal and scope metadata is rendered inertly; attribute values, log bodies and telemetry links are not displayed or exported".into();
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
        options.max_input_bytes.min(MAX_OTLP_BYTES),
        "OTLP JSON input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("OTLP JSON must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OpenTelemetry OTLP JSON".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = OtlpPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

#[derive(Default)]
struct Summary {
    rows: Vec<Vec<String>>,
    resources: usize,
    spans: usize,
    metrics: usize,
    data_points: usize,
    logs: usize,
    profiles: usize,
    attributes: usize,
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_OTLP_BYTES {
        return Err(Error::LimitExceeded(format!(
            "OTLP JSON exceeds {MAX_OTLP_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid OTLP JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("OTLP JSON root must be an object".into()))?;
    let mut summary = Summary::default();
    let mut warnings = Vec::new();
    if let Some(resource_spans) = signal_array(root, "resourceSpans")? {
        for resource_span in resource_spans {
            summarize_traces(resource_span, &mut summary)?;
        }
    }
    if let Some(resource_metrics) = signal_array(root, "resourceMetrics")? {
        for resource_metric in resource_metrics {
            summarize_metrics(resource_metric, &mut summary)?;
        }
    }
    if let Some(resource_logs) = signal_array(root, "resourceLogs")? {
        for resource_log in resource_logs {
            summarize_logs(resource_log, &mut summary)?;
        }
    }
    if let Some(resource_profiles) = signal_array(root, "resourceProfiles")? {
        for resource_profile in resource_profiles {
            summarize_profiles(resource_profile, &mut summary)?;
        }
    }
    if summary.rows.is_empty() {
        return Err(Error::InvalidInput(
            "OTLP JSON requires a non-empty resourceSpans, resourceMetrics, resourceLogs or resourceProfiles array".into(),
        ));
    }
    let metadata = format!(
        "Rows: {}\nResources: {}\nSpans: {}\nMetrics: {}\nData points: {}\nLog records: {}\nProfiles: {}\nAttribute keys: {}",
        summary.rows.len(),
        summary.resources,
        summary.spans,
        summary.metrics,
        summary.data_points,
        summary.logs,
        summary.profiles,
        summary.attributes
    );
    warnings.push("OTLP attribute values, log bodies, trace/span IDs, exemplars, links, schema URLs and instrumentation payloads are omitted; no collector, endpoint, exporter or network operation runs".into());
    warnings.push("OTLP JSON uses the protobuf JSON camelCase field names and bounded traversal; numeric timestamps and values are counted but not interpreted".into());
    Ok((
        TableData {
            headers: vec![
                "Sig".into(),
                "Svc".into(),
                "Scope".into(),
                "N".into(),
                "Structure".into(),
            ],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn signal_array<'a>(
    root: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<&'a [Value]>> {
    root.get(key)
        .map(|value| {
            value
                .as_array()
                .map(Vec::as_slice)
                .ok_or_else(|| Error::InvalidInput(format!("OTLP {key} must be an array")))
        })
        .transpose()
}

fn summarize_traces(resource_span: &Value, summary: &mut Summary) -> Result<()> {
    let object = resource_span
        .as_object()
        .ok_or_else(|| Error::InvalidInput("OTLP resourceSpans item must be an object".into()))?;
    summary.resources = summary.resources.saturating_add(1);
    summary.attributes = summary
        .attributes
        .saturating_add(attribute_count(object.get("resource")));
    let service = service_name(object.get("resource"));
    let scopes = object
        .get("scopeSpans")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::InvalidInput("OTLP resourceSpans item requires scopeSpans array".into())
        })?;
    for scope in scopes {
        let scope_object = scope
            .as_object()
            .ok_or_else(|| Error::InvalidInput("OTLP scopeSpans item must be an object".into()))?;
        let spans = scope_object
            .get("spans")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidInput("OTLP scopeSpans item requires spans array".into())
            })?;
        summary.spans = summary.spans.saturating_add(spans.len());
        let events = spans
            .iter()
            .map(|span| {
                span.get("events")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            })
            .sum::<usize>();
        let links = spans
            .iter()
            .map(|span| {
                span.get("links")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            })
            .sum::<usize>();
        summary.attributes = summary
            .attributes
            .saturating_add(attribute_count(scope_object.get("scope")));
        push_row(
            summary,
            vec![
                "traces".into(),
                service.clone(),
                scope_name(scope_object.get("scope")),
                spans.len().to_string(),
                format!("events {events} · links {links} · attrs omitted"),
            ],
        )?;
    }
    Ok(())
}

fn summarize_metrics(resource_metric: &Value, summary: &mut Summary) -> Result<()> {
    let object = resource_metric
        .as_object()
        .ok_or_else(|| Error::InvalidInput("OTLP resourceMetrics item must be an object".into()))?;
    summary.resources = summary.resources.saturating_add(1);
    summary.attributes = summary
        .attributes
        .saturating_add(attribute_count(object.get("resource")));
    let service = service_name(object.get("resource"));
    let scopes = object
        .get("scopeMetrics")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::InvalidInput("OTLP resourceMetrics item requires scopeMetrics array".into())
        })?;
    for scope in scopes {
        let scope_object = scope.as_object().ok_or_else(|| {
            Error::InvalidInput("OTLP scopeMetrics item must be an object".into())
        })?;
        let metrics = scope_object
            .get("metrics")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidInput("OTLP scopeMetrics item requires metrics array".into())
            })?;
        let data_points = metrics.iter().map(metric_data_points).sum::<usize>();
        summary.metrics = summary.metrics.saturating_add(metrics.len());
        summary.data_points = summary.data_points.saturating_add(data_points);
        summary.attributes = summary
            .attributes
            .saturating_add(attribute_count(scope_object.get("scope")));
        push_row(
            summary,
            vec![
                "metrics".into(),
                service.clone(),
                scope_name(scope_object.get("scope")),
                metrics.len().to_string(),
                format!("data points {data_points} · attrs omitted"),
            ],
        )?;
    }
    Ok(())
}

fn summarize_logs(resource_log: &Value, summary: &mut Summary) -> Result<()> {
    let object = resource_log
        .as_object()
        .ok_or_else(|| Error::InvalidInput("OTLP resourceLogs item must be an object".into()))?;
    summary.resources = summary.resources.saturating_add(1);
    summary.attributes = summary
        .attributes
        .saturating_add(attribute_count(object.get("resource")));
    let service = service_name(object.get("resource"));
    let scopes = object
        .get("scopeLogs")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::InvalidInput("OTLP resourceLogs item requires scopeLogs array".into())
        })?;
    for scope in scopes {
        let scope_object = scope
            .as_object()
            .ok_or_else(|| Error::InvalidInput("OTLP scopeLogs item must be an object".into()))?;
        let records = scope_object
            .get("logRecords")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidInput("OTLP scopeLogs item requires logRecords array".into())
            })?;
        summary.logs = summary.logs.saturating_add(records.len());
        summary.attributes = summary
            .attributes
            .saturating_add(attribute_count(scope_object.get("scope")));
        push_row(
            summary,
            vec![
                "logs".into(),
                service.clone(),
                scope_name(scope_object.get("scope")),
                records.len().to_string(),
                "body omitted · attrs omitted".into(),
            ],
        )?;
    }
    Ok(())
}

fn summarize_profiles(resource_profile: &Value, summary: &mut Summary) -> Result<()> {
    let object = resource_profile.as_object().ok_or_else(|| {
        Error::InvalidInput("OTLP resourceProfiles item must be an object".into())
    })?;
    summary.resources = summary.resources.saturating_add(1);
    let service = service_name(object.get("resource"));
    let scopes = object
        .get("scopeProfiles")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::InvalidInput("OTLP resourceProfiles item requires scopeProfiles array".into())
        })?;
    for scope in scopes {
        let scope_object = scope.as_object().ok_or_else(|| {
            Error::InvalidInput("OTLP scopeProfiles item must be an object".into())
        })?;
        let profiles = scope_object
            .get("profiles")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidInput("OTLP scopeProfiles item requires profiles array".into())
            })?;
        summary.profiles = summary.profiles.saturating_add(profiles.len());
        push_row(
            summary,
            vec![
                "profiles".into(),
                service.clone(),
                scope_name(scope_object.get("scope")),
                profiles.len().to_string(),
                "profile payload omitted".into(),
            ],
        )?;
    }
    Ok(())
}

fn metric_data_points(metric: &Value) -> usize {
    let Some(object) = metric.as_object() else {
        return 0;
    };
    [
        "gauge",
        "sum",
        "histogram",
        "exponentialHistogram",
        "summary",
    ]
    .iter()
    .map(|key| {
        object
            .get(*key)
            .and_then(Value::as_object)
            .and_then(|value| value.get("dataPoints"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    })
    .sum()
}

fn attribute_count(value: Option<&Value>) -> usize {
    value
        .and_then(Value::as_object)
        .and_then(|object| object.get("attributes"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn service_name(value: Option<&Value>) -> String {
    let Some(attributes) = value
        .and_then(Value::as_object)
        .and_then(|object| object.get("attributes"))
        .and_then(Value::as_array)
    else {
        return "—".into();
    };
    for attribute in attributes {
        let Some(attribute) = attribute.as_object() else {
            continue;
        };
        if attribute.get("key").and_then(Value::as_str) != Some("service.name") {
            continue;
        }
        if let Some(service) = attribute
            .get("value")
            .and_then(Value::as_object)
            .and_then(|value| value.get("stringValue"))
            .and_then(Value::as_str)
        {
            return truncate(service);
        }
        return "(set)".into();
    }
    "—".into()
}

fn scope_name(value: Option<&Value>) -> String {
    let Some(object) = value.and_then(Value::as_object) else {
        return "—".into();
    };
    let name = object.get("name").and_then(Value::as_str).unwrap_or("—");
    let version = object.get("version").and_then(Value::as_str).unwrap_or("");
    if version.is_empty() {
        truncate(name)
    } else {
        truncate(&format!("{name} {version}"))
    }
}

fn push_row(summary: &mut Summary, row: Vec<String>) -> Result<()> {
    if summary.rows.len() >= MAX_OTLP_ROWS {
        return Err(Error::LimitExceeded(format!(
            "OTLP rows exceed {MAX_OTLP_ROWS}"
        )));
    }
    summary.rows.push(row);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_OTLP_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_OTLP_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_OTLP_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "OTLP JSON nesting exceeds {MAX_OTLP_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_OTLP_VALUES {
        return Err(Error::LimitExceeded(format!(
            "OTLP JSON contains more than {MAX_OTLP_VALUES} values"
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
        Value::String(value) if value.len() > MAX_OTLP_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "OTLP JSON string exceeds {MAX_OTLP_STRING_BYTES} bytes"
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
                if depth > MAX_OTLP_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "OTLP JSON nesting exceeds {MAX_OTLP_DEPTH} levels"
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
    fn recognizes_otlp_signals() {
        assert!(looks_like_prefix(
            br#"{"resourceSpans":[{"scopeSpans":[]}] }"#
        ));
        assert!(looks_like_prefix(
            br#"{"resourceMetrics":[{"scopeMetrics":[]}] }"#
        ));
        assert!(!looks_like_prefix(br#"{"resourceSpans":[]}"#));
    }

    #[test]
    fn summarizes_signals_without_attribute_or_body_values() {
        let (table, metadata, warnings) = parse(
            r#"{"resourceSpans":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"checkout"}},{"key":"token","value":{"stringValue":"secret"}}]},"scopeSpans":[{"scope":{"name":"demo","version":"1.0"},"spans":[{"events":[{"name":"private event"}],"links":[{}]}]}]}],"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"logs"}}]},"scopeLogs":[{"scope":{"name":"logger"},"logRecords":[{"body":{"stringValue":"very-secret"}}]}]}]}"#,
        )
        .unwrap();
        assert_eq!(table.rows.len(), 2);
        assert!(metadata.contains("Spans: 1"));
        assert!(metadata.contains("Log records: 1"));
        assert!(table.rows.iter().flatten().any(|value| value == "checkout"));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("attribute values"))
        );
    }
}
