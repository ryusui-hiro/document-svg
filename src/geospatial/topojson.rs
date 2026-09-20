//! Bounded TopoJSON topology reconstruction and map previews.

use std::path::Path;

use serde_json::{Value, json};

use crate::convert::{ConvertOptions, PageConsumer, read_limited_file};
use crate::error::{Error, Result};

const MAX_TOPOJSON_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TOPOJSON_ARCS: usize = 200_000;
const MAX_TOPOJSON_ARC_POSITIONS: usize = 500_000;
const MAX_TOPOJSON_GEOMETRIES: usize = 200_000;
const MAX_TOPOJSON_OBJECTS: usize = 100_000;
const MAX_TOPOJSON_EXPANDED_POSITIONS: usize = 500_000;
const MAX_TOPOJSON_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug)]
struct Transform {
    scale: [f64; 2],
    translate: [f64; 2],
}

#[derive(Default)]
struct TopologyBudget {
    arc_positions: usize,
    expanded_positions: usize,
    geometry_count: usize,
    has_extra_dimensions: bool,
}

pub(crate) fn looks_like_topojson_prefix(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with('{')
        && text.contains("\"Topology\"")
        && text.contains("\"arcs\"")
        && text.contains("\"objects\"")
}

pub(crate) fn convert(
    path: &Path,
    options: &ConvertOptions,
    sink: &mut dyn PageConsumer,
) -> Result<Vec<String>> {
    let bytes = read_limited_file(
        path,
        options.max_input_bytes.min(MAX_TOPOJSON_BYTES),
        "TopoJSON input",
    )?;
    crate::geospatial::geojson::preflight_json_budget(&bytes)?;
    let root: Value = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("invalid JSON: {error}")))?;
    let topology = root
        .as_object()
        .filter(|object| object.get("type").and_then(Value::as_str) == Some("Topology"))
        .ok_or_else(|| invalid("root object must have type 'Topology'"))?;
    let transform = parse_transform(topology.get("transform"))?;
    let mut budget = TopologyBudget::default();
    let arcs = parse_arcs(topology.get("arcs"), transform, &mut budget)?;
    let objects = topology
        .get("objects")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("Topology is missing its objects object"))?;
    if objects.len() > MAX_TOPOJSON_OBJECTS {
        return Err(Error::LimitExceeded(format!(
            "TopoJSON exceeds {MAX_TOPOJSON_OBJECTS} objects"
        )));
    }
    let mut features = Vec::with_capacity(objects.len().min(4096));
    for object in objects.values() {
        let geometry = decode_geometry(object, &arcs, transform, 0, &mut budget)?;
        if features.len() >= MAX_TOPOJSON_OBJECTS {
            return Err(Error::LimitExceeded(format!(
                "TopoJSON expands to more than {MAX_TOPOJSON_OBJECTS} root objects"
            )));
        }
        features.push(json!({
            "type": "Feature",
            "geometry": geometry,
            "properties": null
        }));
    }
    if features.is_empty() {
        return Err(invalid("Topology contains no geometry objects"));
    }
    let root = json!({ "type": "FeatureCollection", "features": features });
    let mut warnings = vec![
        "TopoJSON object properties, identifiers, bounding boxes, and topology metadata are omitted".into(),
        "TopoJSON coordinates are treated as WGS 84 longitude/latitude; no CRS metadata or reprojection is available".into(),
    ];
    if budget.has_extra_dimensions {
        warnings.push("TopoJSON coordinate ordinates beyond x/y were omitted".into());
    }
    crate::geospatial::geojson::convert_value(
        &root,
        "topojson",
        "TopoJSON Map Preview",
        "TopoJSON",
        warnings,
        sink,
    )
}

fn parse_transform(value: Option<&Value>) -> Result<Option<Transform>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let transform = value
        .as_object()
        .ok_or_else(|| invalid("transform must be an object"))?;
    let scale = parse_pair(
        transform.get("scale"),
        "transform.scale",
        "transform scale values must be finite numbers",
    )?;
    let translate = parse_pair(
        transform.get("translate"),
        "transform.translate",
        "transform translate values must be finite numbers",
    )?;
    Ok(Some(Transform { scale, translate }))
}

fn parse_pair(value: Option<&Value>, name: &str, message: &str) -> Result<[f64; 2]> {
    let values = value
        .and_then(Value::as_array)
        .filter(|values| values.len() == 2)
        .ok_or_else(|| invalid(format!("{name} must be a two-element array")))?;
    let x = values[0]
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid(message))?;
    let y = values[1]
        .as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| invalid(message))?;
    Ok([x, y])
}

fn parse_arcs(
    value: Option<&Value>,
    transform: Option<Transform>,
    budget: &mut TopologyBudget,
) -> Result<Vec<Vec<[f64; 2]>>> {
    let arcs = value
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Topology is missing its arcs array"))?;
    if arcs.len() > MAX_TOPOJSON_ARCS {
        return Err(Error::LimitExceeded(format!(
            "TopoJSON exceeds {MAX_TOPOJSON_ARCS} arcs"
        )));
    }
    arcs.iter()
        .enumerate()
        .map(|(index, arc)| parse_arc(index, arc, transform, budget))
        .collect()
}

fn parse_arc(
    index: usize,
    value: &Value,
    transform: Option<Transform>,
    budget: &mut TopologyBudget,
) -> Result<Vec<[f64; 2]>> {
    let positions = value
        .as_array()
        .filter(|positions| positions.len() >= 2)
        .ok_or_else(|| invalid(format!("arc {index} must contain at least two positions")))?;
    let new_total = budget.arc_positions.saturating_add(positions.len());
    if new_total > MAX_TOPOJSON_ARC_POSITIONS {
        return Err(Error::LimitExceeded(format!(
            "TopoJSON exceeds {MAX_TOPOJSON_ARC_POSITIONS} decoded arc positions"
        )));
    }
    budget.arc_positions = new_total;
    let mut result = Vec::with_capacity(positions.len());
    let mut accumulator = [0.0, 0.0];
    for position in positions {
        let coordinates = position
            .as_array()
            .filter(|coordinates| coordinates.len() >= 2)
            .ok_or_else(|| {
                invalid(format!(
                    "arc {index} contains a position with fewer than two ordinates"
                ))
            })?;
        let x = parse_ordinate(&coordinates[0], index)?;
        let y = parse_ordinate(&coordinates[1], index)?;
        for ordinate in &coordinates[2..] {
            parse_ordinate(ordinate, index)?;
            budget.has_extra_dimensions = true;
        }
        let [x, y] = if let Some(transform) = transform {
            accumulator[0] += parse_quantized(&coordinates[0], index)?;
            accumulator[1] += parse_quantized(&coordinates[1], index)?;
            [
                accumulator[0] * transform.scale[0] + transform.translate[0],
                accumulator[1] * transform.scale[1] + transform.translate[1],
            ]
        } else {
            [x, y]
        };
        if !x.is_finite() || !y.is_finite() {
            return Err(invalid(format!(
                "arc {index} transforms to a non-finite coordinate"
            )));
        }
        result.push([x, y]);
    }
    Ok(result)
}

fn decode_geometry(
    value: &Value,
    arcs: &[Vec<[f64; 2]>],
    transform: Option<Transform>,
    depth: usize,
    budget: &mut TopologyBudget,
) -> Result<Value> {
    if depth > MAX_TOPOJSON_DEPTH {
        return Err(Error::LimitExceeded(format!(
            "TopoJSON GeometryCollection nesting exceeds {MAX_TOPOJSON_DEPTH}"
        )));
    }
    budget.geometry_count = budget.geometry_count.saturating_add(1);
    if budget.geometry_count > MAX_TOPOJSON_GEOMETRIES {
        return Err(Error::LimitExceeded(format!(
            "TopoJSON exceeds {MAX_TOPOJSON_GEOMETRIES} geometries"
        )));
    }
    let object = value
        .as_object()
        .ok_or_else(|| invalid("Topology object member must be a geometry object"))?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("TopoJSON geometry is missing a string type"))?;
    let geometry = match kind {
        "Point" => {
            let position = parse_point(object.get("coordinates"), transform, budget)?;
            json!({ "type": "Point", "coordinates": position })
        }
        "MultiPoint" => {
            let values = object
                .get("coordinates")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("TopoJSON MultiPoint is missing its coordinates array"))?;
            let mut positions = Vec::with_capacity(values.len().min(4096));
            for position in values {
                positions.push(parse_point(Some(position), transform, budget)?);
            }
            json!({ "type": "MultiPoint", "coordinates": positions })
        }
        "LineString" => {
            let references = object
                .get("arcs")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("TopoJSON LineString is missing its arcs array"))?;
            let positions = join_arcs(references, arcs, budget)?;
            json!({ "type": "LineString", "coordinates": positions })
        }
        "MultiLineString" => {
            let lines = object
                .get("arcs")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("TopoJSON MultiLineString is missing its arcs array"))?;
            let mut coordinates = Vec::with_capacity(lines.len().min(4096));
            for line in lines {
                let references = line.as_array().ok_or_else(|| {
                    invalid("TopoJSON MultiLineString member must be an arc array")
                })?;
                coordinates.push(join_arcs(references, arcs, budget)?);
            }
            json!({ "type": "MultiLineString", "coordinates": coordinates })
        }
        "Polygon" => {
            let rings = object
                .get("arcs")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("TopoJSON Polygon is missing its arcs array"))?;
            let mut coordinates = Vec::with_capacity(rings.len().min(4096));
            for ring in rings {
                let references = ring
                    .as_array()
                    .ok_or_else(|| invalid("TopoJSON Polygon ring must be an arc array"))?;
                coordinates.push(join_arcs(references, arcs, budget)?);
            }
            json!({ "type": "Polygon", "coordinates": coordinates })
        }
        "MultiPolygon" => {
            let polygons = object
                .get("arcs")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("TopoJSON MultiPolygon is missing its arcs array"))?;
            let mut coordinates = Vec::with_capacity(polygons.len().min(4096));
            for polygon in polygons {
                let rings = polygon
                    .as_array()
                    .ok_or_else(|| invalid("TopoJSON MultiPolygon member must be a ring array"))?;
                let mut polygon_coordinates = Vec::with_capacity(rings.len().min(4096));
                for ring in rings {
                    let references = ring.as_array().ok_or_else(|| {
                        invalid("TopoJSON MultiPolygon ring must be an arc array")
                    })?;
                    polygon_coordinates.push(join_arcs(references, arcs, budget)?);
                }
                coordinates.push(polygon_coordinates);
            }
            json!({ "type": "MultiPolygon", "coordinates": coordinates })
        }
        "GeometryCollection" => {
            let geometries = object
                .get("geometries")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    invalid("TopoJSON GeometryCollection is missing its geometries array")
                })?;
            let mut result = Vec::with_capacity(geometries.len().min(4096));
            for geometry in geometries {
                result.push(decode_geometry(
                    geometry,
                    arcs,
                    transform,
                    depth + 1,
                    budget,
                )?);
            }
            json!({ "type": "GeometryCollection", "geometries": result })
        }
        other => {
            return Err(Error::Unsupported(format!(
                "TopoJSON geometry type '{other}' is unsupported"
            )));
        }
    };
    Ok(geometry)
}

fn parse_point(
    value: Option<&Value>,
    transform: Option<Transform>,
    budget: &mut TopologyBudget,
) -> Result<Value> {
    let coordinates = value
        .and_then(Value::as_array)
        .filter(|coordinates| coordinates.len() >= 2)
        .ok_or_else(|| invalid("TopoJSON point position must contain at least x and y"))?;
    let raw_x = parse_ordinate(&coordinates[0], 0)?;
    let raw_y = parse_ordinate(&coordinates[1], 0)?;
    for ordinate in &coordinates[2..] {
        parse_ordinate(ordinate, 0)?;
        budget.has_extra_dimensions = true;
    }
    let [x, y] = if let Some(transform) = transform {
        [
            raw_x * transform.scale[0] + transform.translate[0],
            raw_y * transform.scale[1] + transform.translate[1],
        ]
    } else {
        [raw_x, raw_y]
    };
    if !x.is_finite() || !y.is_finite() {
        return Err(invalid(
            "TopoJSON point transforms to a non-finite coordinate",
        ));
    }
    add_expanded_positions(budget, 1)?;
    Ok(json!([x, y]))
}

fn join_arcs(
    references: &[Value],
    arcs: &[Vec<[f64; 2]>],
    budget: &mut TopologyBudget,
) -> Result<Vec<[f64; 2]>> {
    let mut positions = Vec::new();
    for reference in references {
        let index = reference
            .as_i64()
            .ok_or_else(|| invalid("TopoJSON arc reference must be an integer"))?;
        let (index, reversed) = if index < 0 {
            (
                usize::try_from(!index).map_err(|_| invalid("TopoJSON arc index overflows"))?,
                true,
            )
        } else {
            (
                usize::try_from(index).map_err(|_| invalid("TopoJSON arc index overflows"))?,
                false,
            )
        };
        let arc = arcs
            .get(index)
            .ok_or_else(|| invalid("TopoJSON geometry references an arc outside the arcs array"))?;
        for offset in 0..arc.len() {
            let point = if reversed {
                &arc[arc.len() - 1 - offset]
            } else {
                &arc[offset]
            };
            if offset == 0
                && let Some(last) = positions.last()
            {
                if last != point {
                    return Err(invalid(
                        "TopoJSON arcs used in one path do not share a matching endpoint",
                    ));
                }
                continue;
            }
            add_expanded_positions(budget, 1)?;
            positions.push(*point);
        }
    }
    Ok(positions)
}

fn add_expanded_positions(budget: &mut TopologyBudget, amount: usize) -> Result<()> {
    budget.expanded_positions = budget.expanded_positions.saturating_add(amount);
    if budget.expanded_positions > MAX_TOPOJSON_EXPANDED_POSITIONS {
        return Err(Error::LimitExceeded(format!(
            "TopoJSON expands to more than {MAX_TOPOJSON_EXPANDED_POSITIONS} positions"
        )));
    }
    Ok(())
}

fn parse_ordinate(value: &Value, arc_index: usize) -> Result<f64> {
    value
        .as_f64()
        .filter(|ordinate| ordinate.is_finite())
        .ok_or_else(|| {
            invalid(format!(
                "TopoJSON coordinate in arc/point {arc_index} is not finite numeric data"
            ))
        })
}

fn parse_quantized(value: &Value, arc_index: usize) -> Result<f64> {
    let integer = value
        .as_i64()
        .map(|value| value as f64)
        .or_else(|| value.as_u64().map(|value| value as f64))
        .ok_or_else(|| {
            invalid(format!(
                "quantized TopoJSON arc {arc_index} coordinates must be integers"
            ))
        })?;
    if integer.is_finite() {
        Ok(integer)
    } else {
        Err(invalid(format!(
            "quantized TopoJSON arc {arc_index} coordinate is out of range"
        )))
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidInput(format!("invalid TopoJSON: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_topology_prefix() {
        assert!(looks_like_topojson_prefix(
            br#"{"type":"Topology","objects":{},"arcs":[]}"#
        ));
        assert!(!looks_like_topojson_prefix(br#"{"type":"Feature"}"#));
    }

    #[test]
    fn decodes_delta_arcs_and_inverse_arc_indexes() {
        let transform = Some(Transform {
            scale: [0.01, 0.01],
            translate: [139.0, 35.0],
        });
        let arcs = json!([
            [[0, 0], [100, 0], [0, 100]],
            [[100, 0], [0, 100]],
            [[100, 100], [-100, 0]],
            [[0, 100], [0, -100]],
            [[100, 0], [100, 0]],
            [[200, 0], [0, 100]],
            [[200, 100], [-100, 0]]
        ]);
        let mut budget = TopologyBudget::default();
        let arcs = parse_arcs(Some(&arcs), transform, &mut budget).unwrap();
        assert_eq!(arcs[0], vec![[139.0, 35.0], [140.0, 35.0], [140.0, 36.0]]);
        let ring = join_arcs(
            &json!([4, 5, 6, -2]).as_array().unwrap().clone(),
            &arcs,
            &mut budget,
        )
        .unwrap();
        assert_eq!(ring.first(), Some(&[140.0, 35.0]));
        assert_eq!(ring.last(), Some(&[140.0, 35.0]));
        assert_eq!(ring.len(), 5);
    }

    #[test]
    fn rejects_unjoined_arcs_and_out_of_range_references() {
        let arcs = vec![vec![[0.0, 0.0], [1.0, 0.0]], vec![[2.0, 0.0], [2.0, 1.0]]];
        assert!(join_arcs(&[json!(0), json!(1)], &arcs, &mut TopologyBudget::default()).is_err());
        assert!(join_arcs(&[json!(-3)], &arcs, &mut TopologyBudget::default()).is_err());
    }

    #[test]
    fn decodes_multi_geometries_and_nested_geometry_collections() {
        let arcs = vec![
            vec![[0.0, 0.0], [1.0, 0.0]],
            vec![[1.0, 0.0], [1.0, 1.0], [0.0, 0.0]],
        ];
        let topology = json!({
            "type": "GeometryCollection",
            "geometries": [
                { "type": "MultiPoint", "coordinates": [[0.2, 0.2], [0.8, 0.8]] },
                { "type": "MultiLineString", "arcs": [[0], [1]] },
                { "type": "MultiPolygon", "arcs": [[[0, 1]]] }
            ]
        });
        let decoded =
            decode_geometry(&topology, &arcs, None, 0, &mut TopologyBudget::default()).unwrap();
        assert_eq!(decoded["type"], "GeometryCollection");
        let geometries = decoded["geometries"].as_array().unwrap();
        assert_eq!(geometries[0]["type"], "MultiPoint");
        assert_eq!(geometries[1]["type"], "MultiLineString");
        assert_eq!(geometries[2]["type"], "MultiPolygon");
        assert_eq!(
            geometries[2]["coordinates"][0][0].as_array().unwrap().len(),
            4
        );
    }
}
