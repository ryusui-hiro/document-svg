//! Bounded ASAM OpenDRIVE road-reference-line previews.
//!
//! OpenDRIVE uses XML to describe local road-network geometry. This adapter
//! renders plan-view line and constant-curvature arc reference lines as a
//! planar map, while lane rules, signals, objects, profiles, links and simulator
//! behavior remain inert and no external file or resource is opened.

use std::path::Path;

use serde_json::{Value, json};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::geospatial::xml_tree::{XmlElement, XmlLimits, parse_xml_tree};

const MAX_OPENDRIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_OPENDRIVE_XML_NODES: usize = 500_000;
const MAX_OPENDRIVE_XML_EVENTS: usize = 1_000_000;
const MAX_OPENDRIVE_XML_DEPTH: usize = 96;
const MAX_OPENDRIVE_TEXT_BYTES: usize = 64 * 1024 * 1024;
const MAX_OPENDRIVE_ROADS: usize = 100_000;
const MAX_OPENDRIVE_GEOMETRIES: usize = 200_000;
const MAX_OPENDRIVE_POSITIONS: usize = 500_000;
const METERS_PER_DEGREE: f64 = 111_320.0;
const MAX_ARC_SEGMENTS: usize = 256;

#[derive(Default)]
struct State {
    roads: usize,
    junctions: usize,
    geometries: usize,
    positions: usize,
    unsupported_geometries: usize,
    missing_plan_views: usize,
    invalid_geometry: usize,
    lines: Vec<Vec<[f64; 2]>>,
}

pub(crate) fn looks_like_prefix(bytes: &[u8]) -> bool {
    crate::geospatial::xml_tree::looks_like_root(bytes, b"OpenDRIVE", None)
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_OPENDRIVE_BYTES),
        "OpenDRIVE input",
    )?;
    let root = parse_xml_tree(
        &bytes,
        &XmlLimits {
            max_events: options.max_xml_events.min(MAX_OPENDRIVE_XML_EVENTS),
            max_nodes: MAX_OPENDRIVE_XML_NODES,
            max_depth: MAX_OPENDRIVE_XML_DEPTH,
            max_text_bytes: MAX_OPENDRIVE_TEXT_BYTES,
        },
        "OpenDRIVE",
    )?;
    if root.name != "OpenDRIVE" {
        return Err(Error::InvalidInput(
            "OpenDRIVE XML root must be OpenDRIVE".into(),
        ));
    }
    let mut state = State {
        junctions: root.children_named("junction").count(),
        ..State::default()
    };
    for road in root.children_named("road") {
        parse_road(road, &mut state)?;
    }
    if state.lines.is_empty() {
        return Err(Error::InvalidInput(
            "OpenDRIVE contains no renderable line or arc geometry".into(),
        ));
    }
    normalize_planar_coordinates(&mut state.lines);
    let mut features = Vec::with_capacity(state.lines.len());
    for line in &state.lines {
        let coordinates = line.iter().map(|[x, y]| json!([x, y])).collect::<Vec<_>>();
        features.push(json!({
            "type": "Feature",
            "geometry": {"type": "LineString", "coordinates": coordinates}
        }));
    }
    let mut warnings = vec![
        format!(
            "OpenDRIVE contains {} road(s), {} junction(s), {} plan-view geometries and {} reference-line positions",
            state.roads, state.junctions, state.geometries, state.positions
        ),
        "OpenDRIVE coordinates are local metres normalized to a planar preview; no geographic CRS transformation is inferred".into(),
        "lane sections, elevation profiles, superelevation, signals, objects, controllers, links and simulator behavior remain inert; no external resource is opened".into(),
    ];
    if state.unsupported_geometries > 0 {
        warnings.push(format!(
            "{} OpenDRIVE plan-view geometry element(s) (spiral/poly3/paramPoly3) were omitted",
            state.unsupported_geometries
        ));
    }
    if state.missing_plan_views > 0 {
        warnings.push(format!(
            "{} OpenDRIVE road(s) without a planView were omitted",
            state.missing_plan_views
        ));
    }
    if state.invalid_geometry > 0 {
        warnings.push(format!(
            "{} OpenDRIVE geometry element(s) with invalid finite attributes were omitted",
            state.invalid_geometry
        ));
    }
    let title = format!(
        "ASAM OpenDRIVE — {} roads, {} junctions",
        state.roads, state.junctions
    );
    let root_value: Value = json!({
        "type": "FeatureCollection",
        "features": features,
    });
    let result = crate::geospatial::geojson::convert_value(
        &root_value,
        "opendrive",
        &title,
        "OpenDRIVE local metres",
        std::mem::take(&mut warnings),
        sink,
    )?;
    Ok(result)
}

fn parse_road(road: &XmlElement, state: &mut State) -> Result<()> {
    state.roads = state.roads.saturating_add(1);
    if state.roads > MAX_OPENDRIVE_ROADS {
        return Err(Error::LimitExceeded(format!(
            "OpenDRIVE roads exceed {MAX_OPENDRIVE_ROADS}"
        )));
    }
    let plan_view = road.children_named("planView").next();
    let Some(plan_view) = plan_view else {
        state.missing_plan_views = state.missing_plan_views.saturating_add(1);
        return Ok(());
    };
    for geometry in plan_view.children_named("geometry") {
        state.geometries = state.geometries.saturating_add(1);
        if state.geometries > MAX_OPENDRIVE_GEOMETRIES {
            return Err(Error::LimitExceeded(format!(
                "OpenDRIVE geometries exceed {MAX_OPENDRIVE_GEOMETRIES}"
            )));
        }
        let Some(x) =
            parse_attr(geometry, "x").and_then(|value| finite_nonnegative_or_any(value, false))
        else {
            state.invalid_geometry = state.invalid_geometry.saturating_add(1);
            continue;
        };
        let Some(y) =
            parse_attr(geometry, "y").and_then(|value| finite_nonnegative_or_any(value, false))
        else {
            state.invalid_geometry = state.invalid_geometry.saturating_add(1);
            continue;
        };
        let Some(hdg) =
            parse_attr(geometry, "hdg").and_then(|value| finite_nonnegative_or_any(value, false))
        else {
            state.invalid_geometry = state.invalid_geometry.saturating_add(1);
            continue;
        };
        let Some(length) =
            parse_attr(geometry, "length").and_then(|value| finite_nonnegative_or_any(value, true))
        else {
            state.invalid_geometry = state.invalid_geometry.saturating_add(1);
            continue;
        };
        let child = geometry.children.first();
        let Some(child) = child else {
            state.invalid_geometry = state.invalid_geometry.saturating_add(1);
            continue;
        };
        let line = match child.name.as_str() {
            "line" => vec![[x, y], [x + length * hdg.cos(), y + length * hdg.sin()]],
            "arc" => {
                let Some(curvature) = child
                    .attribute("curvature")
                    .and_then(|value| value.parse::<f64>().ok())
                    .and_then(|value| finite_nonnegative_or_any(value, false))
                else {
                    state.invalid_geometry = state.invalid_geometry.saturating_add(1);
                    continue;
                };
                sample_arc(x, y, hdg, length, curvature)
            }
            "spiral" | "poly3" | "paramPoly3" => {
                state.unsupported_geometries = state.unsupported_geometries.saturating_add(1);
                continue;
            }
            _ => {
                state.unsupported_geometries = state.unsupported_geometries.saturating_add(1);
                continue;
            }
        };
        state.positions = state.positions.saturating_add(line.len());
        if state.positions > MAX_OPENDRIVE_POSITIONS {
            return Err(Error::LimitExceeded(format!(
                "OpenDRIVE positions exceed {MAX_OPENDRIVE_POSITIONS}"
            )));
        }
        state.lines.push(line);
    }
    Ok(())
}

fn sample_arc(x: f64, y: f64, hdg: f64, length: f64, curvature: f64) -> Vec<[f64; 2]> {
    if curvature.abs() < 1.0e-12 {
        return vec![[x, y], [x + length * hdg.cos(), y + length * hdg.sin()]];
    }
    let segments = ((curvature.abs() * length) / (std::f64::consts::PI / 18.0))
        .ceil()
        .clamp(1.0, MAX_ARC_SEGMENTS as f64) as usize;
    let mut points = Vec::with_capacity(segments + 1);
    for index in 0..=segments {
        let distance = length * index as f64 / segments as f64;
        let angle = hdg + curvature * distance;
        points.push([
            x + (angle.sin() - hdg.sin()) / curvature,
            y + (hdg.cos() - angle.cos()) / curvature,
        ]);
    }
    points
}

fn normalize_planar_coordinates(lines: &mut [Vec<[f64; 2]>]) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    for line in lines.iter() {
        for [x, y] in line {
            min_x = min_x.min(*x);
            min_y = min_y.min(*y);
        }
    }
    for line in lines {
        for point in line {
            point[0] = (point[0] - min_x) / METERS_PER_DEGREE;
            point[1] = (point[1] - min_y) / METERS_PER_DEGREE;
        }
    }
}

fn parse_attr(element: &XmlElement, name: &str) -> Option<f64> {
    element.attribute(name)?.parse::<f64>().ok()
}

fn finite_nonnegative_or_any(value: f64, nonnegative: bool) -> Option<f64> {
    if !value.is_finite() || (nonnegative && value < 0.0) {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_opendrive_root() {
        assert!(looks_like_prefix(
            br#"<OpenDRIVE><road id=\"1\"/></OpenDRIVE>"#
        ));
        assert!(!looks_like_prefix(br#"<road id=\"1\"/>"#));
    }

    #[test]
    fn samples_lines_and_arcs_with_bounded_points() {
        let xml = br#"<OpenDRIVE><road id="1" length="20"><planView><geometry s="0" x="0" y="0" hdg="0" length="10"><line/></geometry><geometry s="10" x="10" y="0" hdg="0" length="10"><arc curvature="0.1"/></geometry></planView></road><junction id="j1"/></OpenDRIVE>"#;
        let root = parse_xml_tree(
            xml,
            &XmlLimits {
                max_events: 1000,
                max_nodes: 1000,
                max_depth: 32,
                max_text_bytes: 10000,
            },
            "OpenDRIVE",
        )
        .unwrap();
        let mut state = State {
            junctions: root.children_named("junction").count(),
            ..State::default()
        };
        parse_road(root.children_named("road").next().unwrap(), &mut state).unwrap();
        assert_eq!(state.roads, 1);
        assert_eq!(state.junctions, 1);
        assert_eq!(state.lines.len(), 2);
        assert!(state.positions > 2);
    }
}
