//! Bounded GeoJSON map previews.

use std::path::Path;

use serde_json::Value;

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};
use crate::ir::{IDENTITY, Node, Page, Paint, SourceMeta, Stroke};

const MAX_GEOJSON_INPUT_BYTES: u64 = 32 * 1024 * 1024;
const MAX_GEOJSON_JSON_NODES: usize = 6_000_000;
const MAX_GEOJSON_JSON_DEPTH: usize = 64;
const MAX_GEOJSON_FEATURES: usize = 100_000;
const MAX_GEOJSON_GEOMETRIES: usize = 200_000;
const MAX_GEOJSON_POSITIONS: usize = 500_000;
const MAX_GEOJSON_PATH_BYTES: usize = 40 * 1024 * 1024;
const MAX_GEOMETRY_DEPTH: usize = 16;
const MAX_MERCATOR_LATITUDE: f64 = 85.051_128_779_806_6;
const MAX_UNWRAPPED_LONGITUDE: f64 = 540.0;
const PAGE_WIDTH: f64 = 1200.0;
const PAGE_HEIGHT: f64 = 800.0;
const PAGE_MARGIN: f64 = 40.0;

#[derive(Default)]
struct GeometrySet {
    points: Vec<[f64; 2]>,
    lines: Vec<Vec<[f64; 2]>>,
    polygons: Vec<Vec<Vec<[f64; 2]>>>,
    position_count: usize,
    geometry_count: usize,
    feature_count: usize,
    ignored_properties: bool,
    ignored_altitude: bool,
    open_rings: bool,
    skipped_rings: bool,
    dateline_crossings: bool,
    mercator_clamped: bool,
    longitude_unwrapped: bool,
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_GEOJSON_INPUT_BYTES),
        "GeoJSON input",
    )?;
    preflight_json_budget(&bytes)?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Error::InvalidInput(format!("invalid GeoJSON: {error}")))?;
    convert_value(
        &root,
        "geojson",
        "GeoJSON Map Preview",
        "GeoJSON",
        Vec::new(),
        sink,
    )
}

pub(crate) fn convert_value(
    root: &Value,
    source_format: &str,
    title: &str,
    coordinate_format: &str,
    mut warnings: Vec<String>,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let mut geometry = GeometrySet::default();
    let root_type = root.get("type").and_then(Value::as_str);
    if root.get("bbox").is_some()
        || root.get("crs").is_some()
        || (root_type == Some("FeatureCollection")
            && has_unknown_member(root, &["type", "features", "bbox", "crs"]))
    {
        geometry.ignored_properties = true;
    }
    match root_type {
        Some("FeatureCollection") => {
            let features = root
                .get("features")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    Error::InvalidInput(
                        "GeoJSON FeatureCollection is missing its features array".into(),
                    )
                })?;
            if features.len() > MAX_GEOJSON_FEATURES {
                return Err(Error::LimitExceeded(format!(
                    "GeoJSON has {} features; maximum is {MAX_GEOJSON_FEATURES}",
                    features.len()
                )));
            }
            for feature in features {
                read_feature(feature, &mut geometry)?;
            }
        }
        Some("Feature") => read_feature(root, &mut geometry)?,
        Some(_) => read_geometry(root, 0, &mut geometry)?,
        None => {
            return Err(Error::InvalidInput(
                "GeoJSON object is missing a string 'type' member".into(),
            ));
        }
    }

    if geometry.position_count == 0 {
        return Err(Error::InvalidInput(format!(
            "{coordinate_format} contains no renderable geometry positions"
        )));
    }
    if coordinate_format == "OpenDRIVE local metres" {
        warnings.push(
            "OpenDRIVE local metre coordinates are normalized for a planar preview; Web Mercator and geographic CRS transforms are not applied".into(),
        );
    } else {
        warnings.push(format!(
            "{coordinate_format} longitude/latitude is projected with Web Mercator; altitude and custom CRS transforms are not applied"
        ));
    }
    if geometry.ignored_properties {
        warnings.push(
            "feature properties, identifiers, bbox, and foreign metadata are not shown".into(),
        );
    }
    if geometry.ignored_altitude {
        warnings.push("GeoJSON altitude and additional coordinate dimensions were ignored".into());
    }
    if geometry.open_rings {
        warnings.push("open GeoJSON polygon rings were closed for preview".into());
    }
    if geometry.skipped_rings {
        warnings.push("GeoJSON polygon rings with fewer than three positions were omitted".into());
    }
    if geometry.dateline_crossings {
        warnings.push(
            "segments crossing the antimeridian are drawn as straight projected lines".into(),
        );
    }
    if geometry.mercator_clamped {
        warnings.push(format!(
            "latitudes beyond ±{MAX_MERCATOR_LATITUDE:.6}° were clamped for Web Mercator"
        ));
    }
    if geometry.longitude_unwrapped {
        warnings.push(
            "longitudes beyond ±180° (antimeridian-unwrapped geometry, e.g. the Aleutian Islands) were normalized into WGS 84 bounds".into(),
        );
    }

    let page = render_geometry(&geometry, &warnings, source_format, title)?;
    sink.consume(page)?;
    Ok(warnings)
}

pub(crate) fn preflight_json_budget(bytes: &[u8]) -> Result<()> {
    let mut offset = 0usize;
    let mut nodes = 0usize;
    let mut depth = 0usize;
    while offset < bytes.len() {
        match bytes[offset] {
            b'"' => {
                nodes = nodes.saturating_add(1);
                offset += 1;
                let mut escaped = false;
                while offset < bytes.len() {
                    let byte = bytes[offset];
                    offset += 1;
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        break;
                    }
                }
            }
            b'{' | b'[' => {
                nodes = nodes.saturating_add(1);
                depth += 1;
                if depth > MAX_GEOJSON_JSON_DEPTH {
                    return Err(Error::LimitExceeded(format!(
                        "GeoJSON JSON nesting exceeds {MAX_GEOJSON_JSON_DEPTH}"
                    )));
                }
                offset += 1;
            }
            b'}' | b']' => {
                if depth == 0 {
                    return Err(Error::InvalidInput(
                        "GeoJSON JSON brackets are unbalanced".into(),
                    ));
                }
                depth -= 1;
                offset += 1;
            }
            b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                nodes = nodes.saturating_add(1);
                offset += 1;
                while offset < bytes.len()
                    && !matches!(
                        bytes[offset],
                        b',' | b':' | b']' | b'}' | b' ' | b'\n' | b'\r' | b'\t'
                    )
                {
                    offset += 1;
                }
            }
            _ => offset += 1,
        }
        if nodes > MAX_GEOJSON_JSON_NODES {
            return Err(Error::LimitExceeded(format!(
                "GeoJSON contains more than {MAX_GEOJSON_JSON_NODES} JSON values"
            )));
        }
    }
    if depth != 0 {
        return Err(Error::InvalidInput(
            "GeoJSON JSON brackets are unbalanced".into(),
        ));
    }
    Ok(())
}

fn read_feature(feature: &Value, geometry: &mut GeometrySet) -> Result<()> {
    if feature.get("type").and_then(Value::as_str) != Some("Feature") {
        return Err(Error::InvalidInput(
            "GeoJSON FeatureCollection contains a non-Feature member".into(),
        ));
    }
    geometry.feature_count = geometry.feature_count.saturating_add(1);
    if geometry.feature_count > MAX_GEOJSON_FEATURES {
        return Err(Error::LimitExceeded(format!(
            "GeoJSON has more than {MAX_GEOJSON_FEATURES} features"
        )));
    }
    if feature.get("properties").is_some_and(|properties| {
        !properties.is_null() && !properties.as_object().is_some_and(|map| map.is_empty())
    }) || feature.get("id").is_some()
        || feature.get("bbox").is_some()
        || has_unknown_member(feature, &["type", "geometry", "properties", "id", "bbox"])
    {
        geometry.ignored_properties = true;
    }
    match feature.get("geometry") {
        Some(Value::Null) | None => {
            geometry.ignored_properties = true;
            Ok(())
        }
        Some(geometry_value) => read_geometry(geometry_value, 0, geometry),
    }
}

fn read_geometry(value: &Value, depth: usize, geometry: &mut GeometrySet) -> Result<()> {
    if depth > MAX_GEOMETRY_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "GeoJSON GeometryCollection nesting exceeds {MAX_GEOMETRY_DEPTH}"
        )));
    }
    geometry.geometry_count = geometry.geometry_count.saturating_add(1);
    if geometry.geometry_count > MAX_GEOJSON_GEOMETRIES {
        return Err(Error::LimitExceeded(format!(
            "GeoJSON contains more than {MAX_GEOJSON_GEOMETRIES} geometries"
        )));
    }
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidInput("GeoJSON geometry is missing its type".into()))?;
    if value.get("bbox").is_some()
        || value.get("crs").is_some()
        || has_unknown_member(value, &["type", "coordinates", "geometries", "bbox", "crs"])
    {
        geometry.ignored_properties = true;
    }
    match kind {
        "Point" => {
            let position = parse_position(required(value, "coordinates")?, geometry)?;
            geometry.points.push(position);
        }
        "MultiPoint" => {
            for position in parse_positions(required(value, "coordinates")?, geometry)? {
                geometry.points.push(position);
            }
        }
        "LineString" => {
            let line = parse_positions(required(value, "coordinates")?, geometry)?;
            if line.len() >= 2 {
                detect_antimeridian(&line, geometry);
                geometry.lines.push(line);
            }
        }
        "MultiLineString" => {
            let lines = required(value, "coordinates")?.as_array().ok_or_else(|| {
                Error::InvalidInput("GeoJSON MultiLineString coordinates must be an array".into())
            })?;
            for line_value in lines {
                let line = parse_positions(line_value, geometry)?;
                if line.len() >= 2 {
                    detect_antimeridian(&line, geometry);
                    geometry.lines.push(line);
                }
            }
        }
        "Polygon" => {
            if let Some(polygon) = parse_polygon(required(value, "coordinates")?, geometry)? {
                geometry.polygons.push(polygon);
            }
        }
        "MultiPolygon" => {
            let polygons = required(value, "coordinates")?.as_array().ok_or_else(|| {
                Error::InvalidInput("GeoJSON MultiPolygon coordinates must be an array".into())
            })?;
            for polygon_value in polygons {
                if let Some(polygon) = parse_polygon(polygon_value, geometry)? {
                    geometry.polygons.push(polygon);
                }
            }
        }
        "GeometryCollection" => {
            let geometries = value
                .get("geometries")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    Error::InvalidInput(
                        "GeoJSON GeometryCollection is missing its geometries array".into(),
                    )
                })?;
            for nested in geometries {
                read_geometry(nested, depth + 1, geometry)?;
            }
        }
        other => {
            return Err(Error::Unsupported(format!(
                "unsupported GeoJSON geometry type '{other}'"
            )));
        }
    }
    Ok(())
}

fn parse_polygon(value: &Value, geometry: &mut GeometrySet) -> Result<Option<Vec<Vec<[f64; 2]>>>> {
    let rings = value.as_array().ok_or_else(|| {
        Error::InvalidInput("GeoJSON Polygon coordinates must be an array".into())
    })?;
    if rings.is_empty() {
        return Ok(None);
    }
    let mut parsed = Vec::with_capacity(rings.len());
    for ring_value in rings {
        let mut ring = parse_positions(ring_value, geometry)?;
        if ring.len() < 3 {
            geometry.skipped_rings = true;
            continue;
        }
        if ring.first() != ring.last() {
            ring.push(ring[0]);
            geometry.open_rings = true;
        }
        detect_antimeridian(&ring, geometry);
        parsed.push(ring);
    }
    if parsed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parsed))
    }
}

fn parse_positions(value: &Value, geometry: &mut GeometrySet) -> Result<Vec<[f64; 2]>> {
    let positions = value
        .as_array()
        .ok_or_else(|| Error::InvalidInput("GeoJSON coordinates must be an array".into()))?;
    positions
        .iter()
        .map(|position| parse_position(position, geometry))
        .collect()
}

fn parse_position(value: &Value, geometry: &mut GeometrySet) -> Result<[f64; 2]> {
    let coordinates = value
        .as_array()
        .filter(|coordinates| coordinates.len() >= 2)
        .ok_or_else(|| {
            Error::InvalidInput("GeoJSON position must contain longitude and latitude".into())
        })?;
    let mut longitude = coordinates[0]
        .as_f64()
        .ok_or_else(|| Error::InvalidInput("GeoJSON longitude must be numeric".into()))?;
    let latitude = coordinates[1]
        .as_f64()
        .ok_or_else(|| Error::InvalidInput("GeoJSON latitude must be numeric".into()))?;
    if !longitude.is_finite() || !latitude.is_finite() || !(-90.0..=90.0).contains(&latitude) {
        return Err(Error::InvalidInput(
            "GeoJSON longitude/latitude is outside WGS 84 bounds".into(),
        ));
    }
    if !(-180.0..=180.0).contains(&longitude) {
        // Some real-world datasets (e.g. US state boundaries covering the Aleutian
        // Islands) leave polygons unwrapped across the antimeridian instead of
        // splitting them, so longitude runs a little past +/-180 rather than
        // jumping back. Normalize those back into range; anything further out is
        // not a plausible unwrap and stays an error.
        if longitude.abs() > MAX_UNWRAPPED_LONGITUDE {
            return Err(Error::InvalidInput(
                "GeoJSON longitude/latitude is outside WGS 84 bounds".into(),
            ));
        }
        longitude = ((longitude + 180.0).rem_euclid(360.0)) - 180.0;
        geometry.longitude_unwrapped = true;
    }
    if coordinates.len() > 2 {
        geometry.ignored_altitude = true;
    }
    if latitude.abs() > MAX_MERCATOR_LATITUDE {
        geometry.mercator_clamped = true;
    }
    geometry.position_count = geometry.position_count.saturating_add(1);
    if geometry.position_count > MAX_GEOJSON_POSITIONS {
        return Err(Error::LimitExceeded(format!(
            "GeoJSON contains more than {MAX_GEOJSON_POSITIONS} coordinate positions"
        )));
    }
    Ok([longitude, latitude])
}

fn required<'a>(value: &'a Value, member: &str) -> Result<&'a Value> {
    value
        .get(member)
        .ok_or_else(|| Error::InvalidInput(format!("GeoJSON object is missing '{member}'")))
}

fn has_unknown_member(value: &Value, allowed: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().any(|key| !allowed.contains(&key.as_str())))
}

fn detect_antimeridian(line: &[[f64; 2]], geometry: &mut GeometrySet) {
    if line
        .windows(2)
        .any(|pair| (pair[0][0] - pair[1][0]).abs() > 180.0)
    {
        geometry.dateline_crossings = true;
    }
}

fn project([longitude, latitude]: [f64; 2]) -> [f64; 2] {
    let clipped_latitude = latitude.clamp(-MAX_MERCATOR_LATITUDE, MAX_MERCATOR_LATITUDE);
    let radians = clipped_latitude.to_radians();
    let y = (std::f64::consts::FRAC_PI_4 + radians / 2.0).tan().ln();
    [longitude.to_radians(), y]
}

fn render_geometry(
    geometry: &GeometrySet,
    warnings: &[String],
    source_format: &str,
    title: &str,
) -> Result<Page> {
    let semantic_prefix = source_format.to_ascii_lowercase();
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut include = |position| {
        let [x, y] = project(position);
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x);
        bounds[3] = bounds[3].max(y);
    };
    for point in &geometry.points {
        include(*point);
    }
    for line in &geometry.lines {
        for point in line {
            include(*point);
        }
    }
    for polygon in &geometry.polygons {
        for ring in polygon {
            for point in ring {
                include(*point);
            }
        }
    }
    if !bounds.iter().all(|value| value.is_finite()) {
        return Err(Error::InvalidInput(
            "GeoJSON produced no finite projected bounds".into(),
        ));
    }
    let span_x = (bounds[2] - bounds[0]).max(1.0e-9);
    let span_y = (bounds[3] - bounds[1]).max(1.0e-9);
    let available_width = PAGE_WIDTH - PAGE_MARGIN * 2.0;
    let available_height = PAGE_HEIGHT - PAGE_MARGIN * 2.0;
    let scale = (available_width / span_x).min(available_height / span_y);
    let drawing_width = span_x * scale;
    let drawing_height = span_y * scale;
    let offset_x = PAGE_MARGIN + (available_width - drawing_width) / 2.0;
    let offset_y = PAGE_MARGIN + (available_height - drawing_height) / 2.0;
    let map = |point: [f64; 2]| {
        let [x, y] = project(point);
        [
            offset_x + (x - bounds[0]) * scale,
            PAGE_HEIGHT - offset_y - (y - bounds[1]) * scale,
        ]
    };

    let mut polygon_path = String::new();
    for polygon in &geometry.polygons {
        for ring in polygon {
            append_path(&mut polygon_path, ring, &map, true)?;
        }
    }
    let mut line_path = String::new();
    for line in &geometry.lines {
        append_path(&mut line_path, line, &map, false)?;
    }
    let mut point_path = String::new();
    for point in &geometry.points {
        let [x, y] = map(*point);
        let radius = 4.0;
        point_path.push_str(&format!(
            "M {:.3} {:.3} a{radius} {radius} 0 1 0 {} 0 a{radius} {radius} 0 1 0 -{} 0 ",
            x - radius,
            y,
            radius * 2.0,
            radius * 2.0
        ));
        if polygon_path.len() + line_path.len() + point_path.len() > MAX_GEOJSON_PATH_BYTES {
            return Err(Error::LimitExceeded(format!(
                "GeoJSON SVG paths exceed {MAX_GEOJSON_PATH_BYTES} bytes"
            )));
        }
    }
    let total_path_bytes = polygon_path
        .len()
        .saturating_add(line_path.len())
        .saturating_add(point_path.len());
    if total_path_bytes > MAX_GEOJSON_PATH_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GeoJSON SVG paths exceed {MAX_GEOJSON_PATH_BYTES} bytes"
        )));
    }

    let mut page = Page::new(1, PAGE_WIDTH, PAGE_HEIGHT, source_format);
    page.title = title.into();
    page.description = format!(
        "{} map with {} features, {} geometries, and {} positions",
        title, geometry.feature_count, geometry.geometry_count, geometry.position_count
    );
    for warning in warnings {
        page.warn(warning.clone());
    }
    if !polygon_path.is_empty() {
        page.nodes.push(Node::Path {
            id: format!("{semantic_prefix}-polygons"),
            d: polygon_path,
            fill_rule: "evenodd".into(),
            fill: Paint::solid("#BFDBFE"),
            stroke: Stroke {
                paint: Paint::solid("#2563EB"),
                width: 1.5,
                ..Default::default()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: format!("{semantic_prefix}-polygon"),
                semantic_role: format!("{semantic_prefix}:polygon"),
                ..Default::default()
            },
        });
    }
    if !line_path.is_empty() {
        page.nodes.push(Node::Path {
            id: format!("{semantic_prefix}-lines"),
            d: line_path,
            fill_rule: "nonzero".into(),
            fill: Paint::None,
            stroke: Stroke {
                paint: Paint::solid("#1D4ED8"),
                width: 2.0,
                ..Default::default()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: format!("{semantic_prefix}-line"),
                semantic_role: format!("{semantic_prefix}:line"),
                ..Default::default()
            },
        });
    }
    if !point_path.is_empty() {
        page.nodes.push(Node::Path {
            id: format!("{semantic_prefix}-points"),
            d: point_path,
            fill_rule: "nonzero".into(),
            fill: Paint::solid("#DC2626"),
            stroke: Stroke {
                paint: Paint::solid("#FFFFFF"),
                width: 1.0,
                ..Default::default()
            },
            transform: IDENTITY,
            clip_id: None,
            meta: SourceMeta {
                kind: format!("{semantic_prefix}-point"),
                semantic_role: format!("{semantic_prefix}:point"),
                ..Default::default()
            },
        });
    }
    Ok(page)
}

fn append_path(
    output: &mut String,
    points: &[[f64; 2]],
    map: &impl Fn([f64; 2]) -> [f64; 2],
    close: bool,
) -> Result<()> {
    if points.is_empty() {
        return Ok(());
    }
    let [x, y] = map(points[0]);
    output.push_str(&format!("M {:.3} {:.3} ", x, y));
    for point in &points[1..] {
        let [x, y] = map(*point);
        output.push_str(&format!("L {:.3} {:.3} ", x, y));
    }
    if close {
        output.push_str("Z ");
    }
    if output.len() > MAX_GEOJSON_PATH_BYTES {
        return Err(Error::LimitExceeded(format!(
            "GeoJSON SVG paths exceed {MAX_GEOJSON_PATH_BYTES} bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_json_nesting_before_building_a_value_tree() {
        let text = format!(
            "{}0{}",
            "[".repeat(MAX_GEOJSON_JSON_DEPTH + 1),
            "]".repeat(MAX_GEOJSON_JSON_DEPTH + 1)
        );
        assert!(matches!(
            preflight_json_budget(text.as_bytes()),
            Err(Error::LimitExceeded(message)) if message.contains("nesting")
        ));
    }

    #[test]
    fn rejects_coordinates_outside_the_geojson_wgs84_range() {
        let mut geometry = GeometrySet::default();
        let wrapped = parse_position(&serde_json::json!([181.0, 0.0]), &mut geometry).unwrap();
        assert_eq!(wrapped[0], -179.0);
        assert!(geometry.longitude_unwrapped);
        assert!(parse_position(&serde_json::json!([541.0, 0.0]), &mut geometry).is_err());
        assert!(parse_position(&serde_json::json!([0.0, 91.0]), &mut geometry).is_err());
    }
}
