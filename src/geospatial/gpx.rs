//! Bounded GPX 1.1 waypoint, route, and track previews.

use std::path::Path;

use serde_json::{Value, json};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};

const GPX_NAMESPACE: &str = "http://www.topografix.com/GPX/1/1";
const MAX_GPX_INPUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_GPX_XML_NODES: usize = 200_000;
const MAX_GPX_XML_EVENTS: usize = 1_000_000;
const MAX_GPX_XML_DEPTH: usize = 64;
const MAX_GPX_TEXT_BYTES: usize = 32 * 1024 * 1024;
const MAX_GPX_FEATURES: usize = 100_000;
const MAX_GPX_POSITIONS: usize = 500_000;

#[derive(Default)]
struct GpxState {
    features: usize,
    positions: usize,
    metadata_ignored: bool,
    elevation_ignored: bool,
    links_present: bool,
    extensions_ignored: bool,
    unsupported_elements: bool,
}

pub(crate) fn looks_like_gpx_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"gpx", Some(GPX_NAMESPACE.as_bytes()))
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GPX_INPUT_BYTES),
        "GPX input",
    )?;
    if bytes.len() as u64 > MAX_GPX_INPUT_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GPX input exceeds {MAX_GPX_INPUT_BYTES} bytes"
        )));
    }
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_GPX_XML_EVENTS),
            max_nodes: MAX_GPX_XML_NODES,
            max_depth: MAX_GPX_XML_DEPTH,
            max_text_bytes: MAX_GPX_TEXT_BYTES,
        },
        "GPX",
    )?;
    if root.name != "gpx" || root.namespace.as_deref() != Some(GPX_NAMESPACE) {
        return Err(Error::InvalidInput(
            "XML root is not in the GPX 1.1 namespace".into(),
        ));
    }
    match root.attribute("version") {
        Some("1.1") => {}
        Some(version) => {
            return Err(Error::Unsupported(format!(
                "GPX version {version} is not supported; only GPX 1.1 is supported"
            )));
        }
        None => {
            return Err(Error::InvalidInput(
                "GPX root is missing its version attribute".into(),
            ));
        }
    }

    let mut state = GpxState::default();
    inspect_ignored_data(&root, &mut state);
    let mut features = Vec::new();
    for child in &root.children {
        if !in_gpx_namespace(child) {
            state.extensions_ignored = true;
            continue;
        }
        match child.name.as_str() {
            "wpt" => {
                let position = parse_point(child, &mut state)?;
                push_feature(
                    json!({ "type": "Point", "coordinates": position }),
                    &mut features,
                    &mut state,
                )?;
            }
            "rte" => {
                let positions = collect_named_points(child, "rtept", &mut state)?;
                if let Some(geometry) = line_or_point(positions) {
                    push_feature(geometry, &mut features, &mut state)?;
                } else {
                    state.unsupported_elements = true;
                }
            }
            "trk" => {
                let mut segments = Vec::new();
                for segment in child.children_named("trkseg") {
                    if !in_gpx_namespace(segment) {
                        state.extensions_ignored = true;
                        continue;
                    }
                    let positions = collect_named_points(segment, "trkpt", &mut state)?;
                    if let Some(geometry) = line_or_point(positions) {
                        segments.push(geometry);
                    }
                }
                let geometry = match segments.len() {
                    0 => None,
                    1 => Some(segments.pop().expect("one segment")),
                    _ => Some(json!({ "type": "GeometryCollection", "geometries": segments })),
                };
                if let Some(geometry) = geometry {
                    push_feature(geometry, &mut features, &mut state)?;
                } else {
                    state.unsupported_elements = true;
                }
            }
            "metadata" | "extensions" => {
                state.metadata_ignored = true;
                state.extensions_ignored |= child.name == "extensions";
            }
            _ => state.unsupported_elements = true,
        }
    }
    if features.is_empty() {
        return Err(Error::InvalidInput(
            "GPX document contains no waypoint, route, or track geometry".into(),
        ));
    }

    let mut warnings = Vec::new();
    warnings.push(
        "GPX WGS 84 latitude/longitude is projected with Web Mercator; route and track order is preserved, but distance and elevation are not measured".into(),
    );
    if state.metadata_ignored {
        warnings.push(
            "GPX names, descriptions, symbols, timestamps, and other metadata are not displayed"
                .into(),
        );
    }
    if state.elevation_ignored {
        warnings.push("GPX elevation values are not applied".into());
    }
    if state.links_present {
        warnings.push("GPX linked resources are not fetched".into());
    }
    if state.extensions_ignored {
        warnings.push("GPX extension data is omitted".into());
    }
    if state.unsupported_elements {
        warnings.push("empty or unsupported GPX route/track elements were omitted".into());
    }
    let root = json!({ "type": "FeatureCollection", "features": features });
    crate::geospatial::geojson::convert_value(
        &root,
        "gpx",
        "GPX Map Preview",
        "GPX",
        warnings,
        sink,
    )
}

fn in_gpx_namespace(node: &XmlElement) -> bool {
    node.namespace.as_deref() == Some(GPX_NAMESPACE)
}

fn inspect_ignored_data(node: &XmlElement, state: &mut GpxState) {
    if in_gpx_namespace(node) {
        match node.name.as_str() {
            "ele" => state.elevation_ignored = true,
            "time" => state.metadata_ignored = true,
            "name" | "desc" | "cmt" | "src" | "sym" | "type" | "number" | "fix" | "sat"
            | "hdop" | "vdop" | "pdop" | "ageofdgpsdata" | "dgpsid" | "course" | "speed"
            | "bounds" | "author" | "copyright" | "link" => {
                state.metadata_ignored = true;
                state.links_present |= node.name == "link";
            }
            "extensions" => state.extensions_ignored = true,
            _ => {}
        }
    } else {
        state.extensions_ignored = true;
    }
    if node.name == "href" {
        state.links_present = true;
    }
    for child in &node.children {
        inspect_ignored_data(child, state);
    }
}

fn parse_point(node: &XmlElement, state: &mut GpxState) -> Result<Value> {
    let latitude = parse_decimal_attribute(node, "lat")?;
    let longitude = parse_decimal_attribute(node, "lon")?;
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..180.0).contains(&longitude) {
        return Err(Error::InvalidInput(
            "GPX latitude/longitude is outside WGS 84 bounds".into(),
        ));
    }
    state.positions = state.positions.saturating_add(1);
    if state.positions > MAX_GPX_POSITIONS {
        return Err(Error::LimitExceeded(format!(
            "GPX exceeds {MAX_GPX_POSITIONS} waypoint/route/track positions"
        )));
    }
    Ok(json!([longitude, latitude]))
}

fn parse_decimal_attribute(node: &XmlElement, name: &str) -> Result<f64> {
    let value = node
        .attribute(name)
        .ok_or_else(|| Error::InvalidInput(format!("GPX point is missing its {name} attribute")))?;
    let number = value
        .parse::<f64>()
        .map_err(|_| Error::InvalidInput(format!("GPX point {name} attribute is not numeric")))?;
    if !number.is_finite() {
        return Err(Error::InvalidInput(format!(
            "GPX point {name} attribute is not finite"
        )));
    }
    Ok(number)
}

fn collect_named_points(
    parent: &XmlElement,
    point_name: &str,
    state: &mut GpxState,
) -> Result<Vec<Value>> {
    let mut positions = Vec::new();
    for child in parent.children_named(point_name) {
        if in_gpx_namespace(child) {
            positions.push(parse_point(child, state)?);
        }
    }
    Ok(positions)
}

fn line_or_point(positions: Vec<Value>) -> Option<Value> {
    match positions.len() {
        0 => None,
        1 => Some(json!({ "type": "Point", "coordinates": positions[0] })),
        _ => Some(json!({ "type": "LineString", "coordinates": positions })),
    }
}

fn push_feature(geometry: Value, features: &mut Vec<Value>, state: &mut GpxState) -> Result<()> {
    state.features = state.features.saturating_add(1);
    if state.features > MAX_GPX_FEATURES {
        return Err(Error::LimitExceeded(format!(
            "GPX exceeds {MAX_GPX_FEATURES} rendered waypoints/routes/track segments"
        )));
    }
    features.push(json!({ "type": "Feature", "geometry": geometry, "properties": null }));
    Ok(())
}
