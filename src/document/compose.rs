//! Bounded Docker Compose file previews.
//!
//! Compose files describe services and shared resources. This adapter renders
//! an inert service inventory with bounded counts; it never starts containers,
//! pulls or builds images, evaluates commands, resolves interpolation/includes,
//! contacts a Docker daemon, reads environment values, or exposes secret data.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_COMPOSE_JSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_COMPOSE_YAML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_COMPOSE_DEPTH: usize = 100;
const MAX_COMPOSE_VALUES: usize = 300_000;
const MAX_COMPOSE_SERVICES: usize = 100_000;
const MAX_COMPOSE_LINES: usize = 500_000;
const MAX_COMPOSE_LINE_BYTES: usize = 1024 * 1024;
const MAX_COMPOSE_STRING_BYTES: usize = 2 * 1024 * 1024;

/// Detect a Compose YAML document from its required top-level `services` map.
pub(crate) fn looks_like_yaml_prefix(prefix: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(prefix) else {
        return false;
    };
    let mut services = false;
    let mut service_entry = false;
    let mut service_property = false;
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" || trimmed == "..." {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let Some((key, _)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches(['"', '\'']);
        if indent == 0 {
            if key == "apiVersion" || key == "kind" {
                return false;
            }
            services = key == "services";
            service_entry = false;
            service_property = false;
        } else if services && indent == 2 {
            service_entry = true;
        } else if services && indent >= 4 && service_entry {
            service_property = matches!(
                key,
                "image"
                    | "build"
                    | "command"
                    | "entrypoint"
                    | "environment"
                    | "restart"
                    | "container_name"
                    | "expose"
                    | "healthcheck"
                    | "deploy"
                    | "profiles"
                    | "ports"
                    | "depends_on"
                    | "volumes"
                    | "networks"
                    | "secrets"
            );
        }
        if services && service_entry && service_property {
            return true;
        }
    }
    false
}

/// Detect a Compose JSON document conservatively to avoid claiming generic
/// configuration objects which merely happen to have a `services` key.
pub(crate) fn looks_like_json_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{')
        && text.contains("\"services\"")
        && [
            "\"image\"",
            "\"build\"",
            "\"environment\"",
            "\"restart\"",
            "\"container_name\"",
            "\"expose\"",
            "\"healthcheck\"",
            "\"deploy\"",
            "\"profiles\"",
            "\"ports\"",
            "\"depends_on\"",
            "\"volumes\"",
            "\"networks\"",
            "\"secrets\"",
        ]
        .iter()
        .any(|needle| text.contains(needle))
}

struct ComposePageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for ComposePageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "compose".into();
        if page.title.is_empty() {
            page.title = "Docker Compose".into();
        }
        page.description =
            "Docker Compose services and resource references are rendered as inert metadata; no runtime operation is performed".into();
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
        options.max_input_bytes.min(MAX_COMPOSE_JSON_BYTES),
        "Docker Compose input",
    )?;
    let text = String::from_utf8(bytes).map_err(|error| {
        Error::InvalidInput(format!("Docker Compose input must be UTF-8: {error}"))
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
            text: "Docker Compose".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = ComposePageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse_json(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_COMPOSE_JSON_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Docker Compose JSON exceeds {MAX_COMPOSE_JSON_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid Docker Compose JSON: {error}")))?;
    let mut count = 0usize;
    count_values(&value, 0, &mut count)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("Docker Compose JSON root must be an object".into()))?;
    let services = root
        .get("services")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::InvalidInput("Docker Compose JSON requires a services object".into())
        })?;
    if services.is_empty() {
        return Err(Error::InvalidInput(
            "Docker Compose JSON services object is empty".into(),
        ));
    }
    if services.len() > MAX_COMPOSE_SERVICES {
        return Err(Error::LimitExceeded(format!(
            "Docker Compose services exceed {MAX_COMPOSE_SERVICES}"
        )));
    }
    let rows = services
        .iter()
        .map(|(name, definition)| json_row(name, definition))
        .collect::<Vec<_>>();
    let mut warnings = base_warnings();
    warnings.push(
        "Compose JSON command, environment, healthcheck, config and secret values are omitted"
            .into(),
    );
    let metadata = metadata_json(root, services.len());
    Ok((table(rows), metadata, warnings))
}

fn json_row(name: &str, definition: &Value) -> Vec<String> {
    let object = definition.as_object();
    let image = object
        .and_then(|v| v.get("image"))
        .and_then(Value::as_str)
        .map_or_else(
            || {
                if object.is_some_and(|v| v.contains_key("build")) {
                    "build".to_owned()
                } else {
                    "—".to_owned()
                }
            },
            truncate,
        );
    let build = object.is_some_and(|v| v.contains_key("build"));
    let image_build = if build && image != "build" {
        format!("{image} + build")
    } else {
        image
    };
    let ports = object.map_or(0, |v| count_attr(v.get("ports")));
    let depends = object.map_or(0, |v| count_attr(v.get("depends_on")));
    let volumes = object.map_or(0, |v| count_attr(v.get("volumes")));
    let networks = object.map_or(0, |v| count_attr(v.get("networks")));
    let secrets = object.map_or(0, |v| count_attr(v.get("secrets")));
    let runtime = object.map_or("—", |v| {
        if v.contains_key("command") {
            "command"
        } else {
            "—"
        }
    });
    vec![
        truncate(name),
        truncate(&image_build),
        ports.to_string(),
        depends.to_string(),
        volumes.to_string(),
        format!(
            "net:{networks} sec:{secrets}{}",
            if runtime == "command" { " cmd" } else { "" }
        ),
    ]
}

fn metadata_json(root: &serde_json::Map<String, Value>, services: usize) -> String {
    let count = |key: &str| root.get(key).map_or(0, |value| count_attr(Some(value)));
    let mut parts = vec![
        format!("Services: {services}"),
        format!("Networks: {}", count("networks")),
        format!("Volumes: {}", count("volumes")),
        format!("Secrets: {}", count("secrets")),
        format!("Configs: {}", count("configs")),
    ];
    if let Some(name) = root.get("name").and_then(Value::as_str) {
        parts.insert(0, format!("Name: {}", truncate(name)));
    }
    if let Some(version) = root.get("version").and_then(Value::as_str) {
        parts.push(format!("Version: {}", truncate(version)));
    }
    parts.join("\n")
}

fn parse_yaml(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_COMPOSE_YAML_BYTES {
        return Err(Error::LimitExceeded(format!(
            "Docker Compose YAML exceeds {MAX_COMPOSE_YAML_BYTES} bytes"
        )));
    }
    let (_, mut warnings) = crate::document::yaml::parse_yaml_blocks(text)?;
    let mut parser = ComposeYamlParser::default();
    for (line_number, raw) in text.lines().enumerate() {
        if line_number >= MAX_COMPOSE_LINES {
            return Err(Error::LimitExceeded(format!(
                "Docker Compose YAML exceeds {MAX_COMPOSE_LINES} lines"
            )));
        }
        if raw.len() > MAX_COMPOSE_LINE_BYTES {
            return Err(Error::LimitExceeded(format!(
                "Docker Compose YAML line {} exceeds {MAX_COMPOSE_LINE_BYTES} bytes",
                line_number + 1
            )));
        }
        parser.line(raw)?;
    }
    parser.finish()?;
    if parser.rows.is_empty() {
        return Err(Error::InvalidInput(
            "Docker Compose YAML requires a non-empty services map".into(),
        ));
    }
    warnings.extend(base_warnings());
    warnings.push(
        "Compose YAML command, environment, healthcheck, config and secret values are omitted"
            .into(),
    );
    let metadata = format!(
        "Services: {}\nNetworks: {}\nVolumes: {}\nSecrets: {}\nConfigs: {}",
        parser.rows.len(),
        parser.networks,
        parser.volumes,
        parser.secrets,
        parser.configs
    );
    Ok((table(parser.rows), metadata, warnings))
}

#[derive(Default)]
struct ComposeYamlParser {
    in_services: bool,
    top_section: Option<String>,
    current_field: Option<String>,
    current: Option<ComposeService>,
    rows: Vec<Vec<String>>,
    networks: usize,
    volumes: usize,
    secrets: usize,
    configs: usize,
}

#[derive(Default)]
struct ComposeService {
    name: String,
    image: String,
    build: bool,
    ports: usize,
    depends: usize,
    volumes: usize,
    networks: usize,
    secrets: usize,
    command: bool,
}

impl ComposeYamlParser {
    fn line(&mut self, raw: &str) -> Result<()> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Ok(());
        }
        if trimmed == "---" || trimmed == "..." {
            self.flush_service()?;
            self.in_services = false;
            self.top_section = None;
            self.current_field = None;
            return Ok(());
        }
        let indent = raw.len() - raw.trim_start().len();
        if indent == 0 {
            self.flush_service()?;
            self.current_field = None;
            let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
                self.in_services = false;
                self.top_section = None;
                return Ok(());
            };
            let key = clean_key(raw_key);
            let value = scalar(raw_value);
            self.in_services = key == "services";
            self.top_section = match key.as_str() {
                "networks" | "volumes" | "secrets" | "configs" => Some(key),
                _ => None,
            };
            if !self.in_services {
                self.count_inline_resource(&self.top_section.clone(), &value);
            }
            return Ok(());
        }
        if self.in_services {
            if indent == 2 {
                self.flush_service()?;
                let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
                    self.current_field = None;
                    return Ok(());
                };
                let key = clean_key(raw_key);
                if key.is_empty() {
                    return Ok(());
                }
                self.current = Some(ComposeService {
                    name: key,
                    ..ComposeService::default()
                });
                self.current_field = None;
                let value = scalar(raw_value);
                if !value.is_empty() {
                    self.current_field = Some("service_inline".into());
                }
                return Ok(());
            }
            if indent == 4 {
                let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
                    return Ok(());
                };
                let key = clean_key(raw_key);
                self.current_field = Some(key.clone());
                if let Some(service) = self.current.as_mut() {
                    let value = scalar(raw_value);
                    match key.as_str() {
                        "image" => service.image = truncate(&value),
                        "build" => service.build = true,
                        "command" => service.command = true,
                        "ports" => service.ports += inline_count(&value),
                        "depends_on" => service.depends += inline_count(&value),
                        "volumes" => service.volumes += inline_count(&value),
                        "networks" => service.networks += inline_count(&value),
                        "secrets" => service.secrets += inline_count(&value),
                        _ => {}
                    }
                }
                return Ok(());
            }
            if indent >= 6
                && let Some(field) = self.current_field.as_deref()
                && let Some(service) = self.current.as_mut()
                && indent == 6
                && (trimmed.starts_with('-') || trimmed.contains(':'))
            {
                match field {
                    "ports" => service.ports = service.ports.saturating_add(1),
                    "depends_on" => service.depends = service.depends.saturating_add(1),
                    "volumes" => service.volumes = service.volumes.saturating_add(1),
                    "networks" => service.networks = service.networks.saturating_add(1),
                    "secrets" => service.secrets = service.secrets.saturating_add(1),
                    _ => {}
                }
            }
            return Ok(());
        }
        if let Some(section) = self.top_section.as_deref()
            && indent == 2
            && trimmed.split_once(':').is_some()
        {
            match section {
                "networks" => self.networks = self.networks.saturating_add(1),
                "volumes" => self.volumes = self.volumes.saturating_add(1),
                "secrets" => self.secrets = self.secrets.saturating_add(1),
                "configs" => self.configs = self.configs.saturating_add(1),
                _ => {}
            }
        }
        Ok(())
    }

    fn count_inline_resource(&mut self, section: &Option<String>, value: &str) {
        let count = inline_count(value);
        match section.as_deref() {
            Some("networks") => self.networks = self.networks.saturating_add(count),
            Some("volumes") => self.volumes = self.volumes.saturating_add(count),
            Some("secrets") => self.secrets = self.secrets.saturating_add(count),
            Some("configs") => self.configs = self.configs.saturating_add(count),
            _ => {}
        }
    }

    fn flush_service(&mut self) -> Result<()> {
        let Some(service) = self.current.take() else {
            return Ok(());
        };
        if self.rows.len() >= MAX_COMPOSE_SERVICES {
            return Err(Error::LimitExceeded(format!(
                "Docker Compose services exceed {MAX_COMPOSE_SERVICES}"
            )));
        }
        let image = if service.image.is_empty() {
            if service.build {
                "build".into()
            } else {
                "—".into()
            }
        } else if service.build {
            format!("{} + build", service.image)
        } else {
            service.image
        };
        self.rows.push(vec![
            truncate(&service.name),
            truncate(&image),
            service.ports.to_string(),
            service.depends.to_string(),
            service.volumes.to_string(),
            format!(
                "net:{} sec:{}{}",
                service.networks,
                service.secrets,
                if service.command { " cmd" } else { "" }
            ),
        ]);
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.flush_service()
    }
}

fn table(rows: Vec<Vec<String>>) -> TableData {
    TableData {
        headers: vec![
            "Service".into(),
            "Image/build".into(),
            "Ports".into(),
            "Depends".into(),
            "Volumes".into(),
            "Net/sec/cmd".into(),
        ],
        rows,
        alignments: vec![TableAlign::Left; 6],
        raw_source: String::new(),
    }
}

fn base_warnings() -> Vec<String> {
    vec!["Docker Compose services and resource references are rendered inertly; Docker daemon access, image pull/build, command, healthcheck, network, volume, config and secret operations are never executed".into(), "Compose interpolation, include and merge semantics are not evaluated; secret and environment values remain omitted".into()]
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

fn count_attr(value: Option<&Value>) -> usize {
    match value {
        Some(Value::Array(values)) => values.len(),
        Some(Value::Object(values)) => values.len(),
        Some(Value::String(value)) if !value.trim().is_empty() => 1,
        _ => 0,
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_COMPOSE_STRING_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_COMPOSE_STRING_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_COMPOSE_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "Docker Compose JSON nesting exceeds {MAX_COMPOSE_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_COMPOSE_VALUES {
        return Err(Error::LimitExceeded(format!(
            "Docker Compose JSON contains more than {MAX_COMPOSE_VALUES} values"
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
        Value::String(value) if value.len() > MAX_COMPOSE_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "Docker Compose JSON string exceeds {MAX_COMPOSE_STRING_BYTES} bytes"
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
                if depth > MAX_COMPOSE_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "Docker Compose JSON nesting exceeds {MAX_COMPOSE_DEPTH} levels"
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
    fn recognizes_compose_yaml_but_not_kubernetes() {
        assert!(looks_like_yaml_prefix(
            b"services:\n  web:\n    image: nginx\n"
        ));
        assert!(!looks_like_yaml_prefix(
            b"apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: x\n"
        ));
    }

    #[test]
    fn counts_json_services_without_secret_values() {
        let (table, metadata, warnings) = parse_json(
            r#"{"name":"demo","services":{"web":{"image":"nginx","ports":["80:80"],"secrets":["token"],"command":"echo secret"}},"secrets":{"token":{"file":"secret.txt"}}}"#,
        )
        .unwrap();
        assert!(metadata.contains("Services: 1"));
        assert_eq!(table.rows[0][0], "web");
        assert_eq!(table.rows[0][5], "net:0 sec:1 cmd");
        assert!(!table.rows[0].iter().any(|value| value.contains("secret")));
        assert!(warnings.iter().any(|warning| warning.contains("omitted")));
    }
}
