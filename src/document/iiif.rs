//! Bounded IIIF Presentation 2.1/3.0 manifest previews.
//!
//! IIIF manifests describe ordered Canvases and Web Annotations that paint
//! images, text or other media. This adapter renders labels, dimensions and
//! structural counts only; manifest IDs, image services and external targets
//! are never dereferenced.

use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::document::html::{HtmlBlock, render_blocks_to_pages};
use crate::error::{Error, Result};
use crate::table::{TableAlign, TableData};

const MAX_IIIF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IIIF_DEPTH: usize = 100;
const MAX_IIIF_VALUES: usize = 300_000;
const MAX_IIIF_ROWS: usize = 200_000;
const MAX_IIIF_CANVASES: usize = 100_000;
const MAX_IIIF_ANNOTATIONS: usize = 300_000;
const MAX_IIIF_DISPLAY_BYTES: usize = 512;

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let lower = text.to_ascii_lowercase();
    if !lower.trim_start().starts_with('{') {
        return false;
    }
    let compact: String = lower
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let context = compact.contains("iiif.io/api/presentation");
    let typed = compact.contains("\"type\":\"manifest\"")
        || compact.contains("\"type\":\"collection\"")
        || compact.contains("\"@type\":\"sc:manifest\"")
        || compact.contains("\"@type\":\"sc:collection\"");
    context && typed
}

struct IiifPageSink<'a> {
    inner: &'a mut dyn PageConsumer,
    warnings: &'a [String],
}

impl PageConsumer for IiifPageSink<'_> {
    fn consume(&mut self, mut page: crate::ir::Page) -> Result<()> {
        page.source_format = "iiif".into();
        if page.title.is_empty() {
            page.title = "IIIF Presentation manifest".into();
        }
        page.description =
            "IIIF manifest and Canvas metadata is rendered inertly; IDs, image services and external resources are not resolved or fetched".into();
        for warning in self.warnings {
            page.warn(warning.clone());
        }
        self.inner.consume(page)
    }
}

#[derive(Default)]
struct Summary {
    version: String,
    resource_type: String,
    label: String,
    canvases: usize,
    images: usize,
    annotations: usize,
    ranges: usize,
    metadata: usize,
    rows: Vec<Vec<String>>,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_IIIF_BYTES),
        "IIIF input",
    )?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Error::InvalidInput(format!("invalid IIIF JSON: {error}")))?;
    let mut values = 0usize;
    count_values(&value, 0, &mut values)?;
    let root = value
        .as_object()
        .ok_or_else(|| Error::InvalidInput("IIIF root must be a JSON object".into()))?;
    let resource_type = root
        .get("type")
        .or_else(|| root.get("@type"))
        .and_then(Value::as_str)
        .unwrap_or("Manifest")
        .to_owned();
    if !matches!(
        resource_type.as_str(),
        "Manifest" | "Collection" | "sc:Manifest" | "sc:Collection"
    ) {
        return Err(Error::Unsupported(format!(
            "IIIF resource type '{resource_type}' is unsupported"
        )));
    }
    let version = detect_version(root);
    let mut summary = Summary {
        version,
        resource_type: resource_type.clone(),
        label: label_value(root.get("label"), "—"),
        metadata: root
            .get("metadata")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
        ..Summary::default()
    };
    summary.ranges = root
        .get("structures")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if let Some(structures) = root.get("structures").and_then(Value::as_array) {
        for (index, range) in structures.iter().enumerate() {
            let range_label = label_value(range.get("label"), "—");
            push_row(
                &mut summary,
                "Range",
                &(index + 1).to_string(),
                &range_label,
                "range structure".into(),
            )?;
        }
    }
    if resource_type.eq_ignore_ascii_case("collection") || resource_type == "sc:Collection" {
        collect_collection(root, &mut summary)?;
    } else if summary.version == "3" {
        collect_manifest_v3(root, &mut summary)?;
    } else {
        collect_manifest_v2(root, &mut summary)?;
    }
    if summary.rows.is_empty() {
        summary.rows.push(vec![
            "Manifest".into(),
            "—".into(),
            display_or_dash(&summary.label).into(),
            "no embedded Canvas items".into(),
        ]);
    }
    let metadata = format!(
        "Presentation API: {}\nResource type: {}\nLabel: {}\nCanvases: {}\nPainted images: {}\nAnnotations: {}\nRanges: {}\nMetadata entries: {}",
        summary.version,
        summary.resource_type,
        display_or_dash(&summary.label),
        summary.canvases,
        summary.images,
        summary.annotations,
        summary.ranges,
        summary.metadata,
    );
    let blocks = vec![
        HtmlBlock::Heading {
            level: 1,
            text: "IIIF Presentation manifest".into(),
        },
        HtmlBlock::Paragraph { text: metadata },
        HtmlBlock::Table(TableData {
            headers: vec!["Kind".into(), "No.".into(), "Label".into(), "Detail".into()],
            rows: summary.rows,
            alignments: vec![TableAlign::Left; 4],
            raw_source: String::new(),
        }),
    ];
    let warnings = vec![
        "IIIF labels, Canvas dimensions and structural counts are shown; IDs, service URLs, thumbnails, image bodies, metadata values and external annotations are omitted".into(),
        "IIIF JSON traversal and rendered rows are bounded; no manifest, image service, OCR resource, script or network operation runs".into(),
    ];
    let mut page_sink = IiifPageSink {
        inner: sink,
        warnings: &warnings,
    };
    render_blocks_to_pages(&blocks, &mut page_sink, options)?;
    Ok(warnings)
}

fn collect_manifest_v3(root: &serde_json::Map<String, Value>, summary: &mut Summary) -> Result<()> {
    let items = array(root.get("items"));
    for (index, canvas) in items.iter().enumerate() {
        collect_canvas_v3(canvas, index + 1, summary)?;
    }
    Ok(())
}

fn collect_canvas_v3(canvas: &Value, index: usize, summary: &mut Summary) -> Result<()> {
    let object = canvas
        .as_object()
        .ok_or_else(|| Error::InvalidInput("IIIF Canvas must be an object".into()))?;
    summary.canvases = summary.canvases.saturating_add(1);
    if summary.canvases > MAX_IIIF_CANVASES {
        return Err(Error::LimitExceeded(format!(
            "IIIF canvases exceed {MAX_IIIF_CANVASES}"
        )));
    }
    let label = label_value(object.get("label"), "—");
    let width = object.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = object.get("height").and_then(Value::as_u64).unwrap_or(0);
    let annotation_pages = array(object.get("items"));
    let mut image_count = 0usize;
    let mut annotation_count = 0usize;
    for page in annotation_pages {
        let page_items = array(page.get("items"));
        annotation_count = annotation_count.saturating_add(page_items.len());
        for annotation in page_items {
            if annotation.get("motivation").and_then(Value::as_str) == Some("painting")
                && annotation
                    .get("body")
                    .and_then(Value::as_object)
                    .and_then(|body| body.get("type"))
                    .and_then(Value::as_str)
                    == Some("Image")
            {
                image_count = image_count.saturating_add(1);
            }
        }
    }
    summary.images = summary.images.saturating_add(image_count);
    summary.annotations = summary.annotations.saturating_add(annotation_count);
    if summary.annotations > MAX_IIIF_ANNOTATIONS {
        return Err(Error::LimitExceeded(format!(
            "IIIF annotations exceed {MAX_IIIF_ANNOTATIONS}"
        )));
    }
    push_row(
        summary,
        "Canvas",
        &index.to_string(),
        &label,
        format!(
            "{}×{}, images {image_count}, annotations {annotation_count}",
            width, height
        ),
    )?;
    Ok(())
}

fn collect_manifest_v2(root: &serde_json::Map<String, Value>, summary: &mut Summary) -> Result<()> {
    let sequences = array(root.get("sequences"));
    for sequence in sequences {
        for (index, canvas) in array(sequence.get("canvases")).iter().enumerate() {
            collect_canvas_v2(canvas, index + 1, summary)?;
        }
    }
    Ok(())
}

fn collect_canvas_v2(canvas: &Value, index: usize, summary: &mut Summary) -> Result<()> {
    let object = canvas
        .as_object()
        .ok_or_else(|| Error::InvalidInput("IIIF Canvas must be an object".into()))?;
    summary.canvases = summary.canvases.saturating_add(1);
    if summary.canvases > MAX_IIIF_CANVASES {
        return Err(Error::LimitExceeded(format!(
            "IIIF canvases exceed {MAX_IIIF_CANVASES}"
        )));
    }
    let label = label_value(object.get("label"), "—");
    let width = object.get("width").and_then(Value::as_u64).unwrap_or(0);
    let height = object.get("height").and_then(Value::as_u64).unwrap_or(0);
    let images = array(object.get("images"));
    summary.images = summary.images.saturating_add(images.len());
    summary.annotations = summary.annotations.saturating_add(images.len());
    if summary.annotations > MAX_IIIF_ANNOTATIONS {
        return Err(Error::LimitExceeded(format!(
            "IIIF annotations exceed {MAX_IIIF_ANNOTATIONS}"
        )));
    }
    push_row(
        summary,
        "Canvas",
        &index.to_string(),
        &label,
        format!("{}×{}, images {}", width, height, images.len()),
    )?;
    Ok(())
}

fn collect_collection(root: &serde_json::Map<String, Value>, summary: &mut Summary) -> Result<()> {
    for (index, item) in array(root.get("items")).iter().enumerate() {
        let kind = item
            .get("type")
            .or_else(|| item.get("@type"))
            .and_then(Value::as_str)
            .unwrap_or("Resource");
        push_row(
            summary,
            kind,
            &(index + 1).to_string(),
            &label_value(item.get("label"), "—"),
            "referenced item omitted".into(),
        )?;
    }
    Ok(())
}

fn count_values(value: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if depth > MAX_IIIF_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "IIIF JSON depth exceeds {MAX_IIIF_DEPTH}"
        )));
    }
    *count = count.saturating_add(1);
    if *count > MAX_IIIF_VALUES {
        return Err(Error::LimitExceeded(format!(
            "IIIF JSON values exceed {MAX_IIIF_VALUES}"
        )));
    }
    match value {
        Value::Array(values) => {
            for child in values {
                count_values(child, depth.saturating_add(1), count)?;
            }
        }
        Value::Object(values) => {
            for child in values.values() {
                count_values(child, depth.saturating_add(1), count)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn array(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

fn detect_version(root: &serde_json::Map<String, Value>) -> String {
    let context = root
        .get("@context")
        .map(Value::to_string)
        .unwrap_or_default();
    if context.contains("/presentation/3/") || root.get("items").is_some_and(Value::is_array) {
        "3".into()
    } else {
        "2".into()
    }
}

fn label_value(value: Option<&Value>, fallback: &str) -> String {
    let Some(value) = value else {
        return fallback.into();
    };
    let text = match value {
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(|value| label_value(Some(value), ""))
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" / "),
        Value::Object(values) => values
            .values()
            .next()
            .map(|value| label_value(Some(value), ""))
            .unwrap_or_default(),
        _ => value.to_string(),
    };
    safe_text(&text)
}

fn safe_text(value: &str) -> String {
    if value.contains("://") {
        "[URL omitted]".into()
    } else {
        truncate(value)
    }
}

fn push_row(
    summary: &mut Summary,
    kind: &str,
    number: &str,
    label: &str,
    detail: String,
) -> Result<()> {
    if summary.rows.len() >= MAX_IIIF_ROWS {
        return Err(Error::LimitExceeded(format!(
            "IIIF rendered rows exceed {MAX_IIIF_ROWS}"
        )));
    }
    summary.rows.push(vec![
        truncate(kind),
        truncate(number),
        truncate(label),
        truncate(&detail),
    ]);
    Ok(())
}

fn truncate(value: &str) -> String {
    if value.len() <= MAX_IIIF_DISPLAY_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_IIIF_DISPLAY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &value[..end])
}

fn display_or_dash(value: &str) -> &str {
    if value.is_empty() { "—" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_iiif_versions() {
        assert!(looks_like_prefix(br#"{"@context":"http://iiif.io/api/presentation/3/context.json","type":"Manifest","items":[]}"#));
        assert!(looks_like_prefix(br#"{"@context":"http://iiif.io/api/presentation/2/context.json","@type":"sc:Manifest","sequences":[]}"#));
        assert!(!looks_like_prefix(
            br#"{"@context":"http://iiif.io/api/presentation/3/context.json","items":[]}"#
        ));
        assert!(!looks_like_prefix(br#"{"type":"Manifest","items":[]}"#));
    }

    #[test]
    fn labels_and_urls_are_safe() {
        assert_eq!(
            label_value(
                Some(&Value::String("https://private.example.invalid".into())),
                "—"
            ),
            "[URL omitted]"
        );
        assert_eq!(
            label_value(Some(&Value::String("Page one".into())), "—"),
            "Page one"
        );
    }
}
