//! Bounded OGC CityJSON 1.x/2.0 3D city-model previews.
//!
//! CityJSON stores CityGML-derived objects with shared integer vertices and an
//! optional scale/translate transform. This adapter validates object geometry
//! indices and renders thematic/LoD counts only; coordinate arrays, textures,
//! attributes, extensions and external resources are never expanded or fetched.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_CITYJSON_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CITYJSON_DEPTH: usize = 100;
const MAX_CITYJSON_VALUES: usize = 300_000;
const MAX_CITYJSON_OBJECTS: usize = 100_000;
const MAX_CITYJSON_VERTICES: usize = 500_000;
const MAX_CITYJSON_GEOMETRIES: usize = 200_000;
const MAX_CITYJSON_ROWS: usize = 200_000;
const MAX_CITYJSON_STRING_BYTES: usize = 2 * 1024 * 1024;
const MAX_CITYJSON_DISPLAY_BYTES: usize = 256;

pub(crate) fn looks_like_prefix(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(trimmed) {
        return object.get("type").and_then(Value::as_str) == Some("CityJSON")
            && object
                .get("CityObjects")
                .and_then(Value::as_object)
                .is_some();
    }
    text.contains("\"CityObjects\"") && text.contains("\"type\"")
}

struct CityJsonPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for CityJsonPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "cityjson".into();
        if page.title.is_empty() {
            page.title = "OGC CityJSON".into();
        }
        page.description =
            "CityJSON object and geometry metadata is rendered inertly; coordinate payloads, textures, attributes and external resources are not expanded or resolved".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Counts {
    objects: usize,
    geometries: usize,
    lod0: usize,
    lod1: usize,
    lod2: usize,
    lod3: usize,
    lod4: usize,
    lod_other: usize,
}

#[derive(Default)]
struct Summary {
    by_type: BTreeMap<String, Counts>,
    objects: usize,
    geometries: usize,
    boundaries: usize,
    referenced_vertices: usize,
    vertices: usize,
    metadata: bool,
    transform: bool,
    extensions: usize,
    invalid_indices: usize,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_CITYJSON_BYTES),
        "CityJSON input",
    )?;
    let text = String::from_utf8(bytes)
        .map_err(|error| Error::InvalidInput(format!("CityJSON must be UTF-8 JSON: {error}")))?;
    let (table, metadata, warnings) = parse(&text)?;
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "OGC CityJSON".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(table),
    ];
    let mut page_sink = CityJsonPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn parse(text: &str) -> Result<(TableData, String, Vec<String>)> {
    if text.len() as u64 > MAX_CITYJSON_BYTES {
        return Err(Error::LimitExceeded(format!(
            "CityJSON exceeds {MAX_CITYJSON_BYTES} bytes"
        )));
    }
    preflight_depth(text)?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| Error::InvalidInput(format!("invalid CityJSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("CityJSON root must be an object".into()))?;
    if root.get("type").and_then(Value::as_str) != Some("CityJSON") {
        return Err(Error::InvalidInput(
            "CityJSON type must be the string CityJSON".into(),
        ));
    }
    let version = root
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput("CityJSON requires a version string".into()))?;
    if version.is_empty() {
        return Err(Error::InvalidInput(
            "CityJSON version must not be empty".into(),
        ));
    }
    let city_objects = root
        .get("CityObjects")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::InvalidInput("CityJSON requires a CityObjects object".into()))?;
    if city_objects.len() > MAX_CITYJSON_OBJECTS {
        return Err(Error::LimitExceeded(format!(
            "CityJSON objects exceed {MAX_CITYJSON_OBJECTS}"
        )));
    }
    let vertices = root
        .get("vertices")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidInput("CityJSON requires a vertices array".into()))?;
    if vertices.len() > MAX_CITYJSON_VERTICES {
        return Err(Error::LimitExceeded(format!(
            "CityJSON vertices exceed {MAX_CITYJSON_VERTICES}"
        )));
    }
    for vertex in vertices {
        let coordinates = vertex
            .as_array()
            .ok_or_else(|| Error::InvalidInput("CityJSON vertex must be an array".into()))?;
        if coordinates.len() != 3 || !coordinates.iter().all(Value::is_number) {
            return Err(Error::InvalidInput(
                "CityJSON vertices must contain three numeric coordinates".into(),
            ));
        }
    }
    let mut summary = Summary {
        vertices: vertices.len(),
        metadata: root.contains_key("metadata"),
        transform: root.contains_key("transform"),
        extensions: root.get("extensions").map_or(0, collection_len),
        ..Summary::default()
    };
    validate_transform(root.get("transform"))?;
    let mut warnings = Vec::new();
    for (id, city_object) in city_objects {
        let object = city_object.as_object().ok_or_else(|| {
            Error::InvalidInput(format!("CityJSON CityObject '{id}' must be an object"))
        })?;
        let object_type = object.get("type").and_then(Value::as_str).ok_or_else(|| {
            Error::InvalidInput(format!("CityJSON CityObject '{id}' requires type"))
        })?;
        let entry = summary.by_type.entry(object_type.to_owned()).or_default();
        entry.objects = entry.objects.saturating_add(1);
        summary.objects = summary.objects.saturating_add(1);
        if let Some(geometry) = object.get("geometry") {
            let geometry = geometry.as_array().ok_or_else(|| {
                Error::InvalidInput(format!(
                    "CityJSON CityObject '{id}' geometry must be an array"
                ))
            })?;
            for item in geometry {
                let geometry_object = item.as_object().ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "CityJSON CityObject '{id}' geometry item must be an object"
                    ))
                })?;
                summary.geometries = summary.geometries.saturating_add(1);
                if summary.geometries > MAX_CITYJSON_GEOMETRIES {
                    return Err(Error::LimitExceeded(format!(
                        "CityJSON geometries exceed {MAX_CITYJSON_GEOMETRIES}"
                    )));
                }
                entry.geometries = entry.geometries.saturating_add(1);
                match geometry_object.get("lod").and_then(Value::as_str) {
                    Some("0") => entry.lod0 = entry.lod0.saturating_add(1),
                    Some("1") => entry.lod1 = entry.lod1.saturating_add(1),
                    Some("2") => entry.lod2 = entry.lod2.saturating_add(1),
                    Some("3") => entry.lod3 = entry.lod3.saturating_add(1),
                    Some("4") => entry.lod4 = entry.lod4.saturating_add(1),
                    Some(_) => entry.lod_other = entry.lod_other.saturating_add(1),
                    None => warnings.push(format!(
                        "CityJSON CityObject '{id}' geometry has no lod string"
                    )),
                }
                if let Some(boundaries) = geometry_object.get("boundaries") {
                    summary.boundaries = summary.boundaries.saturating_add(1);
                    collect_indices(
                        boundaries,
                        0,
                        vertices.len(),
                        &mut summary.referenced_vertices,
                        &mut summary.invalid_indices,
                    )?;
                }
            }
        }
    }
    let mut rows = vec![vec![
        "CityJSON".into(),
        summary.objects.to_string(),
        summary.geometries.to_string(),
        format!("{} types", summary.by_type.len()),
    ]];
    for (kind, counts) in &summary.by_type {
        if rows.len() >= MAX_CITYJSON_ROWS {
            return Err(Error::LimitExceeded(format!(
                "CityJSON rows exceed {MAX_CITYJSON_ROWS}"
            )));
        }
        rows.push(vec![
            truncate(kind),
            counts.objects.to_string(),
            counts.geometries.to_string(),
            format!(
                "lod0 {} · lod1 {} · lod2 {} · lod3 {} · lod4 {} · other {}",
                counts.lod0, counts.lod1, counts.lod2, counts.lod3, counts.lod4, counts.lod_other
            ),
        ]);
    }
    if summary.invalid_indices > 0 {
        warnings.push(format!(
            "{} CityJSON boundary vertex index reference(s) were invalid and omitted from counts",
            summary.invalid_indices
        ));
    }
    if !warnings.is_empty() {
        warnings.push("Some CityJSON geometry metadata was incomplete; missing LoD labels remain inert and no geometry was expanded".into());
    }
    warnings.push("CityJSON coordinate arrays, transform values, geometry boundaries, semantics, materials, textures, attributes, extensions and external resources remain inert; no 3D rendering or CRS transformation runs".into());
    warnings.push("CityJSON object/geometry/vertex traversal is bounded; vertex indices are checked without allocating unbounded geometry structures".into());
    let metadata = format!(
        "Version: {}\nObjects: {}\nGeometries: {}\nVertices: {}\nBoundary arrays: {}\nReferenced vertex indices: {}\nTransform: {}\nMetadata: {}\nExtensions: {}",
        truncate(version),
        summary.objects,
        summary.geometries,
        summary.vertices,
        summary.boundaries,
        summary.referenced_vertices,
        if summary.transform {
            "present"
        } else {
            "missing"
        },
        if summary.metadata {
            "present"
        } else {
            "missing"
        },
        summary.extensions
    );
    Ok((
        TableData {
            headers: vec![
                "Kind".into(),
                "Objects".into(),
                "Geometry".into(),
                "LoD breakdown".into(),
            ],
            rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        },
        metadata,
        warnings,
    ))
}

fn validate_transform(value: Option<&Value>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let object = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("CityJSON transform must be an object".into()))?;
    for key in ["scale", "translate"] {
        let values = object.get(key).and_then(Value::as_array).ok_or_else(|| {
            Error::InvalidInput(format!("CityJSON transform requires {key} array"))
        })?;
        if values.len() != 3 || !values.iter().all(Value::is_number) {
            return Err(Error::InvalidInput(format!(
                "CityJSON transform {key} must contain three numeric values"
            )));
        }
    }
    Ok(())
}

fn collect_indices(
    value: &Value,
    depth: usize,
    vertex_count: usize,
    referenced: &mut usize,
    invalid: &mut usize,
) -> Result<()> {
    if depth > MAX_CITYJSON_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "CityJSON boundaries nesting exceeds {MAX_CITYJSON_DEPTH} levels"
        )));
    }
    match value {
        Value::Array(values) => {
            for item in values {
                collect_indices(item, depth + 1, vertex_count, referenced, invalid)?;
            }
        }
        Value::Number(number) => {
            if let Some(index) = number.as_u64() {
                if index < vertex_count as u64 {
                    *referenced = referenced.saturating_add(1);
                } else {
                    *invalid = invalid.saturating_add(1);
                }
            } else {
                *invalid = invalid.saturating_add(1);
            }
        }
        _ => {
            *invalid = invalid.saturating_add(1);
        }
    }
    Ok(())
}

fn collection_len(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.len(),
        Value::Object(map) => map.len(),
        _ => 0,
    }
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_CITYJSON_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_CITYJSON_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_CITYJSON_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "CityJSON nesting exceeds {MAX_CITYJSON_DEPTH} levels"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_CITYJSON_VALUES {
        return Err(Error::LimitExceeded(format!(
            "CityJSON contains more than {MAX_CITYJSON_VALUES} values"
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
        Value::String(value) if value.len() > MAX_CITYJSON_STRING_BYTES => {
            return Err(Error::LimitExceeded(format!(
                "CityJSON string exceeds {MAX_CITYJSON_STRING_BYTES} bytes"
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
                if depth > MAX_CITYJSON_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "CityJSON nesting exceeds {MAX_CITYJSON_DEPTH} levels"
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
    fn recognizes_cityjson_root() {
        assert!(looks_like_prefix(
            br#"{"type":"CityJSON","version":"2.0","CityObjects":{},"vertices":[]}"#
        ));
        assert!(!looks_like_prefix(br#"{"type":"FeatureCollection"}"#));
    }

    #[test]
    fn summarizes_city_objects_and_checks_boundary_indices() {
        let (table, metadata, warnings) = parse(
            r#"{"type":"CityJSON","version":"2.0","transform":{"scale":[0.01,0.01,0.01],"translate":[1,2,3]},"CityObjects":{"b":{"type":"Building","geometry":[{"type":"Solid","lod":"2","boundaries":[[[0,1,2]]]}]},"r":{"type":"Road","geometry":[{"type":"MultiSurface","lod":"0","boundaries":[[0,3,9]]}]}},"vertices":[[0,0,0],[1,0,0],[0,1,0]],"metadata":{"referenceSystem":"private"}}"#,
        ).unwrap();
        assert_eq!(table.rows.len(), 3);
        assert!(metadata.contains("Objects: 2"));
        assert!(metadata.contains("Transform: present"));
        assert!(warnings.iter().any(|warning| warning.contains("invalid")));
    }
}
