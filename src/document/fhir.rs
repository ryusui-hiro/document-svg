//! Bounded HL7 FHIR R4 JSON previews.
//!
//! FHIR resources can contain clinical narratives, identifiers, names, codes and
//! references. This adapter validates the resource envelope and renders only
//! inert structural metadata; patient content, narrative XHTML, identifiers,
//! extension values and references are counted or omitted and never resolved.

use serde_json::Value;
use std::path::Path;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_FHIR_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FHIR_DEPTH: usize = 100;
const MAX_FHIR_VALUES: usize = 300_000;
const MAX_FHIR_RESOURCES: usize = 100_000;
const MAX_FHIR_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_FHIR_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) {
        return object
            .get("resourceType")
            .and_then(Value::as_str)
            .is_some_and(|value| {
                !value.is_empty() && value.chars().next().is_some_and(char::is_uppercase)
            });
    }
    text.contains("\"resourceType\"")
}

struct FhirPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for FhirPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "fhir-json".into();
        if page.title.is_empty() {
            page.title = "FHIR JSON".into();
        }
        page.description =
            "FHIR JSON resource structure is rendered inertly; clinical values, narrative XHTML, identifiers, extensions and references are not displayed or resolved".into();
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
        options.max_input_bytes.min(MAX_FHIR_BYTES),
        "FHIR JSON input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("FHIR JSON must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "FHIR JSON".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = FhirPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_FHIR_BYTES {
        return Err(Error::LimitExceeded(format!(
            "FHIR JSON exceeds {MAX_FHIR_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid FHIR JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("FHIR JSON root must be a resource object".into()))?;
    let root_type = required_string(root, "resourceType", 0)?;
    if !root_type.chars().next().is_some_and(char::is_uppercase) {
        return Err(Error::InvalidInput(format!(
            "FHIR resourceType '{root_type}' must start with an uppercase letter"
        )));
    }

    let mut warnings = Vec::new();
    let mut rows = Vec::new();
    let mut bundle_type = None;
    let mut declared_total = None;
    let mut skipped_entries = 0usize;
    if root_type == "Bundle" {
        bundle_type = Some(required_string(root, "type", 0)?.to_owned());
        declared_total = root
            .get("total")
            .map(|value| {
                value.as_u64().ok_or_else(|| {
                    Error::InvalidInput("FHIR Bundle total must be an unsigned integer".into())
                })
            })
            .transpose()?;
        let entries = match root.get("entry") {
            None => &[] as &[Value],
            Some(value) => value
                .as_array()
                .ok_or_else(|| Error::InvalidInput("FHIR Bundle entry must be an array".into()))?,
        };
        if entries.len() > MAX_FHIR_RESOURCES {
            return Err(Error::LimitExceeded(format!(
                "FHIR Bundle entries exceed {MAX_FHIR_RESOURCES}"
            )));
        }
        for (index, entry) in entries.iter().enumerate() {
            let Some(entry_object) = entry.as_object() else {
                skipped_entries = skipped_entries.saturating_add(1);
                continue;
            };
            let Some(resource) = entry_object.get("resource") else {
                skipped_entries = skipped_entries.saturating_add(1);
                continue;
            };
            let Some(resource_object) = resource.as_object() else {
                return Err(Error::InvalidInput(format!(
                    "FHIR Bundle entry {} resource must be an object",
                    index + 1
                )));
            };
            rows.push(resource_row(resource_object, index + 1)?);
        }
        if rows.is_empty() {
            rows.push(resource_row(root, 0)?);
        }
    } else {
        rows.push(resource_row(root, 0)?);
    }
    if skipped_entries > 0 {
        warnings.push(format!(
            "{skipped_entries} FHIR Bundle entr{} without a usable resource were omitted",
            if skipped_entries == 1 { "y" } else { "ies" }
        ));
    }
    let mut metadata = format!(
        "Root: {root_type}\nResources: {}\nBundle type: {}\nDeclared total: {}",
        rows.len(),
        bundle_type.as_deref().unwrap_or("—"),
        declared_total.map_or_else(|| "—".into(), |value| value.to_string())
    );
    warnings.push("FHIR clinical values, Narrative.div XHTML, names, addresses, telecom, identifiers, coded displays, extension values and resource references are omitted; no validation, terminology lookup, URL fetch or clinical operation is executed".into());
    warnings.push("FHIR JSON is parsed as UTF-8 with bounded depth, value, resource and string limits; unknown properties are retained only for structural counting".into());
    if let Some(profile_count) = rows
        .iter()
        .map(|row| row[3].parse::<usize>().unwrap_or(0))
        .max()
    {
        metadata.push_str(&format!("\nMax profiles/resource: {profile_count}"));
    }
    Ok((
        TableData {
            headers: vec![
                "Resource".into(),
                "ID".into(),
                "Status/type".into(),
                "Profiles".into(),
                "Structure".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 5],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn resource_row(object: &serde_json::Map<String, Value>, index: usize) -> Result<Vec<String>> {
    let resource_type = required_string(object, "resourceType", index)?;
    let id = if let Some(value) = object.get("id") {
        let id = value.as_str().ok_or_else(|| {
            Error::InvalidInput(format!(
                "FHIR resource {} id must be a string",
                index.max(1)
            ))
        })?;
        if id.is_empty() {
            return Err(Error::InvalidInput(format!(
                "FHIR resource {} id must not be empty",
                index.max(1)
            )));
        }
        id
    } else {
        "—"
    };
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("type")
                .and_then(Value::as_object)
                .and_then(|type_object| type_object.get("code"))
                .and_then(Value::as_str)
        })
        .map_or_else(|| "—".into(), truncate);
    let profiles = object
        .get("meta")
        .and_then(Value::as_object)
        .and_then(|meta| meta.get("profile"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let subject = if object.contains_key("subject") {
        "subject"
    } else {
        "—"
    };
    let narrative = if object
        .get("text")
        .and_then(Value::as_object)
        .and_then(|text| text.get("div"))
        .is_some()
    {
        "narrative"
    } else {
        "—"
    };
    let contained = object
        .get("contained")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let extensions = object
        .get("extension")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    let elements = object.len().saturating_sub(1);
    Ok(vec![
        truncate(resource_type),
        truncate(id),
        status,
        profiles.to_string(),
        format!(
            "{elements} fields · {subject} · {narrative} · contained {contained} · ext {extensions}"
        ),
    ])
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
    index: usize,
) -> Result<&'a str> {
    let value = object.get(key).and_then(Value::as_str).ok_or_else(|| {
        Error::InvalidInput(format!(
            "FHIR resource {} requires string {key}",
            index.max(1)
        ))
    })?;
    if value.is_empty() {
        return Err(Error::InvalidInput(format!(
            "FHIR resource {} {key} must not be empty",
            index.max(1)
        )));
    }
    Ok(value)
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_FHIR_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_FHIR_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_FHIR_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "FHIR JSON nesting exceeds {MAX_FHIR_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_FHIR_VALUES {
        return Err(Error::LimitExceeded(format!(
            "FHIR JSON contains more than {MAX_FHIR_VALUES} values"
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
        Value::String(value) if value.len() > MAX_FHIR_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "FHIR JSON string exceeds {MAX_FHIR_STRING_BYTES} bytes"
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
                if depth > MAX_FHIR_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "FHIR JSON nesting exceeds {MAX_FHIR_DEPTH} levels"
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
    fn recognizes_resource_root_without_confusing_nested_json() {
        assert!(looks_like_prefix(
            br#"{"resourceType":"Patient","id":"p1","meta":{"versionId":"1"}}"#
        ));
        assert!(looks_like_prefix(
            br#"{"resourceType":"Bundle","type":"collection","entry":[]}"#
        ));
        assert!(!looks_like_prefix(
            br#"{"wrapper":{"resourceType":"Patient","id":"p1"}}"#
        ));
    }

    #[test]
    fn summarizes_bundle_without_clinical_values() {
        let (table, metadata, warnings) = parse(
            r#"{"resourceType":"Bundle","type":"collection","total":1,"entry":[{"resource":{"resourceType":"Patient","id":"p1","text":{"status":"generated","div":"<div>secret patient</div>"},"name":[{"family":"Secret"}],"identifier":[{"value":"private"}],"meta":{"profile":["https://profile.invalid/patient"]}}}]}"#,
        )
        .unwrap();
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0][0], "Patient");
        assert_eq!(table.rows[0][1], "p1");
        assert!(table.rows[0][4].contains("narrative"));
        assert!(metadata.contains("Bundle type: collection"));
        assert!(
            !table
                .rows
                .iter()
                .flatten()
                .any(|value| value.contains("secret")
                    || value.contains("private")
                    || value.contains("profile.invalid"))
        );
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("clinical values"))
        );
    }

    #[test]
    fn rejects_non_string_resource_id() {
        let result = parse(r#"{"resourceType":"Patient","id":7}"#);
        assert!(result.is_err());
    }
}
